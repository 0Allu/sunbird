//! A "Plumage" row in Discord's Settings that opens plumage's own pages.
//!
//! Phoenix builds its UI as a stream of UI-op batches from Rust to Java. This
//! module rewrites those batches to splice in nodes built from Phoenix's own
//! layouts, and answers view events on them. Injected nodes use ids from
//! `0x5000_0000`, far above the counter Rust allocates from.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   thread,
};

use memchr::memmem;
use plumage_ui::{
   ChildEdit,
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
   motion::{
      self,
      Spring,
      Worklet,
   },
   templates,
};
use plumage_wire::{
   DispatchEvent,
   Value,
   decode_dispatch,
   without_frames,
};

use crate::{
   Chosen,
   Control,
   PluginInfo,
   bridge::{
      self,
      Action,
      Sink,
   },
};

const FIRST_ID: i32 = 0x5000_0000;
/// The host drops motion slots at or above 65536, and Rust counts its own up
/// from 0, so plumage takes them downward from the top.
const SLOT_END: i32 = 1 << 16;
const LOGOUT_ROW: &str = "settings.row.logout";
const ICON_PLUGIN: &str = "res://ic_mana_puzzle_piece_plus";
const ICON_RELOAD: &str = "res://ic_mana_puzzle_piece_retry";
const ICON_UNINSTALL: &str = "res://ic_mana_puzzle_piece_minus";
const ICON_CONSOLE: &str = "res://ic_mana_angle_brackets";
/// Ids Phoenix's captured page layout uses for the parts plumage fills in.
const PAGE_HEADER_SPACER: u32 = 0x4E1;
const PAGE_CONTENT: u32 = 0x4E4;
const PAGE_HEADER: u32 = 0x562;
const PAGE_BACK: u32 = 0x565;
const PAGE_TITLE: u32 = 0x56A;

const RADII: [Prop; 5] = [
   Prop::RADIUS,
   Prop::RADIUS_TOP_LEFT,
   Prop::RADIUS_TOP_RIGHT,
   Prop::RADIUS_BOTTOM_LEFT,
   Prop::RADIUS_BOTTOM_RIGHT,
];

/// Plugins are named by their index in [`crate::plugins`] as of the last
/// render, which every install follows with a fresh one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Press {
   OpenMain,
   OpenConsole,
   OpenPlugin(usize),
   Toggle(usize),
   Reload(usize),
   Uninstall(usize),
   /// A plugin's switch or button, by its index in [`PluginInfo::settings`].
   Setting(usize, usize),
   Back,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageKind {
   Main,
   Console,
   Plugin(usize),
}

/// What the UI thread hands to the worker. UI ops must not be sent from the
/// main thread, where Rust may hold the UI lock while it waits on Java.
enum Job {
   Press(Press),
   NavigateTo(usize),
   /// A text field's contents after an edit.
   Draft(Field, String),
   Submit(Field),
   Log,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Field {
   Eval,
   Install,
   /// A text or number setting, indexed like [`Press::Setting`].
   Setting(usize, usize),
}

/// Nodes updated in place, so typing is not interrupted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
   Input(Field),
   Log,
}

struct Tree {
   id:      i32,
   kind:    ViewKind,
   props:   Props,
   kids:    Vec<Self>,
   src:     Option<u32>,
   press:   Option<Press>,
   slot:    Option<Slot>,
   animate: Option<Animate>,
}

/// A node the host animates between two prop sets, like a switch's knob.
struct Animate {
   worklet: &'static Worklet,
   /// The props of the other state.
   other:   Props,
   /// Set when the node starts at `other` and springs to its own props.
   flipped: bool,
}

impl Tree {
   fn from_template(node: &Node) -> Self {
      Self {
         id:      0,
         kind:    node.kind.clone(),
         props:   node.props.clone(),
         kids:    node.kids.iter().map(Self::from_template).collect(),
         src:     node.src,
         press:   None,
         slot:    None,
         animate: None,
      }
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }

   fn find(&mut self, src: u32) -> Option<&mut Self> {
      if self.src == Some(src) {
         return Some(self);
      }
      self.kids.iter_mut().find_map(|kid| kid.find(src))
   }

   fn slots(&self) -> Vec<(i32, Slot)> {
      let mut slots = self
         .slot
         .map(|slot| (self.id, slot))
         .into_iter()
         .collect::<Vec<_>>();
      slots.extend(self.kids.iter().flat_map(Self::slots));
      slots
   }

   /// Marks the animated nodes under the one `press` targets as just flipped.
   fn flip(&mut self, press: Press, inside: bool) {
      let inside = inside || (self.press == Some(press) && self.animate.is_some());
      if inside && let Some(animate) = &mut self.animate {
         animate.flipped = true;
      }
      for kid in &mut self.kids {
         kid.flip(press, inside);
      }
   }

   fn ids(&self) -> Vec<i32> {
      let mut ids = vec![self.id];
      ids.extend(self.kids.iter().flat_map(Self::ids));
      ids
   }
}

fn set_prop(props: &mut Props, key: Prop, value: Option<Value>) {
   let at = props.iter().position(|(existing, _)| *existing == key);
   match (at, value) {
      (Some(index), Some(value)) => props[index].1 = value,
      (Some(index), None) => {
         props.remove(index);
      },
      (None, Some(value)) => props.push((key, value)),
      (None, None) => {},
   }
}

