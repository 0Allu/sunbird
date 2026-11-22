//! Plugin UI spliced into Phoenix's screens: buttons in the chat header and
//! the message box, lines under messages, rows in the message long-press
//! menu, profile sections, and slash commands in the `/` suggestions.
//!
//! - Rust's batches are mirrored (children, accessibility ids), and plugin
//!   nodes go next to stable accessibility ids, cloned from Phoenix's own
//!   neighbours.
//! - No plugin code runs on the threads that carry UI batches or view events.
//!   Rust's store thread can hold the engine while it emits a batch, and the
//!   main thread can be waited on with the UI lock held. Plugin callbacks run
//!   on a worker, and their results reach the screen as follow-up batches.

mod commands;
mod mirror;
pub mod targets;

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   fmt,
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicU64,
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
   Node,
   Op,
   Prop,
   Props,
   ViewKind,
   decode,
   encode,
   set_prop,
};
use plumage_wire::{
   DispatchEvent,
   Key,
   Value,
   decode_dispatch,
   encode_batch,
   encode_view_event,
};

use crate::{
   bridge::{
      self,
      Action,
      Sink,
   },
   patches::{
      mirror::{
         Mirror,
         prop,
      },
      targets::{
         Seen,
         Target,
         Templates,
      },
   },
   store,
};

/// Plugin node ids count up from here, clear of Rust's and the settings page's.
const FIRST_ID: i32 = 0x5800_0000;
const DISMISS: Key = Prop::named("dismiss").key();
const ROW_LONG_PRESS: Key = Prop::named("rowLongPress").key();
const RESULTS_CAP: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ContributionId(u64);

impl ContributionId {
   pub fn next() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(1);
      Self(NEXT.fetch_add(1, Ordering::Relaxed))
   }
}

impl fmt::Display for ContributionId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self.0.fmt(formatter)
   }
}

/// Something a plugin added to a [`Target`]. Its callbacks stay in the
/// engine.
pub struct Contribution {
   pub id:          ContributionId,
   pub plugin:      String,
   pub target:      Target,
   pub label:       String,
   pub icon:        Option<String>,
   pub danger:      bool,
   pub priority:    i32,
   /// Shown only where the plugin's `visible` says so.
   pub conditional: bool,
}

/// A slash command, as the `/` suggestions list it.
#[derive(Clone)]
pub struct Command {
   pub name:        String,
   pub description: String,
   pub icon:        Option<String>,
   pub plugin:      String,
}

/// What a contribution's nodes are about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Place {
   Screen,
   Message(String),
   User(Option<String>),
}

#[derive(Clone)]
pub struct Rendered {
   pub text:  String,
   pub color: Option<i64>,
}

/// A plugin's answer for one place: the text it renders, or whether its row
/// shows.
#[derive(Clone)]
pub enum Outcome {
   Text(Option<Rendered>),
   Shown(bool),
}

/// What plugins made of a message about to be sent.
pub enum Sent {
   Cancel,
   Text(String),
   Unchanged,
}

/// What plugin callbacks receive as `ctx`.
#[derive(Default)]
pub struct UiContext {
   pub channel_id: Option<String>,
   pub message_id: Option<String>,
   pub user_id:    Option<String>,
   /// The message as Phoenix cached it, JSON.
   pub message:    Option<String>,
}

#[derive(Clone)]
enum Handler {
   Press {
      contribution: ContributionId,
      place:        Place,
      dismiss:      bool,
   },
   /// A command row in the `/` suggestions.
   Pick(String),
}

/// A node to create, with the handler its subtree answers presses with.
pub struct Draft {
   kind:  ViewKind,
   props: Props,
   kids:  Vec<Self>,
   press: Option<Handler>,
}

