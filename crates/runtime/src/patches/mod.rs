//! Plugin UI spliced into Phoenix's screens: buttons in the chat header and
//! the message box, lines under messages, rows in the message long-press
//! menu, profile sections and tabs, and slash commands in the `/` suggestions.
//!
//! - Rust's batches are mirrored (children, accessibility ids), and plugin
//!   nodes go next to stable accessibility ids, cloned from Phoenix's own
//!   neighbours.
//! - No plugin code runs on the threads that carry UI batches or view events.
//!   Rust's store thread can hold the engine while it emits a batch, and the
//!   main thread can be waited on with the UI lock held. Plugin callbacks run
//!   on a worker, and their results reach the screen as follow-up batches.

mod banners;
mod commands;
mod emoji;
mod events;
mod hidden;
mod menu;
mod mount;
mod nodes;
mod pages;
mod profile;
mod props;
mod sink;
mod slots;
mod tabs;
pub mod targets;
mod tints;
mod worker;

use std::{
   collections::{
      HashMap,
      HashSet,
      VecDeque,
   },
   fmt,
   sync::{
      LazyLock,
      Mutex,
      MutexGuard,
      PoisonError,
      atomic::{
         AtomicBool,
         AtomicI32,
         AtomicU32,
         AtomicU64,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   thread,
   time::{
      Duration,
      Instant,
   },
};

use phoenix_ui::{
   Node,
   Prop,
   Props,
   ViewKind,
   encode,
   mirror::Mirror,
   set_prop,
};
use phoenix_wire::{
   Key,
   Value,
   encode_view_event,
};

use crate::{
   bridge,
   patches::{
      sink::PatchSink,
      targets::{
         Seen,
         Target,
         Templates,
      },
      worker::{
         Job,
         WORKER,
         queue,
         run,
      },
   },
};

/// Plugin node ids count up from here, clear of Rust's and the settings page's.
const FIRST_ID: i32 = 0x5800_0000;
const FORCE_VALUE: Prop = Prop::named("forceValue");
const RESULTS_CAP: usize = 4096;
const PAGE: Prop = Prop::named("page");

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
   pub tint:        Option<i64>,
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
   /// Under a message's content: the color of its author and text (32-bit
   /// ARGB).
   pub tint:  Option<i64>,
}

/// A plugin's answer for one place: the text it renders, or whether its row
/// shows.
#[derive(Clone)]
pub enum Outcome {
   Text(Option<Rendered>),
   Shown(bool),
   /// A profile tab's page.
   Page(Vec<Block>),
}

/// A piece of a profile tab's page. `handler` indexes the callbacks the
/// engine kept for the page.
#[derive(Clone)]
pub enum Block {
   Text {
      title: Option<String>,
      text:  String,
      color: Option<i64>,
   },
   Entry {
      avatar:   Option<String>,
      title:    String,
      subtitle: Option<String>,
      text:     String,
      /// Small images after the title, like badges.
      badges:   Vec<String>,
      /// Images under the text.
      images:   Vec<String>,
      /// A row under the text. Actions without a handler are plain text.
      actions:  Vec<BlockAction>,
      /// A column at the entry's end, like votes.
      side:     Vec<BlockAction>,
      /// Runs on a long press of the entry.
      hold:     Option<u32>,
   },
   /// Buttons side by side.
   Buttons(Vec<BlockAction>),
   Input {
      placeholder: String,
      handler:     u32,
      /// A send button, which submits like the keyboard's send key.
      send:        bool,
   },
   Button {
      label:   String,
      danger:  bool,
      handler: u32,
   },
}

#[derive(Clone)]
pub struct BlockAction {
   pub label:   String,
   pub danger:  bool,
   /// 32-bit ARGB, over `danger`.
   pub color:   Option<i64>,
   pub handler: Option<u32>,
}

/// What plugins made of a message about to be sent.
pub enum Sent {
   Cancel,
   Text(String),
   Unchanged,
}

/// Matches a Phoenix node's `accessibilityId`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeMatch {
   Exact(String),
   Prefix(String),
}

impl NodeMatch {
   pub fn matches(&self, aid: &str) -> bool {
      match self {
         Self::Exact(expected) => aid == expected,
         Self::Prefix(prefix) => aid.starts_with(prefix.as_str()),
      }
   }
}