fn text(value: &str) -> Value {
   Value::Str(value.to_owned())
}

/// Motion slots for animated nodes, reused once a node is deleted.
struct SlotPool {
   next:  i32,
   free:  Vec<(i32, i32)>,
   owned: HashMap<i32, (i32, i32)>,
}

impl SlotPool {
   /// The first of `count` slots for `node`, and whether they were used
   /// before.
   fn take(&mut self, node: i32, count: i32) -> (i32, bool) {
      let (first, reused) = match self.free.iter().position(|&(_, size)| size == count) {
         Some(at) => (self.free.swap_remove(at).0, true),
         None => {
            self.next -= count;
            (self.next, false)
         },
      };
      self.owned.insert(node, (first, count));
      (first, reused)
   }

   fn release(&mut self, node: i32) {
      if let Some(block) = self.owned.remove(&node) {
         self.free.push(block);
      }
   }
}

/// The "Plumage" row in the Settings list.
struct Entry {
   root: i32,
   ids:  Vec<i32>,
}

struct Page {
   root:     i32,
   ids:      Vec<i32>,
   kind:     PageKind,
   content:  i32,
   sections: Vec<i32>,
   slots:    Vec<(i32, Slot)>,
}

/// The open Settings screen and what plumage added to it.
struct Screen {
   list:       i32,
   /// Rust's own rows in the list and pages on the navigator, kept current
   /// from its batches.
   rust_rows:  Option<Vec<i32>>,
   search:     Option<i32>,
   navigator:  Option<i32>,
   rust_pages: Option<Vec<i32>>,
   header:     Option<Props>,
   entry:      Option<Entry>,
   stack:      Vec<Page>,
}

struct Settings {
   next_id:    i32,
   slots:      SlotPool,
   /// Motion words for nodes built since the last batch went out. They run
   /// after the batch attaches those nodes.
   motion:     Vec<i32>,
   navigators: HashSet<i32>,
   screen:     Option<Screen>,
   handlers:   HashMap<i32, Press>,
   plugins:    Vec<PluginInfo>,
   drafts:     HashMap<Field, String>,
}

static SETTINGS: LazyLock<Mutex<Settings>> = LazyLock::new(|| {
   Mutex::new(Settings {
      next_id:    FIRST_ID,
      slots:      SlotPool {
         next:  SLOT_END,
         free:  Vec::new(),
         owned: HashMap::new(),
      },
      motion:     Vec::new(),
      navigators: HashSet::new(),
      screen:     None,
      handlers:   HashMap::new(),
      plugins:    Vec::new(),
      drafts:     HashMap::new(),
   })
});

static WORKER: OnceLock<Sender<Job>> = OnceLock::new();
/// Set while a [`Job::Log`] is queued, so a burst of lines costs one update.
static LOG_QUEUED: AtomicBool = AtomicBool::new(false);

fn settings() -> MutexGuard<'static, Settings> {
   SETTINGS.lock().unwrap_or_else(PoisonError::into_inner)
}

struct SettingsSink;

impl Sink for SettingsSink {
   fn ops(&self, batch: &[u8]) -> Action {
      rewrite_ops(batch)
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      filter_dispatch(frame)
   }
}

/// Hooks the UI bridge and starts the worker that answers presses.
pub fn start() {
   let (sender, receiver) = mpsc::channel::<Job>();
   if WORKER.set(sender).is_err() {
      log::warn!("settings: already started");
      return;
   }
   bridge::install(Box::new(SettingsSink));
   thread::spawn(move || {
      for job in receiver {
         act(&job);
         let plugins = matches!(job, Job::Press(_) | Job::Submit(_)).then(crate::plugins);
         let ops = {
            let mut state = settings();
            if let Some(plugins) = plugins {
               state.plugins = plugins;
            }
            let mut ops = state.run(job);
            if !state.motion.is_empty() {
               ops.push(Op::Motion {
                  words: std::mem::take(&mut state.motion),
               });
            }
            ops
         };
         if !ops.is_empty() && !bridge::apply_ops(&encode(&ops)) {
            log::warn!("settings: the UI update did not apply");
         }
      }
   });
}