impl Draft {
   fn from_node(node: &Node) -> Self {
      Self {
         kind:  node.kind.clone(),
         props: node.props.clone(),
         kids:  node.kids.iter().map(Self::from_node).collect(),
         press: None,
      }
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
   Target(Target),
   Suggestions,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Before {
   Start,
   Node(i32),
   End,
}

/// Plugin nodes living under one of Phoenix's parents.
struct Slot {
   kind:   SlotKind,
   parent: i32,
   before: Before,
   place:  Place,
   roots:  Vec<i32>,
   all:    Vec<i32>,
}

enum Job {
   Refresh(Target),
   Evaluate(ContributionId, Place),
   Evaluated(ContributionId, Place, Outcome),
   Press(ContributionId, Place),
   Suggest,
   Pick {
      input: i32,
      name:  String,
   },
   Submit {
      input: i32,
      text:  String,
      frame: Vec<u8>,
   },
}

#[derive(Default)]
struct Patches {
   mirror:        Mirror,
   templates:     Templates,
   seen:          Seen,
   contributions: Vec<Contribution>,
   commands:      Vec<Command>,
   interceptors:  usize,
   slots:         Vec<Slot>,
   anchors:       HashMap<Target, HashSet<i32>>,
   handlers:      HashMap<i32, Handler>,
   results:       HashMap<(ContributionId, Place), Outcome>,
   pending:       HashSet<(ContributionId, Place)>,
   next_id:       i32,
   /// The channel last seen open.
   channel:       Option<String>,
   suggestions:   commands::Suggestions,
   /// Sends plugins already saw, let through once when Rust gets them back.
   resent:        Vec<Vec<u8>>,
}

static PATCHES: LazyLock<Mutex<Patches>> = LazyLock::new(|| {
   Mutex::new(Patches {
      next_id: FIRST_ID,
      ..Patches::default()
   })
});
/// Set once a plugin adds anything, so builds without UI plugins never
/// decode a batch.
static ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKER: OnceLock<Sender<Job>> = OnceLock::new();

fn patches() -> MutexGuard<'static, Patches> {
   PATCHES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn queue(job: Job) {
   if let Some(worker) = WORKER.get()
      && worker.send(job).is_err()
   {
      log::warn!("patches: the worker is gone");
   }
}

struct PatchSink;

impl Sink for PatchSink {
   fn ops(&self, batch: &[u8]) -> Action {
      if !ACTIVE.load(Ordering::Acquire) {
         return Action::Pass;
      }
      let ops = match decode(batch) {
         Ok(ops) => ops,
         Err(err) => {
            log::warn!("patches: undecodable batch: {err}");
            return Action::Pass;
         },
      };
      patches()
         .rewrite(ops)
         .map_or(Action::Pass, |ops| Action::Replace(encode(&ops)))
   }

   fn dispatch(&self, frame: &[u8]) -> Action {
      if !ACTIVE.load(Ordering::Acquire) {
         return Action::Pass;
      }
      patches().filter(frame)
   }
}

/// Hooks the UI bridge and starts the worker that runs plugin callbacks.
pub fn start() {
   let (sender, receiver) = mpsc::channel::<Job>();
   if WORKER.set(sender).is_err() {
      log::warn!("patches: already started");
      return;
   }
   bridge::install(Box::new(PatchSink));
   // The chat header and message box are built once, before the first
   // gateway event would attach the bridge.
   bridge::attach();
   thread::spawn(move || {
      for job in receiver {
         let ops = run(job);
         if !ops.is_empty() && !bridge::apply_ops(&encode(&ops)) {
            log::warn!("patches: the UI update did not apply");
         }
      }
   });
}

/// Runs `job`'s plugin calls, then returns the ops it leaves for the screen.
/// The engine and the patch state are never held together.
fn run(job: Job) -> Vec<Op> {
   match job {
      Job::Refresh(target) => patches().refresh(target),
      Job::Evaluate(contribution, place) => {
         let context = UiContext {
            channel_id: patches().channel.clone(),
            message_id: match &place {
               Place::Message(id) => Some(id.clone()),
               _ => None,
            },
            user_id:    match &place {
               Place::User(id) => id.clone(),
               _ => None,
            },
            message:    None,
         };
         let outcome =
            crate::engine().and_then(|engine| engine.ui_evaluate(contribution, &place, &context));
         outcome.map_or_else(Vec::new, |outcome| {
            patches().evaluated(contribution, place, outcome)
         })
      },
      Job::Evaluated(contribution, place, outcome) => {
         patches().evaluated(contribution, place, outcome)
      },
      Job::Press(contribution, place) => {
         let context = press_context(&place);
         if let Some(engine) = crate::engine() {
            engine.ui_press(contribution, &context);
         }
         Vec::new()
      },
      Job::Suggest => {
         let mut out = Vec::new();
         patches().suggest(&mut out);
         out
      },
      Job::Pick { input, name } => {
         let mut state = patches();
         let mut out = vec![commands::force_text(input, &format!("/{name} "))];
         state.mirror.set_composer_text(input, format!("/{name} "));
         state.suggest(&mut out);
         out
      },
      Job::Submit { input, text, frame } => commands::submit(input, &text, &frame),
   }
}

/// `ctx` for a press: the open channel, and the message when there is one.
/// It reads Phoenix's cache, so it runs on the worker.
fn press_context(place: &Place) -> UiContext {
   let database = crate::STATE
      .get()
      .and_then(|state| store::database(&state.files_dir()).ok());
   let rows = |sql: &str, args: &[String]| {
      let database = database.as_ref()?;
      store::query(database, sql, args)
         .inspect_err(|err| log::warn!("patches: {sql}: {err}"))
         .ok()
   };
   // Phoenix keeps its JSON values as BLOBs, which come back as byte arrays.
   let value = |rows: &[serde_json::Map<String, serde_json::Value>], key: &str| {
      match rows.first()?.get(key)? {
         serde_json::Value::String(text) => Some(text.clone()),
         serde_json::Value::Array(bytes) => {
            let bytes = bytes
               .iter()
               .map(|byte| u8::try_from(byte.as_u64()?).ok())
               .collect::<Option<Vec<u8>>>()?;
            String::from_utf8(bytes).ok()
         },
         _ => None,
      }
   };

   let message = |id: &str| {
      let found = rows("SELECT k, v FROM messages WHERE k LIKE ? LIMIT 1", &[
         format!("%:{id:0>20}"),
      ])
      .unwrap_or_default();
      let channel = value(&found, "k")
         .and_then(|key| Some(key.split(':').next()?.trim_start_matches('0').to_owned()));
      (channel, value(&found, "v"))
   };

   let mut context = UiContext::default();
   if let Place::Message(id) = place {
      (context.channel_id, context.message) = message(id);
      context.message_id = Some(id.clone());
   }
   if let Place::User(id) = place {
      context.user_id.clone_from(id);
   }
   if context.channel_id.is_none() {
      let selected =
         rows("SELECT v FROM meta WHERE k = ?", &["selected".to_owned()]).unwrap_or_default();
      context.channel_id = value(&selected, "v")
         .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
         .and_then(|selected| {
            match selected.get("channel_id")? {
               serde_json::Value::String(id) => Some(id.clone()),
               serde_json::Value::Number(id) => Some(id.to_string()),
               _ => None,
            }
         });
   }
   if context.channel_id.is_none() {
      let newest = patches().newest_message();
      context.channel_id = newest.and_then(|id| message(&id).0);
   }
   if context.channel_id.is_some() {
      patches().channel.clone_from(&context.channel_id);
   }
   context
}

/// Adds `contribution` and draws it wherever its target is on screen.
pub fn add(contribution: Contribution) {
   ACTIVE.store(true, Ordering::Release);
   let target = contribution.target;
   let mut state = patches();
   state.contributions.push(contribution);
   state
      .contributions
      .sort_by_key(|contribution| std::cmp::Reverse(contribution.priority));
   drop(state);
   queue(Job::Refresh(target));
}

pub fn remove(id: ContributionId) {
   let mut state = patches();
   let Some(at) = state
      .contributions
      .iter()
      .position(|contribution| contribution.id == id)
   else {
      return;
   };
   let target = state.contributions.remove(at).target;
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// Draws `id` again, asking its plugin anew.
pub fn update(id: ContributionId) {
   let mut state = patches();
   let Some(target) = state
      .contributions
      .iter()
      .find(|contribution| contribution.id == id)
      .map(|contribution| contribution.target)
   else {
      return;
   };
   state
      .results
      .retain(|(contribution, _), _| *contribution != id);
   drop(state);
   queue(Job::Refresh(target));
}

/// A plugin settled a `render` or `visible` it returned as a promise.
pub fn evaluated(contribution: ContributionId, place: Place, outcome: Outcome) {
   queue(Job::Evaluated(contribution, place, outcome));
}

pub fn add_command(command: Command) {
   ACTIVE.store(true, Ordering::Release);
   let mut state = patches();
   state
      .commands
      .retain(|existing| existing.name != command.name);
   state.commands.push(command);
   drop(state);
   queue(Job::Suggest);
}

pub fn remove_command(name: &str) {
   patches().commands.retain(|command| command.name != name);
   queue(Job::Suggest);
}

pub fn commands() -> Vec<Command> {
   patches().commands.clone()
}

/// How many `onSend` interceptors plugins hold, so sends are taken only
/// while one exists.
pub fn set_interceptors(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().interceptors = count;
}

/// Drops everything `plugin` added.
pub fn forget(plugin: &str) {
   let mut state = patches();
   let targets = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.target)
      .collect::<HashSet<_>>();
   let gone = state
      .contributions
      .iter()
      .filter(|contribution| contribution.plugin == plugin)
      .map(|contribution| contribution.id)
      .collect::<HashSet<_>>();
   state
      .contributions
      .retain(|contribution| contribution.plugin != plugin);
   state
      .results
      .retain(|(contribution, _), _| !gone.contains(contribution));
   let had_commands = state
      .commands
      .iter()
      .any(|command| command.plugin == plugin);
   state.commands.retain(|command| command.plugin != plugin);
   drop(state);
   for target in targets {
      queue(Job::Refresh(target));
   }
   if had_commands {
      queue(Job::Suggest);
   }
}

impl Patches {
   /// Mirrors `ops` and splices plugin nodes in. `None` leaves the batch as
   /// it was.
   fn rewrite(&mut self, ops: Vec<Op>) -> Option<Vec<Op>> {
      let mut mine = Vec::new();
      let mut touched = HashSet::new();
      let mut extra_deletes;
      {
         let (created, deleted) = self.mirror.apply(&ops);
         extra_deletes = self.drop_deleted(&deleted);

         let mut named = created
            .keys()
            .copied()
            .filter(|id| self.mirror.accessibility_id(*id).is_some())
            .collect::<Vec<_>>();
         named.sort_unstable();
         for id in &named {
            let aid = self
               .mirror
               .accessibility_id(*id)
               .unwrap_or_default()
               .to_owned();
            self.templates.learn(&self.mirror, *id, &aid, &created);
            if aid == "suggestions" {
               self.suggestions.scroll = Some(*id);
            }
            if aid == "profile-tab-pages" {
               self.seen.profile_user = profile_user(&ops, *id);
            }
         }
         if self.suggestions.scroll.is_some() {
            self.learn_suggestion_row(&created);
         }
         for id in named {
            let aid = self
               .mirror
               .accessibility_id(id)
               .unwrap_or_default()
               .to_owned();
            for target in Target::ALL {
               if !target.matches(&aid) {
                  continue;
               }
               self.anchors.entry(target).or_default().insert(id);
               if let Some(parent) = self.place(target, id, &mut mine) {
                  touched.insert(parent);
               }
            }
         }
      }

      let composer_slash = self
         .mirror
         .composer()
         .is_some_and(|(_, text)| text.starts_with('/'));
      let mut extra = Vec::new();
      if !self.commands.is_empty()
         && self.suggestions.scroll.is_some()
         && (self.has_suggestions() || composer_slash)
      {
         self.suggest(&mut extra);
      }

      let slot_parents = self
         .slots
         .iter()
         .map(|slot| slot.parent)
         .collect::<HashSet<_>>();
      let rewrites_children = ops.iter().any(|op| {
         matches!(op, Op::Children { id, .. } | Op::ChildEdits { id, .. } if slot_parents.contains(id))
      });
      if touched.is_empty() && extra_deletes.is_empty() && extra.is_empty() && !rewrites_children {
         return None;
      }

      let mut out = mine;
      for op in ops {
         match op {
            Op::Children { id, .. } | Op::ChildEdits { id, .. } if slot_parents.contains(&id) => {
               out.push(Op::Children {
                  id,
                  ids: self.merged(id),
               });
               touched.remove(&id);
            },
            Op::Delete { mut ids } if !extra_deletes.is_empty() => {
               ids.append(&mut extra_deletes);
               out.push(Op::Delete { ids });
            },
            op => out.push(op),
         }
      }
      for parent in touched {
         out.push(Op::Children {
            id:  parent,
            ids: self.merged(parent),
         });
      }
      out.extend(extra);
      Some(out)
   }

   /// Forgets the slots and anchors under `deleted` nodes and returns the
   /// plugin nodes that go with them.
   fn drop_deleted(&mut self, deleted: &[i32]) -> Vec<i32> {
      let mut orphans = Vec::new();
      for id in deleted {
         let (gone, kept) = std::mem::take(&mut self.slots)
            .into_iter()
            .partition::<Vec<_>, _>(|slot| slot.parent == *id);
         self.slots = kept;
         for slot in gone {
            self.forget_slot(&slot);
            orphans.extend(slot.all);
         }
         for anchors in self.anchors.values_mut() {
            anchors.remove(id);
         }
         if self.suggestions.scroll == Some(*id) {
            orphans.extend(self.drop_suggestions());
         }
      }
      orphans
   }

   /// Takes presses on plugin nodes, long presses, and sends out of a
   /// Java-to-Rust dispatch.
   fn filter(&mut self, frame: &[u8]) -> Action {
      let Ok(items) = decode_dispatch(frame) else {
         return Action::Pass;
      };
      let mut changed = false;
      let mut frames = Vec::new();
      for item in &items {
         let raw = frame[item.offset..item.offset + item.len].to_vec();
         if let Some(at) = self.resent.iter().position(|sent| *sent == raw) {
            self.resent.swap_remove(at);
            frames.push(raw);
            continue;
         }
         let DispatchEvent::View {
            node,
            event,
            payload,
         } = &item.event
         else {
            frames.push(raw);
            continue;
         };
         if *event == ROW_LONG_PRESS
            && let Some(index) = payload.field(Prop::INDEX.key()).and_then(Value::as_i32)
         {
            self.seen.long_pressed = self.row_message(*node, index);
         }
         if *event == Prop::CHANGE_TEXT.key() && self.mirror.is_composer(*node) {
            let text = payload
               .field(Prop::TEXT.key())
               .and_then(Value::as_str)
               .unwrap_or_default()
               .to_owned();
            self.mirror.set_composer_text(*node, text);
            if !self.commands.is_empty() {
               queue(Job::Suggest);
            }
         }
         if *node >= FIRST_ID
            && let Some(handler) = self.handlers.get(node).cloned()
         {
            changed = true;
            if *event == Prop::PRESS.key() {
               self.pressed(*node, handler, &mut frames);
            }
            continue;
         }
         if let Some(input) = self.sending(*node, *event)
            && let Some((_, text)) = self.mirror.composer()
         {
            queue(Job::Submit {
               input,
               text: text.to_owned(),
               frame: raw,
            });
            changed = true;
            continue;
         }
         frames.push(raw);
      }
      if !changed {
         return Action::Pass;
      }
      match frames.len() {
         0 => Action::Drop,
         1 => Action::Replace(frames.remove(0)),
         _ => Action::Replace(encode_batch(&frames)),
      }
   }

   fn pressed(&self, node: i32, handler: Handler, frames: &mut Vec<Vec<u8>>) {
      match handler {
         Handler::Press {
            contribution,
            place,
            dismiss,
         } => {
            queue(Job::Press(contribution, place));
            let slot = self.slots.iter().find(|slot| slot.all.contains(&node));
            if dismiss && let Some(sheet) = slot.and_then(|slot| self.mirror.sheet_of(slot.parent))
            {
               frames.push(encode_view_event(sheet, DISMISS, &Value::Obj(Vec::new())));
            }
         },
         Handler::Pick(name) => {
            if let Some((input, _)) = self.mirror.composer() {
               frames.push(commands::change_text(input, &format!("/{name} ")));
               queue(Job::Pick { input, name });
            }
         },
      }
   }

   /// The composer input when `event` on `node` sends a message and a plugin
   /// wants to see it first.
   fn sending(&self, node: i32, event: Key) -> Option<i32> {
      if self.commands.is_empty() && self.interceptors == 0 {
         return None;
      }
      let (input, _) = self.mirror.composer()?;
      let submit = event == Prop::SUBMIT.key() && self.mirror.is_composer(node);
      let send = event == Prop::PRESS.key() && self.mirror.accessibility_id(node) == Some("send");
      (submit || send).then_some(input)
   }

   /// The newest message on screen.
   fn newest_message(&self) -> Option<String> {
      self
         .anchors
         .get(&Target::Content)?
         .iter()
         .filter_map(|id| message_id(self.mirror.accessibility_id(*id)?))
         .max_by_key(|id| (id.len(), *id))
         .map(str::to_owned)
   }

   /// The message id in row `index` of list `node`.
   fn row_message(&self, node: i32, index: i32) -> Option<String> {
      let row = self.mirror.row(node, index)?;
      let hit = self
         .mirror
         .find_down(row, &|aid| message_id(aid).is_some(), 8)?;
      message_id(self.mirror.accessibility_id(hit)?).map(str::to_owned)
   }

   /// Opens `target`'s slot for the anchor `id` and draws into it. Returns
   /// the parent when it opened a new slot.
   fn place(&mut self, target: Target, id: i32, out: &mut Vec<Op>) -> Option<i32> {
      if !self
         .contributions
         .iter()
         .any(|contribution| contribution.target == target)
      {
         return None;
      }
      let aid = self.mirror.accessibility_id(id)?.to_owned();
      let placement = target.place(&self.mirror, id, &aid, &self.seen)?;
      let before = placement.before.map_or(Before::End, Before::Node);
      if let Some(slot) = self
         .slots
         .iter_mut()
         .find(|slot| slot.parent == placement.parent && slot.kind == SlotKind::Target(target))
      {
         slot.before = before;
         return None;
      }
      self.slots.push(Slot {
         kind: SlotKind::Target(target),
         parent: placement.parent,
         before,
         place: placement.place,
         roots: Vec::new(),
         all: Vec::new(),
      });
      let index = self.slots.len() - 1;
      self.fill(index, out);
      Some(placement.parent)
   }

   /// Draws slot `index` anew into `out`. The caller deletes what it held.
   fn fill(&mut self, index: usize, out: &mut Vec<Op>) {
      let SlotKind::Target(target) = self.slots[index].kind else {
         return;
      };
      let place = self.slots[index].place.clone();
      let contributions = self
         .contributions
         .iter()
         .filter(|contribution| contribution.target == target)
         .collect::<Vec<_>>();
      for contribution in &contributions {
         let key = (contribution.id, place.clone());
         if (target.evaluates() || contribution.conditional)
            && !self.results.contains_key(&key)
            && self.pending.insert(key)
         {
            queue(Job::Evaluate(contribution.id, place.clone()));
         }
      }
      let drafts = self
         .templates
         .draw(target, &contributions, &place, &|contribution| {
            self.results.get(&(contribution.id, place.clone())).cloned()
         });
      let mut roots = Vec::new();
      let mut all = Vec::new();
      for draft in drafts {
         roots.push(self.emit(draft, None, out, &mut all));
      }
      self.slots[index].roots = roots;
      self.slots[index].all = all;
   }

   fn emit(
      &mut self,
      draft: Draft,
      inherited: Option<&Handler>,
      out: &mut Vec<Op>,
      all: &mut Vec<i32>,
   ) -> i32 {
      let handler = draft.press.or_else(|| inherited.cloned());
      let ids = draft
         .kids
         .into_iter()
         .map(|kid| self.emit(kid, handler.as_ref(), out, all))
         .collect::<Vec<_>>();
      let id = self.next_id;
      self.next_id += 1;
      if let Some(handler) = handler {
         self.handlers.insert(id, handler);
      }
      out.push(Op::Create {
         id,
         kind: draft.kind,
         props: draft.props,
      });
      if !ids.is_empty() {
         out.push(Op::Children { id, ids });
      }
      all.push(id);
      id
   }

   /// `parent`'s children as Rust has them, with the slots spliced in.
   fn merged(&self, parent: i32) -> Vec<i32> {
      let mut list = self.mirror.kids(parent).to_vec();
      for slot in self.slots.iter().filter(|slot| slot.parent == parent) {
         let at = match slot.before {
            Before::Start => 0,
            Before::Node(node) => list.iter().position(|id| *id == node).unwrap_or(list.len()),
            Before::End => list.len(),
         };
         list.splice(at..at, slot.roots.iter().copied());
      }
      list
   }

   fn forget_slot(&mut self, slot: &Slot) {
      for id in &slot.all {
         self.handlers.remove(id);
      }
   }

   /// Draws every slot of `target` anew, and opens slots at anchors that had
   /// none.
   fn refresh(&mut self, target: Target) -> Vec<Op> {
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         if self.slots[index].kind != SlotKind::Target(target) {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         for id in &old {
            self.handlers.remove(id);
         }
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      let anchors = self
         .anchors
         .get(&target)
         .map_or_else(Vec::new, |anchors| anchors.iter().copied().collect());
      for anchor in anchors {
         if let Some(parent) = self.place(target, anchor, &mut out) {
            touched.insert(parent);
         }
      }
      self.finish(out, touched, deletes)
   }

   /// Records a plugin's answer and redraws the slots it was for.
   fn evaluated(
      &mut self,
      contribution: ContributionId,
      place: Place,
      outcome: Outcome,
   ) -> Vec<Op> {
      let key = (contribution, place);
      self.pending.remove(&key);
      let Some(target) = self
         .contributions
         .iter()
         .find(|known| known.id == contribution)
         .map(|known| known.target)
      else {
         return Vec::new();
      };
      if self.results.len() >= RESULTS_CAP {
         self.results.clear();
      }
      let visible = !matches!(outcome, Outcome::Text(None) | Outcome::Shown(false));
      let place = key.1.clone();
      self.results.insert(key, outcome);
      if !visible {
         return Vec::new();
      }
      let mut out = Vec::new();
      let mut deletes = Vec::new();
      let mut touched = HashSet::new();
      for index in 0..self.slots.len() {
         let slot = &self.slots[index];
         if slot.kind != SlotKind::Target(target) || slot.place != place {
            continue;
         }
         let old = std::mem::take(&mut self.slots[index].all);
         for id in &old {
            self.handlers.remove(id);
         }
         deletes.extend(old);
         self.fill(index, &mut out);
         touched.insert(self.slots[index].parent);
      }
      self.finish(out, touched, deletes)
   }

   fn finish(&self, mut out: Vec<Op>, touched: HashSet<i32>, deletes: Vec<i32>) -> Vec<Op> {
      for parent in touched {
         if self.mirror.has_kids(parent) {
            out.push(Op::Children {
               id:  parent,
               ids: self.merged(parent),
            });
         }
      }
      if !deletes.is_empty() {
         out.push(Op::Delete { ids: deletes });
      }
      out
   }
}

/// The user whose avatar a profile batch shows before its pages node.
fn profile_user(ops: &[Op], pages: i32) -> Option<String> {
   ops.iter().find_map(|op| {
      let Op::Create { id, props, .. } = op else {
         return None;
      };
      if *id > pages {
         return None;
      }
      let Some(Value::Str(uri)) = prop(props, Prop::URI) else {
         return None;
      };
      let rest = &uri[memmem::find(uri.as_bytes(), b"/avatars/")? + "/avatars/".len()..];
      let id = rest.split('/').next()?;
      id.bytes()
         .all(|byte| byte.is_ascii_digit())
         .then(|| id.to_owned())
   })
}

/// The message id in `message.{content,avatar,author,timestamp}:<id>`.
fn message_id(aid: &str) -> Option<&str> {
   let (kind, id) = aid.strip_prefix("message.")?.split_once(':')?;
   (matches!(kind, "content" | "avatar" | "author" | "timestamp")
      && !id.is_empty()
      && id.bytes().all(|byte| byte.is_ascii_digit()))
   .then_some(id)
}