/// Which node a declarative [`PropPatch`] targets when a matching node mounts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PatchTarget {
   /// The matching node itself.
   Node,
   /// The enclosing sheet (and its created outer wrapper), for zero-flash
   /// suppression.
   Sheet,
   /// The enclosing virtualized/row-gesture list.
   List,
}

/// Snapshot of a live Phoenix node from [`Mirror`], exposed to plugins.
#[derive(Clone, Debug)]
pub struct NodeInfo {
   pub id:               i32,
   pub kind:             Option<String>,
   pub accessibility_id: Option<String>,
   pub parent:           Option<i32>,
   pub children:         Vec<i32>,
   pub sheet:            Option<i32>,
   pub list:             Option<i32>,
   pub on:               Vec<String>,
}

/// A synchronous Rust-side prop patch applied inside `rewrite` with zero JS
/// overhead per batch.
struct PropPatch {
   id:       u64,
   plugin:   String,
   matcher:  NodeMatch,
   target:   PatchTarget,
   props:    Props,
   add_on:   Vec<String>,
   once:     bool,
   deadline: Option<Instant>,
}

struct EventHookMeta {
   id:        u64,
   plugin:    String,
   event:     Key,
   intercept: bool,
}

struct NodeWatcher {
   id:      u64,
   plugin:  String,
   matcher: NodeMatch,
}

/// What plugin callbacks receive as `ctx`.
#[derive(Default)]
pub struct UiContext {
   pub channel_id:      Option<String>,
   pub message_id:      Option<String>,
   pub user_id:         Option<String>,
   pub current_user_id: Option<String>,
   /// The message as Phoenix cached it, JSON.
   pub message:         Option<String>,
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
   /// A plugin's profile tab.
   Tab { contribution: ContributionId },
   /// A press in a profile tab's page.
   Block {
      contribution: ContributionId,
      index:        u32,
   },
   /// A text box in a profile tab's page.
   Input {
      contribution: ContributionId,
      index:        u32,
   },
   /// A profile tab page's content, which reports its height.
   Page { contribution: ContributionId },
   /// The send button of a text box in a profile tab's page.
   Send {
      contribution: ContributionId,
      index:        u32,
   },
   /// A row of a plugin menu sheet, or with `index` -1 the sheet itself,
   /// which says when it was swiped away.
   Choice { menu: u32, index: i32 },
   /// An entry in a profile tab's page that takes long presses.
   Hold {
      contribution: ContributionId,
      index:        u32,
   },
   /// Back button of a plugin full-screen page.
   PageBack(u32),
   /// Action button on a plugin full-screen page.
   PageButton { page: u32, index: u32 },
   /// Tap on an in-app notification banner card.
   BannerPress(u32),
   /// Tap on an in-app notification banner's dismiss button.
   BannerDismiss(u32),
   /// Events on a raw JS-mounted subtree (`phoenix.ui.mount`).
   Mounted {
      mount:   u32,
      press:   Option<u32>,
      dismiss: Option<u32>,
   },
}

/// A node to create, with the handler its subtree answers presses with.
pub struct Draft {
   kind:  ViewKind,
   props: Props,
   kids:  Vec<Self>,
   press: Option<Handler>,
   src:   Option<u32>,
}

impl Draft {
   fn from_node(node: &Node) -> Self {
      Self {
         kind:  node.kind.clone(),
         props: node.props.clone(),
         kids:  node.kids.iter().map(Self::from_node).collect(),
         press: None,
         src:   node.src,
      }
   }

   fn find(&mut self, src: u32) -> Option<&mut Self> {
      if self.src == Some(src) {
         return Some(self);
      }
      self.kids.iter_mut().find_map(|kid| kid.find(src))
   }

   fn set(&mut self, key: Prop, value: Option<Value>) {
      set_prop(&mut self.props, key, value);
   }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
   Target(Target),
   Suggestions,
   /// A plugin menu sheet.
   Menu(u32),
   /// A plugin full-screen page pushed onto a navigator.
   Page(u32),
   /// A native top in-app notification banner attached to the root view.
   Banner(u32),
   /// A raw JS-mounted subtree (`phoenix.ui.mount`).
   Mounted(u32),
}

/// Placement of a raw JS-mounted subtree inside its parent.
#[derive(Clone, Copy)]
pub enum MountBefore {
   Start,
   End,
   Node(i32),
}

/// A raw node specification for `phoenix.ui.mount`.
pub struct MountNode {
   pub kind:       ViewKind,
   pub props:      Props,
   pub kids:       Vec<Self>,
   pub on_press:   Option<u32>,
   pub on_dismiss: Option<u32>,
}