/// Makes the runtime calls `job` asks for. They take the engine lock, so they
/// run before the settings lock is taken.
fn act(job: &Job) {
   let plugin = |index: usize| settings().plugins.get(index).cloned();
   match *job {
      Job::Log => LOG_QUEUED.store(false, Ordering::Relaxed),
      Job::Submit(field) => {
         let Some(draft) = settings().drafts.remove(&field) else {
            return;
         };
         let input = draft.trim();
         match field {
            Field::Eval if !input.is_empty() => crate::evaluate(input),
            Field::Install if !input.is_empty() => {
               match crate::install(input) {
                  Ok(name) => log::info!("installed {name}"),
                  Err(err) => log::error!("install failed: {err}"),
               }
            },
            Field::Setting(index, at) => {
               let Some(plugin) = plugin(index) else {
                  return;
               };
               let Some(setting) = plugin.settings.get(at) else {
                  return;
               };
               let value = match setting.control {
                  Control::Text(_) => Chosen::Text(draft.clone()),
                  Control::Number(_) => {
                     let Ok(number) = input.parse::<f64>() else {
                        log::error!(
                           "[{}] {}: {input:?} is not a number",
                           plugin.name,
                           setting.key
                        );
                        return;
                     };
                     Chosen::Number(number)
                  },
                  Control::Switch(_) | Control::Button => return,
               };
               crate::set_setting(&plugin.name, &setting.key, value);
            },
            Field::Eval | Field::Install => {},
         }
      },
      Job::Press(Press::Toggle(index)) => {
         if let Some(plugin) = plugin(index) {
            crate::set_enabled(&plugin.name, !plugin.enabled);
         }
      },
      Job::Press(Press::Reload(index)) => {
         if let Some(plugin) = plugin(index) {
            crate::reload(&plugin.name);
         }
      },
      Job::Press(Press::Uninstall(index)) => {
         if let Some(plugin) = plugin(index) {
            match crate::uninstall(&plugin.name) {
               Ok(()) => log::info!("uninstalled {}", plugin.name),
               Err(err) => log::error!("uninstall failed: {err}"),
            }
         }
      },
      Job::Press(Press::Setting(index, at)) => {
         let Some(plugin) = plugin(index) else {
            return;
         };
         match plugin
            .settings
            .get(at)
            .map(|setting| (&setting.key, &setting.control))
         {
            Some((key, Control::Switch(on))) => {
               crate::set_setting(&plugin.name, key, Chosen::Switch(!on));
            },
            Some((key, Control::Button)) => crate::press(&plugin.name, key),
            _ => {},
         }
      },
      Job::Press(_) | Job::NavigateTo(_) | Job::Draft(..) => {},
   }
}

/// Refreshes the console page's log, if one is open.
pub fn log_changed() {
   if LOG_QUEUED.swap(true, Ordering::Relaxed) {
      return;
   }
   if let Some(worker) = WORKER.get()
      && worker.send(Job::Log).is_err()
   {
      LOG_QUEUED.store(false, Ordering::Relaxed);
   }
}

/// Splices plumage's nodes into a Rust-to-Java UI batch.
fn rewrite_ops(batch: &[u8]) -> Action {
   // Most batches carry neither the Settings list nor a navigator, so they
   // pass without being decoded.
   let relevant = settings().screen.is_some()
      || memmem::find(batch, LOGOUT_ROW.as_bytes()).is_some()
      || memmem::find(batch, ViewKind::Navigator.as_str().as_bytes()).is_some();
   if !relevant {
      return Action::Pass;
   }
   let ops = match decode(batch) {
      Ok(ops) => ops,
      Err(err) => {
         log::warn!("settings: undecodable batch: {err}");
         return Action::Pass;
      },
   };
   settings()
      .rewrite(ops)
      .map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
}

/// Takes view events on plumage's nodes out of a Java-to-Rust dispatch.
fn filter_dispatch(frame: &[u8]) -> Action {
   let Ok(items) = decode_dispatch(frame) else {
      return Action::Pass;
   };
   let mut taken = Vec::new();
   let mut jobs = Vec::new();
   {
      let state = settings();
      let Some(screen) = &state.screen else {
         return Action::Pass;
      };
      for item in &items {
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            continue;
         };
         let job = if *event == Prop::NAVIGATE.key()
            && !screen.stack.is_empty()
            && Some(*node) == screen.navigator
         {
            let index = payload
               .field(Prop::INDEX.key())
               .and_then(Value::as_i32)
               .and_then(|index| usize::try_from(index).ok());
            Some(Job::NavigateTo(
               index.unwrap_or_else(|| state.page_index().saturating_sub(1)),
            ))
         } else if let Some(field) = state.field(*node) {
            if *event == Prop::CHANGE_TEXT.key() {
               text_of(payload).map(|text| Job::Draft(field, text))
            } else {
               (*event == Prop::SUBMIT.key()).then_some(Job::Submit(field))
            }
         } else if let Some(press) = state.handlers.get(node) {
            (*event == Prop::PRESS.key()).then_some(Job::Press(*press))
         } else {
            continue;
         };
         taken.push(item.offset);
         jobs.extend(job);
      }
   }

   if let Some(worker) = WORKER.get() {
      for job in jobs {
         if worker.send(job).is_err() {
            log::warn!("settings: the worker stopped");
            break;
         }
      }
   }
   match (taken.len(), frame.first()) {
      (0, _) => Action::Pass,
      (count, _) if count == items.len() => Action::Drop,
      (_, Some(4)) => Action::Replace(without_frames(frame, &taken)),
      _ => Action::Drop,
   }
}

/// A `changeText` payload is the text itself or an object holding it.
fn text_of(payload: &Value) -> Option<String> {
   match payload {
      Value::Str(text) => Some(text.clone()),
      Value::Obj(_) => {
         [Prop::TEXT, Prop::VALUE]
            .into_iter()
            .find_map(|prop| payload.field(prop.key())?.as_str().map(str::to_owned))
      },
      _ => {
         log::warn!("settings: unexpected changeText payload {payload:?}");
         None
      },
   }
}

impl Settings {
   fn page_index(&self) -> usize {
      self.screen.as_ref().map_or(0, |screen| {
         (screen.rust_pages.as_ref().map_or(0, Vec::len) + screen.stack.len()).saturating_sub(1)
      })
   }

   fn build(&mut self, tree: &mut Tree, inherited: Option<Press>, ops: &mut Vec<Op>) -> i32 {
      let press = tree.press.or(inherited);
      let kids = tree
         .kids
         .iter_mut()
         .map(|kid| self.build(kid, press, ops))
         .collect::<Vec<i32>>();
      tree.id = self.next_id;
      self.next_id += 1;
      if let Some(press) = press {
         self.handlers.insert(tree.id, press);
      }
      let mut props = tree.props.clone();
      let motion = tree.animate.as_ref().map(|animate| {
         let (first, reused) = self.slots.take(tree.id, animate.worklet.slot_count());
         let bound = animate.worklet.bind(first);
         props.push(bound.prop());
         let start = if animate.flipped {
            &animate.other
         } else {
            &tree.props
         };
         let mut commands = bound.rest(start);
         if reused {
            commands.extend(bound.pin(start, Spring::SWITCH));
         }
         if animate.flipped {
            commands.extend(bound.spring(start, &tree.props, Spring::SWITCH));
         }
         commands
      });
      ops.push(Op::Create {
         id: tree.id,
         kind: tree.kind.clone(),
         props,
      });
      if let Some(commands) = motion {
         self.motion.extend(motion::words(&commands));
      }
      if !kids.is_empty() {
         ops.push(Op::Children {
            id:  tree.id,
            ids: kids,
         });
      }
      tree.id
   }

   fn forget(&mut self, ids: &[i32]) {
      for id in ids {
         self.handlers.remove(id);
         self.slots.release(*id);
      }
   }