/// What happens when the user taps an in-app notification banner.
pub enum BannerAction {
   None,
   ApplyUpdate,
   JsCallback,
}

/// Specification for a native Discord in-app notification banner.
pub struct BannerSpec {
   pub id:          u32,
   pub title:       String,
   pub body:        Option<String>,
   pub icon:        Option<String>,
   pub accent:      Option<i64>,
   pub duration_ms: u64,
   pub action:      BannerAction,
}

/// A button row on a plugin full-screen page.
pub struct PageButton {
   pub label:  String,
   pub icon:   Option<String>,
   pub danger: bool,
   pub index:  u32,
}

/// A section on a plugin full-screen page.
pub enum PageSection {
   Code {
      title:     Option<String>,
      text:      String,
      monospace: bool,
      highlight: bool,
   },
   Text {
      title: Option<String>,
      text:  String,
   },
   Buttons {
      title:   Option<String>,
      buttons: Vec<PageButton>,
   },
}

/// A menu sheet row a plugin asked for.
pub struct MenuItem {
   pub label:  String,
   pub icon:   Option<String>,
   pub danger: bool,
}

/// Scrollable text content shown inside a plugin sheet.
pub struct SheetBody {
   pub text:      String,
   pub monospace: bool,
   pub highlight: bool,
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
   /// The Phoenix node the slot was opened for.
   anchor: i32,
   parent: i32,
   before: Before,
   place:  Place,
   roots:  Vec<i32>,
   all:    Vec<i32>,
}

#[derive(Default)]
struct Patches {
   mirror:                 Mirror,
   templates:              Templates,
   seen:                   Seen,
   contributions:          Vec<Contribution>,
   commands:               Vec<Command>,
   interceptors:           usize,
   pickers:                usize,
   event_hooks:            Vec<EventHookMeta>,
   node_watchers:          Vec<NodeWatcher>,
   prop_rules:             Vec<PropPatch>,
   /// Live lists discovered for each `PatchTarget::List` prop patch (by patch
   /// id).
   patched_lists:          HashMap<i32, HashSet<u64>>,
   slots:                  Vec<Slot>,
   anchors:                HashMap<Target, HashSet<i32>>,
   handlers:               HashMap<i32, Handler>,
   results:                HashMap<(ContributionId, Place), Outcome>,
   pending:                HashSet<(ContributionId, Place)>,
   next_id:                i32,
   /// The channel last seen open.
   channel:                Option<String>,
   suggestions:            commands::Suggestions,
   /// Sends plugins already saw, let through once when Rust gets them back.
   resent:                 Vec<Vec<u8>>,
   /// Message text color plugins asked for, by message id.
   tints:                  HashMap<String, i64>,
   /// Phoenix's author and content text nodes: their message and props as
   /// Phoenix set them.
   texts:                  HashMap<i32, (String, Props)>,
   /// The plugin tab open on each profile pager.
   shown:                  HashMap<i32, ContributionId>,
   /// The height Rust last gave each pager.
   rust_heights:           HashMap<i32, Value>,
   /// The height of each plugin page, by pager.
   page_heights:           HashMap<(i32, ContributionId), f64>,
   /// What is typed in the text boxes of plugin pages, so a redraw keeps it.
   typed:                  HashMap<(ContributionId, Place, u32), String>,
   /// A plugin tab's text and bar, by the tab.
   tab_parts:              HashMap<i32, (i32, i32)>,
   /// Open menu sheets and who waits for them.
   menus:                  HashMap<u32, Sender<i32>>,
   /// Last seen parent container that hosts sheets, kept so sheets can open
   /// even after a message action sheet dismisses.
   sheet_host:             Option<i32>,
   /// Pagers whose own tab labels we color, since a plugin tab was opened:
   /// their animation stops at Phoenix's last tab and would light it up.
   static_tabs:            HashSet<i32>,
   /// Accessibility ids hidden by `phoenix.ui.hide`, with active refcount.
   hidden:                 HashMap<String, usize>,
   /// `(handle, plugin, aid)` entries for `phoenix.ui.hide`.
   hidden_entries:         Vec<(u64, String, String)>,
   /// Node ids on which `phoenix.ui.hide` currently holds `hidden: true`
   /// (`View.GONE`). Reconciled on every batch so dynamic swaps (like
   /// `"Record Voice Message"` -> `"send"` inside the same wrapper) restore
   /// visibility automatically.
   hidden_nodes:           HashSet<i32>,
   /// When the root view first became ready, so cold-start banners wait for
   /// the splash screen to clear.
   root_ready_at:          Option<Instant>,
   /// Banners waiting for the root view / cold-start settle delay.
   pending_banners:        Vec<BannerSpec>,
   banner_flush_scheduled: bool,
   /// Tap actions for currently visible in-app notification banners, by id.
   banner_actions:         HashMap<u32, BannerAction>,
}