   fn navigator_ops(&self) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      let Some(navigator) = screen.navigator else {
         return Vec::new();
      };
      let mut ids = screen.rust_pages.clone().unwrap_or_default();
      ids.extend(screen.stack.iter().map(|page| page.root));
      vec![Op::Children { id: navigator, ids }, Op::Update {
         id:    navigator,
         props: vec![(
            Prop::INDEX,
            Value::I32(i32::try_from(self.page_index()).unwrap_or(i32::MAX)),
         )],
      }]
   }

   fn slots(&self) -> impl Iterator<Item = (i32, Slot)> + '_ {
      self
         .screen
         .iter()
         .flat_map(|screen| &screen.stack)
         .flat_map(|page| page.slots.iter().copied())
   }

   fn field(&self, node: i32) -> Option<Field> {
      self.slots().find_map(|(id, slot)| {
         match slot {
            Slot::Input(field) if id == node => Some(field),
            _ => None,
         }
      })
   }

   fn log_ops(&self) -> Vec<Op> {
      let lines = log_text();
      self
         .slots()
         .filter(|(_, slot)| *slot == Slot::Log)
         .map(|(id, _)| {
            Op::Update {
               id,
               props: vec![(Prop::TEXT, text(&lines))],
            }
         })
         .collect()
   }

   fn run(&mut self, job: Job) -> Vec<Op> {
      match job {
         Job::Draft(field, draft) => {
            self.drafts.insert(field, draft);
            Vec::new()
         },
         Job::Log => self.log_ops(),
         Job::Submit(Field::Eval) => {
            let mut ops = self.log_ops();
            ops.extend(
               self
                  .slots()
                  .filter(|(_, slot)| *slot == Slot::Input(Field::Eval))
                  .map(|(id, _)| {
                     Op::Update {
                        id,
                        props: vec![(Prop::VALUE, text(""))],
                     }
                  }),
            );
            ops
         },
         Job::Press(Press::OpenMain) => self.open_page(PageKind::Main),
         Job::Press(Press::OpenConsole) => self.open_page(PageKind::Console),
         Job::Press(Press::OpenPlugin(index)) => self.open_page(PageKind::Plugin(index)),
         Job::Press(press @ (Press::Toggle(_) | Press::Setting(..))) => {
            self.rerender_all(Some(press))
         },
         Job::Submit(Field::Install | Field::Setting(..)) | Job::Press(Press::Reload(_)) => {
            self.rerender_all(None)
         },
         Job::Press(Press::Back) => self.close_top(),
         Job::Press(Press::Uninstall(_)) => {
            let mut ops = self.close_top();
            ops.extend(self.rerender_all(None));
            ops
         },
         Job::NavigateTo(index) => {
            let mut ops = Vec::new();
            while self
               .screen
               .as_ref()
               .is_some_and(|screen| !screen.stack.is_empty())
               && self.page_index() > index
            {
               ops.extend(self.close_top());
            }
            ops
         },
      }
   }

   fn render(&self, kind: PageKind) -> (String, Vec<Tree>) {
      match kind {
         PageKind::Main => ("Plumage".to_owned(), main_sections(&self.plugins)),
         PageKind::Console => ("Console".to_owned(), console_sections()),
         PageKind::Plugin(index) => {
            self.plugins.get(index).map_or_else(
               || ("Plugin".to_owned(), Vec::new()),
               |plugin| (plugin.name.clone(), plugin_sections(index, plugin)),
            )
         },
      }
   }

   fn build_sections(&mut self, sections: &mut [Tree], ops: &mut Vec<Op>) -> (Vec<i32>, Vec<i32>) {
      let roots = sections
         .iter_mut()
         .map(|section| self.build(section, None, ops))
         .collect();
      let ids = sections.iter().flat_map(Tree::ids).collect();
      (roots, ids)
   }

   /// Rebuilds every open page. The switches `flipped` targets spring from
   /// their old state.
   fn rerender_all(&mut self, flipped: Option<Press>) -> Vec<Op> {
      let kinds = self.screen.as_ref().map_or_default(|screen| {
         screen
            .stack
            .iter()
            .map(|page| page.kind)
            .collect::<Vec<PageKind>>()
      });
      let mut ops = Vec::new();
      for (position, kind) in kinds.into_iter().enumerate() {
         if kind == PageKind::Console {
            continue;
         }
         let (_, mut sections) = self.render(kind);
         if let Some(press) = flipped {
            for section in &mut sections {
               section.flip(press, false);
            }
         }
         let (roots, ids) = self.build_sections(&mut sections, &mut ops);
         let Some(page) = self
            .screen
            .as_mut()
            .and_then(|screen| screen.stack.get_mut(position))
         else {
            continue;
         };
         page.slots = sections.iter().flat_map(Tree::slots).collect();
         let old = std::mem::replace(&mut page.sections, ids.clone());
         page.ids.retain(|id| !old.contains(id));
         page.ids.extend(ids);
         ops.push(Op::Children {
            id:  page.content,
            ids: roots,
         });
         ops.push(Op::Delete { ids: old.clone() });
         self.forget(&old);
      }
      ops
   }

   fn open_page(&mut self, kind: PageKind) -> Vec<Op> {
      let Some(screen) = &self.screen else {
         return Vec::new();
      };
      if screen.rust_pages.is_none() {
         log::warn!("settings: no navigator pages known yet");
         return Vec::new();
      }
      let header = screen.header.clone();
      let (title, sections) = self.render(kind);
      let title = title.as_str();

      let mut page = Tree::from_template(&templates().page);
      page.set(Prop::PANE_TITLE, Some(text(title)));
      if let Some(title_node) = page.find(PAGE_TITLE) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      if let Some(header) = header {
         let value = |key: Prop| {
            header
               .iter()
               .find(|(existing, _)| *existing == key)
               .map(|(_, value)| value.clone())
         };
         let height = value(Prop::HEIGHT);
         if let Some(node) = page.find(PAGE_HEADER) {
            for key in [Prop::PADDING, Prop::HEIGHT] {
               if let Some(found) = value(key) {
                  node.set(key, Some(found));
               }
            }
         }
         if let (Some(node), Some(height)) = (page.find(PAGE_HEADER_SPACER), height) {
            node.set(Prop::HEIGHT, Some(height));
         }
      }
      if let Some(back) = page.find(PAGE_BACK) {
         back.set(Prop::ACCESSIBILITY_LABEL, Some(text("Back")));
         back.press = Some(Press::Back);
      }
      if let Some(content) = page.find(PAGE_CONTENT) {
         content.kids = sections;
      }

      let mut ops = Vec::new();
      let root = self.build(&mut page, None, &mut ops);
      let ids = page.ids();
      let slots = page.slots();
      let Some(content) = page.find(PAGE_CONTENT) else {
         log::warn!("settings: the page layout has no content node");
         return Vec::new();
      };
      let sections = content.kids.iter().flat_map(Tree::ids).collect();
      let content = content.id;
      if let Some(screen) = &mut self.screen {
         screen.stack.push(Page {
            root,
            ids,
            kind,
            content,
            sections,
            slots,
         });
      }
      ops.extend(self.navigator_ops());
      ops
   }

   fn close_top(&mut self) -> Vec<Op> {
      let Some(page) = self.screen.as_mut().and_then(|screen| screen.stack.pop()) else {
         return Vec::new();
      };
      self.forget(&page.ids);
      let mut ops = self.navigator_ops();
      ops.push(Op::Delete { ids: page.ids });
      ops
   }

   fn entry_ops(&mut self) -> (Entry, Vec<Op>) {
      let row = button_row("Plumage", ICON_PLUGIN, Some(Press::OpenMain), true);
      let mut section = section("", vec![row]);
      let mut ops = Vec::new();
      let root = self.build(&mut section, None, &mut ops);
      (
         Entry {
            root,
            ids: section.ids(),
         },
         ops,
      )
   }

   fn rewrite(&mut self, ops: Vec<Op>) -> Option<Vec<Op>> {
      let mut creates = HashMap::new();
      let mut parents = HashMap::new();
      for op in &ops {
         match op {
            Op::Create { id, kind, props } => {
               if *kind == ViewKind::Navigator {
                  self.navigators.insert(*id);
               }
               creates.insert(*id, (kind, props));
            },
            Op::Children { id, ids } => {
               for kid in ids {
                  parents.insert(*kid, *id);
               }
            },
            _ => {},
         }
      }
      self.find_screen(&creates, &parents);
      self.screen.as_ref()?;
      let stacks = creates
         .iter()
         .filter(|(_, (kind, _))| **kind == ViewKind::Stack)
         .map(|(id, _)| *id)
         .collect::<HashSet<i32>>();
      drop(creates);

      let mut changed = false;
      let mut out = Vec::with_capacity(ops.len());
      let mut rest = ops.into_iter();
      while let Some(op) = rest.next() {
         let screen = self.screen.as_ref().expect("checked above");
         let (list, navigator) = (screen.list, screen.navigator);
         match op {
            Op::Children { id, ids } if id == list => {
               let search = ids.first().copied().filter(|first| stacks.contains(first));
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::ChildEdits { id, edits } if id == list && screen.rust_rows.is_some() => {
               let ids = apply_edits(screen.rust_rows.as_deref().unwrap_or_default(), &edits);
               let search = screen.search;
               self.list_changed(ids, search, &mut out);
               changed = true;
            },
            Op::Children { id, ids } if Some(id) == navigator => {
               let original = Op::Children {
                  id,
                  ids: ids.clone(),
               };
               changed |= self.navigator_changed(original, ids, &mut out);
            },
            Op::ChildEdits { id, edits }
               if Some(id) == navigator && screen.rust_pages.is_some() =>
            {
               let ids = apply_edits(screen.rust_pages.as_deref().unwrap_or_default(), &edits);
               changed |= self.navigator_changed(Op::ChildEdits { id, edits }, ids, &mut out);
            },
            Op::Delete { mut ids }
               if ids.contains(&list)
                  || navigator.is_some_and(|navigator| ids.contains(&navigator)) =>
            {
               // Java's delete does not recurse, so plumage's nodes are listed
               // here.
               let screen = self.screen.take().expect("checked above");
               let ours = screen
                  .entry
                  .into_iter()
                  .flat_map(|entry| entry.ids)
                  .chain(screen.stack.into_iter().flat_map(|page| page.ids))
                  .collect::<Vec<i32>>();
               self.forget(&ours);
               ids.extend(ours);
               out.push(Op::Delete { ids });
               out.extend(rest.by_ref());
               changed = true;
            },
            other => out.push(other),
         }
      }
      changed.then_some(out)
   }

   fn find_screen(
      &mut self,
      creates: &HashMap<i32, (&ViewKind, &Props)>,
      parents: &HashMap<i32, i32>,
   ) {
      for (id, (_, props)) in creates {
         let is_logout = props.iter().any(|(key, value)| {
            *key == Prop::ACCESSIBILITY_ID
               && matches!(value, Value::Str(found) if found == LOGOUT_ROW)
         });
         if !is_logout {
            continue;
         }
         let Some(list) = parents
            .get(id)
            .and_then(|row| parents.get(row))
            .and_then(|group| parents.get(group))
            .copied()
         else {
            continue;
         };
         if self
            .screen
            .as_ref()
            .is_some_and(|screen| screen.list == list)
         {
            continue;
         }

         let presented = |node: &i32| {
            creates.get(node).is_some_and(|(_, props)| {
               props
                  .iter()
                  .any(|(key, _)| *key == Prop::PRESENTATION || *key == Prop::UIKIT_PUSH)
            })
         };
         let mut navigator = None;
         let mut cursor = Some(list);
         while let Some(node) = cursor {
            if self.navigators.contains(&node) {
               navigator = Some(node);
               break;
            }
            if presented(&node) {
               navigator = parents.get(&node).copied();
               break;
            }
            cursor = parents.get(&node).copied();
         }
         let header = creates
            .values()
            .find(|(_, props)| props.iter().any(|(key, _)| *key == Prop::STACK_HEADER))
            .map(|(_, props)| (*props).clone());
         self.screen = Some(Screen {
            list,
            rust_rows: None,
            search: None,
            navigator,
            rust_pages: None,
            header,
            entry: None,
            stack: Vec::new(),
         });
      }
   }

   fn list_changed(&mut self, ids: Vec<i32>, search: Option<i32>, out: &mut Vec<Op>) {
      if self
         .screen
         .as_ref()
         .is_some_and(|screen| screen.entry.is_none())
      {
         let (entry, ops) = self.entry_ops();
         out.extend(ops);
         if let Some(screen) = &mut self.screen {
            screen.entry = Some(entry);
         }
      }
      let Some(screen) = &mut self.screen else {
         return;
      };
      if screen.search.is_none() {
         screen.search = search;
      }
      let at = usize::from(screen.search.is_some() && ids.first() == screen.search.as_ref());
      let mut with_entry = ids.clone();
      if let Some(entry) = &screen.entry {
         with_entry.insert(at, entry.root);
      }
      screen.rust_rows = Some(ids);
      out.push(Op::Children {
         id:  screen.list,
         ids: with_entry,
      });
   }

   /// Records Rust's pages on the navigator. While plumage has pages open,
   /// they stay on top of Rust's.
   fn navigator_changed(&mut self, original: Op, ids: Vec<i32>, out: &mut Vec<Op>) -> bool {
      let Some(screen) = &mut self.screen else {
         out.push(original);
         return false;
      };
      let (Some(navigator), false) = (screen.navigator, screen.stack.is_empty()) else {
         screen.rust_pages = Some(ids);
         out.push(original);
         return false;
      };
      let mut all = ids.clone();
      all.extend(screen.stack.iter().map(|page| page.root));
      screen.rust_pages = Some(ids);
      out.push(Op::Children {
         id:  navigator,
         ids: all,
      });
      true
   }
}