static ROOT_ID: AtomicI32 = AtomicI32::new(0);

static PATCHES: LazyLock<Mutex<Patches>> = LazyLock::new(|| {
   Mutex::new(Patches {
      next_id: FIRST_ID,
      ..Patches::default()
   })
});
/// Set once a plugin adds anything, so builds without UI plugins never
/// decode a batch.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Your messages' text as Discord has it, by the text plugin hooks turned it
/// into. Phoenix fills the edit box from the hooked text, and saving it would
/// post what the plugins added.
static ORIGINALS: Mutex<VecDeque<(String, String)>> = Mutex::new(VecDeque::new());
static GUARDING: AtomicBool = AtomicBool::new(false);
const ORIGINALS_CAP: usize = 256;

/// Remembers that a plugin hook turned your message `original` into `shown`.
pub fn remember_original(shown: String, original: String) {
   let mut originals = ORIGINALS.lock().unwrap_or_else(PoisonError::into_inner);
   originals.retain(|(known, _)| *known != shown);
   if originals.len() == ORIGINALS_CAP {
      originals.pop_front();
   }
   originals.push_back((shown, original));
   drop(originals);
   GUARDING.store(true, Ordering::Release);
}

fn patches() -> MutexGuard<'static, Patches> {
   PATCHES.lock().unwrap_or_else(PoisonError::into_inner)
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