fn apply_edits(old: &[i32], edits: &[ChildEdit]) -> Vec<i32> {
   let mut out = Vec::new();
   let mut cursor = 0;
   for edit in edits {
      let keep = usize::try_from(edit.keep).unwrap_or(0);
      let del = usize::try_from(edit.del).unwrap_or(0);
      out.extend(old.iter().skip(cursor).take(keep));
      cursor += keep + del;
      out.extend(&edit.ins);
   }
   out.extend(old.iter().skip(cursor));
   out
}

fn is_radius(key: Prop) -> bool {
   RADII.contains(&key)
}

fn section(title: &str, rows: Vec<Tree>) -> Tree {
   let layouts = templates();
   let mut section = Tree::from_template(&layouts.section);
   let mut group = Tree::from_template(&layouts.group);
   group.kids = rows
      .into_iter()
      .enumerate()
      .flat_map(|(index, row)| {
         let divider = (index > 0).then(|| Tree::from_template(&layouts.divider));
         divider.into_iter().chain([row])
      })
      .collect();
   if !title.is_empty() {
      let mut header = Tree::from_template(&layouts.header);
      header.set(Prop::TEXT, Some(text(title)));
      section.kids.push(header);
   }
   section.kids.push(group);
   section
}

/// `root > row > [icon, text, chevron]`
fn button_row(title: &str, icon: &str, press: Option<Press>, chevron: bool) -> Tree {
   let mut root = Tree::from_template(&templates().button_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(title)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("plumage-{title}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(icon_node) = row.kids.get_mut(0) {
         icon_node.set(Prop::URI, Some(text(icon)));
      }
      if let Some(label) = row.kids.get_mut(1) {
         label.set(Prop::TEXT, Some(text(title)));
      }
      if !chevron {
         row.kids.truncate(2);
      }
   }
   if press.is_none() {
      root.set(Prop::ON, None);
      root.set(Prop::ROLE, None);
   }
   root.press = press;
   root
}

/// `root > row > [label, value wrap > value, chevron]`
fn value_row(label: &str, value: &str) -> Tree {
   let mut root = Tree::from_template(&templates().value_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   root.set(Prop::ON, None);
   root.set(Prop::ROLE, None);
   root.set(
      Prop::ACCESSIBILITY_LABEL,
      Some(text(&format!("{label}, {value}"))),
   );
   if let Some(row) = root.kids.first_mut() {
      if let Some(label_node) = row.kids.get_mut(0) {
         label_node.set(Prop::TEXT, Some(text(label)));
      }
      if let Some(value_node) = row
         .kids
         .get_mut(1)
         .and_then(|wrap| wrap.kids.get_mut(0))
         .and_then(|inner| inner.kids.get_mut(0))
      {
         value_node.set(Prop::TEXT, Some(text(value)));
      }
      row.kids.truncate(2);
   }
   root
}

/// `root > row > [icon, column > [title, subtitle], wrap > track > knob]`.
/// Pressing the row runs `press`, pressing the switch runs `toggle`.
fn switch_row(
   icon: Option<&str>,
   title: &str,
   subtitle: Option<&str>,
   on: bool,
   press: Press,
   toggle: Press,
) -> Tree {
   let layouts = templates();
   let mut root = Tree::from_template(&layouts.switch_row);
   root.props.retain(|(key, _)| !is_radius(*key));
   let label = subtitle.map_or_else(
      || title.to_owned(),
      |subtitle| format!("{title}, {subtitle}"),
   );
   root.set(Prop::ACCESSIBILITY_LABEL, Some(text(&label)));
   root.set(Prop::ACCESSIBILITY_CHECKED, Some(Value::Bool(on)));
   root.set(
      Prop::ACCESSIBILITY_ID,
      Some(text(&format!("plumage-{title}"))),
   );
   root.press = Some(press);
   let Some(row) = root.kids.first_mut() else {
      return root;
   };
   if let Some(icon_node) = row.kids.get_mut(0) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(column) = row.kids.get_mut(1) {
      if let Some(title_node) = column.kids.get_mut(0) {
         title_node.set(Prop::TEXT, Some(text(title)));
      }
      match subtitle {
         Some(subtitle) => {
            if let Some(subtitle_node) = column.kids.get_mut(1) {
               subtitle_node.set(Prop::TEXT, Some(text(subtitle)));
            }
         },
         None => column.kids.truncate(1),
      }
   }
   if let Some(track) = row.kids.get_mut(2).and_then(|wrap| wrap.kids.get_mut(0)) {
      track.press = Some(toggle);
      animate(track, &layouts.track_worklet, &layouts.track_off, on);
      if let Some(knob) = track.kids.get_mut(0) {
         animate(knob, &layouts.knob_worklet, &layouts.knob_off, on);
      }
   }
   if icon.is_none() {
      row.kids.remove(0);
   }
   root
}

/// Gives a switch part built from the "on" template the props for `on` and a
/// worklet to spring between both states.
fn animate(node: &mut Tree, worklet: &'static Worklet, off: &Props, on: bool) {
   let other = if on {
      off.clone()
   } else {
      std::mem::replace(&mut node.props, off.clone())
   };
   node.animate = Some(Animate {
      worklet,
      other,
      flipped: false,
   });
}

fn main_sections(plugins: &[PluginInfo]) -> Vec<Tree> {
   let rows = if plugins.is_empty() {
      vec![button_row("No plugins found", ICON_PLUGIN, None, false)]
   } else {
      plugins
         .iter()
         .enumerate()
         .map(|(index, plugin)| {
            let subtitle = plugin
               .error
               .as_deref()
               .map(|error| format!("Failed: {error}"))
               .or_else(|| plugin.header.description.clone());
            switch_row(
               Some(ICON_PLUGIN),
               &plugin.name,
               subtitle.as_deref(),
               plugin.enabled,
               Press::OpenPlugin(index),
               Press::Toggle(index),
            )
         })
         .collect()
   };
   let enabled = plugins.iter().filter(|plugin| plugin.enabled).count();
   vec![
      section("Plugins", rows),
      section("Install from URL", vec![text_field(
         Field::Install,
         Some(ICON_PLUGIN),
         "https://example.com/plugin.js",
         "",
         false,
      )]),
      section("", vec![button_row(
         "Console",
         ICON_CONSOLE,
         Some(Press::OpenConsole),
         true,
      )]),
      section("About", vec![
         value_row("Runtime", "rquickjs"),
         value_row("Hooks", "registry pointer swap"),
         value_row("Enabled", &format!("{enabled} of {}", plugins.len())),
      ]),
   ]
}

fn log_text() -> String {
   let lines = crate::console::lines();
   if lines.is_empty() {
      return "Nothing logged yet".to_owned();
   }
   lines.join("\n")
}

/// `root > frame > row > [icon wrap > icon, input]`, Phoenix's search box.
fn text_field(
   field: Field,
   icon: Option<&str>,
   placeholder: &str,
   value: &str,
   number: bool,
) -> Tree {
   let mut root = Tree::from_template(&templates().input);
   let Some(row) = root
      .kids
      .first_mut()
      .and_then(|frame| frame.kids.first_mut())
   else {
      return root;
   };
   if let Some(icon_node) = row.kids.first_mut().and_then(|wrap| wrap.kids.first_mut()) {
      icon_node.set(Prop::URI, icon.map(text));
   }
   if let Some(input) = row.kids.get_mut(1) {
      input.set(Prop::PLACEHOLDER, Some(text(placeholder)));
      input.set(Prop::VALUE, Some(text(value)));
      input.set(Prop::KEYBOARD, number.then(|| text("number")));
      input.set(Prop::RETURN_KEY, Some(text("go")));
      input.set(Prop::ROLE, None);
      input.set(
         Prop::ACCESSIBILITY_ID,
         Some(text(&format!("plumage-{placeholder}"))),
      );
      input.slot = Some(Slot::Input(field));
   }
   if icon.is_none() {
      row.kids.remove(0);
      if let Some(input) = row.kids.first_mut() {
         input.set(Prop::MARGIN, None);
      }
   }
   root
}

fn console_sections() -> Vec<Tree> {
   let input_row = text_field(
      Field::Eval,
      Some(ICON_CONSOLE),
      "Evaluate JavaScript",
      "",
      false,
   );
   let mut log = button_row(&log_text(), ICON_CONSOLE, None, false);
   if let Some(row) = log.kids.first_mut() {
      row.kids.remove(0);
      if let Some(label) = row.kids.first_mut() {
         label.slot = Some(Slot::Log);
      }
   }
   vec![section("", vec![input_row]), section("Log", vec![log])]
}

fn plugin_sections(index: usize, plugin: &PluginInfo) -> Vec<Tree> {
   let subtitle = plugin
      .error
      .as_deref()
      .map(|error| format!("Failed: {error}"));
   let status = match (&plugin.error, plugin.enabled) {
      (Some(_), _) => "failed",
      (None, true) => "running",
      (None, false) => "off",
   };
   let mut rows = Vec::new();
   let mut fields = Vec::new();
   for (at, setting) in plugin.settings.iter().enumerate() {
      let press = Press::Setting(index, at);
      let description = setting.description.as_deref();
      match &setting.control {
         Control::Switch(on) => {
            rows.push(switch_row(
               None,
               &setting.title,
               description,
               *on,
               press,
               press,
            ));
         },
         Control::Button => {
            let mut row = button_row(&setting.title, ICON_PLUGIN, Some(press), false);
            if let Some(inner) = row.kids.first_mut() {
               inner.kids.remove(0);
            }
            rows.push(row);
         },
         Control::Text(current) => {
            let field = text_field(
               Field::Setting(index, at),
               None,
               description.unwrap_or(""),
               current,
               false,
            );
            fields.push(section(&setting.title, vec![field]));
         },
         Control::Number(number) => {
            let field = text_field(
               Field::Setting(index, at),
               None,
               description.unwrap_or(""),
               &number.to_string(),
               true,
            );
            fields.push(section(&setting.title, vec![field]));
         },
      }
   }
   let mut sections = vec![section("", vec![switch_row(
      Some(ICON_PLUGIN),
      "Enabled",
      subtitle.as_deref(),
      plugin.enabled,
      Press::Toggle(index),
      Press::Toggle(index),
   )])];
   if !rows.is_empty() {
      sections.push(section("Settings", rows));
   }
   sections.extend(fields);
   let mut actions = vec![button_row(
      "Reload",
      ICON_RELOAD,
      Some(Press::Reload(index)),
      false,
   )];
   if plugin.installed {
      actions.push(button_row(
         "Uninstall",
         ICON_UNINSTALL,
         Some(Press::Uninstall(index)),
         false,
      ));
   }
   sections.push(section("", actions));
   let mut info = vec![value_row("Name", &plugin.name)];
   for (label, value) in [
      ("Version", &plugin.header.version),
      ("Author", &plugin.header.author),
   ] {
      if let Some(value) = value {
         info.push(value_row(label, value));
      }
   }
   info.push(value_row("Status", status));
   sections.push(section("Info", info));
   sections
}