/// Draws `id` again, asking its plugin anew: everywhere, or only at `place`,
/// where the old result stays up until the new one is in.
pub fn update(id: ContributionId, place: Option<Place>) {
   let mut state = patches();
   if let Some(place) = place {
      if state.pending.insert((id, place.clone())) {
         queue(Job::Evaluate(id, place));
      }
      return;
   }
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

/// What [`update_button`] does to a button's tint.
#[derive(Clone, Copy)]
pub enum TintChange {
   Keep,
   Clear,
   Set(i64),
}

/// Updates a button contribution's icon, label, or tint and redraws its target.
pub fn update_button(
   id: ContributionId,
   icon: Option<String>,
   label: Option<String>,
   tint: TintChange,
) {
   let mut state = patches();
   let Some(contribution) = state.contributions.iter_mut().find(|known| known.id == id) else {
      return;
   };
   if let Some(icon) = icon {
      contribution.icon = Some(icon);
   }
   if let Some(label) = label {
      contribution.label = label;
   }
   match tint {
      TintChange::Keep => {},
      TintChange::Clear => contribution.tint = None,
      TintChange::Set(color) => contribution.tint = Some(color),
   }
   let target = contribution.target;
   drop(state);
   queue(Job::Refresh(target));
}

/// Shows a menu sheet like the message long-press one and waits for the
/// choice: the row's index, -1 when it was swiped away, or `None` when no
/// such sheet can be drawn yet.
pub fn menu(title: Option<String>, items: Vec<MenuItem>) -> Option<i32> {
   let (answer, receiver) = mpsc::channel();
   queue(Job::Menu {
      title,
      body: None,
      items,
      answer,
   });
   match receiver.recv() {
      Ok(-2) | Err(_) => None,
      Ok(index) => Some(index),
   }
}

/// Shows a scrollable text/code viewer sheet with optional action rows.
pub fn sheet(title: Option<String>, body: SheetBody, items: Vec<MenuItem>) -> Option<i32> {
   let (answer, receiver) = mpsc::channel();
   queue(Job::Menu {
      title,
      body: Some(body),
      items,
      answer,
   });
   match receiver.recv() {
      Ok(-2) | Err(_) => None,
      Ok(index) => Some(index),
   }
}

/// Pushes a full-screen page onto Phoenix's active navigator.
pub fn open_page(id: u32, title: String, sections: Vec<PageSection>) {
   ACTIVE.store(true, Ordering::Release);
   queue(Job::OpenPage {
      id,
      title,
      sections,
   });
}

/// Closes a plugin full-screen page pushed with [`open_page`].
pub fn close_page(id: u32) {
   queue(Job::ClosePage(id));
}

/// Shows a native Discord in-app notification banner at the top of the screen.
/// Returns the banner id so it can be dismissed early with [`dismiss_banner`].
pub fn show_banner(
   title: String,
   body: Option<String>,
   icon: Option<String>,
   accent: Option<i64>,
   duration_ms: u64,
   action: BannerAction,
) -> u32 {
   static NEXT: AtomicU32 = AtomicU32::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   ACTIVE.store(true, Ordering::Release);
   queue(Job::ShowBanner(BannerSpec {
      id,
      title,
      body,
      icon,
      accent,
      duration_ms,
      action,
   }));
   id
}

/// Dismisses an in-app notification banner early if it is still visible.
pub fn dismiss_banner(id: u32) {
   queue(Job::DismissBanner(id));
}

/// Mounts a raw JS-constructed Phoenix subtree onto `parent` (or the root view
/// when `parent` is `None`).
pub fn mount(id: u32, parent: Option<i32>, before: MountBefore, node: MountNode) {
   ACTIVE.store(true, Ordering::Release);
   queue(Job::Mount {
      id,
      parent,
      before,
      node,
   });
}

/// Unmounts a raw JS-constructed subtree mounted with [`mount`].
pub fn unmount(id: u32) {
   queue(Job::Unmount(id));
}

/// The current Phoenix root view id, if seen.
pub fn root_id() -> Option<i32> {
   patches().mirror.root()
}

/// Serializes the live mirrored view tree for the debug inspector (`GET
/// /tree`).
pub fn dump_tree(root: Option<i32>, max_depth: usize) -> serde_json::Value {
   let state = patches();
   let Some(id) = root.or_else(|| state.mirror.root()) else {
      return serde_json::json!({ "error": "root node not seen yet" });
   };
   state.mirror.dump_tree(id, max_depth)
}

/// Hides every Phoenix node with accessibility id `aid` by setting
/// `hidden: true` (`View.GONE`) on it (or its unnamed single-child wrapper).
/// Returns a handle for [`unhide`].
pub fn hide(plugin: String, aid: String) -> u64 {
   static NEXT: AtomicU64 = AtomicU64::new(1);
   let handle = NEXT.fetch_add(1, Ordering::Relaxed);
   ACTIVE.store(true, Ordering::Release);
   let mut state = patches();
   *state.hidden.entry(aid.clone()).or_default() += 1;
   state.hidden_entries.push((handle, plugin, aid));
   drop(state);
   queue(Job::RefreshHidden);
   handle
}

/// Undoes a [`hide`] call and restores the node if no other plugin hides it.
pub fn unhide(handle: u64) {
   let mut state = patches();
   let Some(pos) = state
      .hidden_entries
      .iter()
      .position(|(id, ..)| *id == handle)
   else {
      return;
   };
   let (_, _, aid) = state.hidden_entries.remove(pos);
   if let Some(count) = state.hidden.get_mut(&aid) {
      *count = count.saturating_sub(1);
      if *count == 0 {
         state.hidden.remove(&aid);
      }
   }
   drop(state);
   queue(Job::RefreshHidden);
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

/// How many `onEmojiPick` handlers plugins hold, so picker presses are taken,
/// and Nitro-locked emojis offered, only while one exists.
pub fn set_emoji_pickers(count: usize) {
   if count > 0 {
      ACTIVE.store(true, Ordering::Release);
   }
   patches().pickers = count;
   emoji::unlock(count > 0);
}

/// Registers a raw view event hook (`phoenix.ui.onEvent`).
pub fn add_event_hook(id: u64, plugin: String, event: Key, intercept: bool) {
   ACTIVE.store(true, Ordering::Release);
   patches().event_hooks.push(EventHookMeta {
      id,
      plugin,
      event,
      intercept,
   });
}

pub fn remove_event_hook(id: u64) {
   patches().event_hooks.retain(|hook| hook.id != id);
}

/// Registers a raw node creation watcher (`phoenix.ui.onNode`).
pub fn add_node_watcher(id: u64, plugin: String, matcher: NodeMatch) {
   ACTIVE.store(true, Ordering::Release);
   patches().node_watchers.push(NodeWatcher {
      id,
      plugin,
      matcher,
   });
}

pub fn remove_node_watcher(id: u64) {
   patches().node_watchers.retain(|watcher| watcher.id != id);
}

/// Registers a declarative Rust-side prop patch (`phoenix.ui.patchProps`).
pub fn add_prop_patch(
   plugin: String,
   matcher: NodeMatch,
   target: PatchTarget,
   props: Props,
   add_on: Vec<String>,
   once: bool,
   timeout: Option<Duration>,
) -> u64 {
   static NEXT: AtomicU64 = AtomicU64::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   ACTIVE.store(true, Ordering::Release);
   let is_list = target == PatchTarget::List;
   patches().prop_rules.push(PropPatch {
      id,
      plugin,
      matcher,
      target,
      props,
      add_on,
      once,
      deadline: timeout.map(|dur| Instant::now() + dur),
   });
   if is_list {
      queue(Job::RefreshPropPatches);
   }
   id
}

pub fn remove_prop_patch(id: u64) {
   let mut state = patches();
   let was_list = state
      .prop_rules
      .iter()
      .any(|patch| patch.id == id && patch.target == PatchTarget::List);
   state.prop_rules.retain(|patch| patch.id != id);
   drop(state);
   if was_list {
      queue(Job::RefreshPropPatches);
   }
}

/// Injects a raw view event (`phoenix.ui.dispatch`) straight to Phoenix's Rust
/// guest.
pub fn dispatch_event(node: i32, event: Key, payload: &Value) -> bool {
   let frame = encode_view_event(node, event, payload);
   patches().resent.push(frame.clone());
   if !bridge::inject(&frame) {
      patches().resent.retain(|sent| *sent != frame);
      return false;
   }
   true
}

/// Sends a raw `Op::Update` (`phoenix.ui.update`) on the patches worker thread.
pub fn update_node(node: i32, props: Props) {
   ACTIVE.store(true, Ordering::Release);
   queue(Job::UpdateNode { node, props });
}

/// Finds all live nodes matching `matcher` (`phoenix.ui.find`).
pub fn find_nodes(matcher: &NodeMatch) -> Vec<i32> {
   patches().mirror.nodes_where(&|aid| matcher.matches(aid))
}

/// Finds the first node at or under `root` matching `matcher`
/// (`phoenix.ui.findDown`).
pub fn find_down(root: i32, matcher: &NodeMatch, max_depth: usize) -> Option<i32> {
   patches()
      .mirror
      .find_down(root, &|aid| matcher.matches(aid), max_depth)
}

/// Snapshot of a live node (`phoenix.ui.node`).
pub fn node_info(id: i32) -> Option<NodeInfo> {
   let state = patches();
   state.mirror.exists(id).then(|| state.build_node_info(id))
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
   let mut unhidden = Vec::new();
   state.hidden_entries.retain(|(_, owner, aid)| {
      if owner == plugin {
         unhidden.push(aid.clone());
         false
      } else {
         true
      }
   });
   for aid in &unhidden {
      if let Some(count) = state.hidden.get_mut(aid) {
         *count = count.saturating_sub(1);
         if *count == 0 {
            state.hidden.remove(aid);
         }
      }
   }
   state.event_hooks.retain(|hook| hook.plugin != plugin);
   state
      .node_watchers
      .retain(|watcher| watcher.plugin != plugin);
   let had_list_patches = state
      .prop_rules
      .iter()
      .any(|patch| patch.plugin == plugin && patch.target == PatchTarget::List);
   state.prop_rules.retain(|patch| patch.plugin != plugin);
   drop(state);
   if !unhidden.is_empty() {
      queue(Job::RefreshHidden);
   }
   for target in targets {
      queue(Job::Refresh(target));
   }
   if had_commands {
      queue(Job::Suggest);
   }
   if had_list_patches {
      queue(Job::RefreshPropPatches);
   }
}

const HIDDEN: Prop = Prop::named("hidden");

fn number(value: &Value) -> Option<f64> {
   match value {
      Value::F64(number) => Some(*number),
      Value::I32(number) => Some(f64::from(*number)),
      _ => None,
   }
}
const BACKGROUND: Prop = Prop::named("background");

/// The message id in `message.{content,avatar,author,timestamp}:<id>`.
fn message_id(aid: &str) -> Option<&str> {
   let (kind, id) = aid.strip_prefix("message.")?.split_once(':')?;
   (matches!(kind, "content" | "avatar" | "author" | "timestamp")
      && !id.is_empty()
      && id.bytes().all(|byte| byte.is_ascii_digit()))
   .then_some(id)
}
