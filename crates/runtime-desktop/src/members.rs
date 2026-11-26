//! Plugin decorations keyed by user id for Desktop Phoenix: the member list
//! (`phoenix.ui.addMemberDecoration`) and message headers
//! (`phoenix.ui.addMessageHeaderDecoration`, drawn by [`crate::headers`]).
//!
//! In the member list's row renderer, the function formatting `member-{}-{}`
//! found by [`desktop::member_row`], the calls adding the member's name to its
//! row are pointed at a detour that adds plugin decorations right after it.

use std::{
   collections::HashMap,
   ffi::c_void,
   sync::{
      LazyLock,
      OnceLock,
      RwLock,
      atomic::{
         AtomicUsize,
         Ordering,
      },
      mpsc,
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::{
   accessories::{
      RenderedAccessory,
      invalidate_discord_windows,
   },
   gpui,
};

/// `Div::child` by value, as the name row adds the name: `(sret, div, &name)`.
type NameChildFn = unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> *mut c_void;

enum CacheEntry {
   Pending,
   Ready(Vec<RenderedAccessory>),
}

const MAX_CACHE_ENTRIES: usize = 8192;
/// Snowflakes from 2015 on are far above this, so anything below is not an id.
pub const MIN_SNOWFLAKE: u64 = 1 << 40;

/// Where decorations keyed by user id go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
   /// After names in the member list.
   Members,
   /// After authors' names in message headers.
   Headers,
}

impl Place {
   pub const fn decorations(self) -> &'static Decorations {
      match self {
         Self::Members => &MEMBERS,
         Self::Headers => &HEADERS,
      }
   }
}

/// What a [`Place`]'s decorations drew per user, filled on a worker thread so
/// Discord's render never waits on plugins.
pub struct Decorations {
   place:  Place,
   active: AtomicUsize,
   cache:  LazyLock<RwLock<HashMap<u64, CacheEntry>>>,
   worker: OnceLock<mpsc::Sender<u64>>,
}

pub static MEMBERS: Decorations = Decorations::new(Place::Members);
pub static HEADERS: Decorations = Decorations::new(Place::Headers);

impl Decorations {
   const fn new(place: Place) -> Self {
      Self {
         place,
         active: AtomicUsize::new(0),
         cache: LazyLock::new(|| RwLock::new(HashMap::new())),
         worker: OnceLock::new(),
      }
   }

   pub fn active(&self) -> bool {
      self.active.load(Ordering::Relaxed) > 0
   }

   pub fn set_active_count(&'static self, count: usize) {
      self.active.store(count, Ordering::Release);
      if count > 0 {
         self.ensure_worker();
      }
   }

   pub fn clear_cache(&self) {
      if let Ok(mut map) = self.cache.write() {
         map.clear();
      }
   }

   /// Stores what the decorations drew for `user_id` and repaints when it
   /// changed.
   pub fn set_rendered(&self, user_id: u64, mut items: Vec<RenderedAccessory>) {
      items.sort_by_key(|a| a.priority);
      let changed = self.cache.write().is_ok_and(|mut map| {
         if map.len() > MAX_CACHE_ENTRIES {
            map.clear();
         }
         let same = matches!(map.get(&user_id), Some(CacheEntry::Ready(old)) if *old == items);
         map.insert(user_id, CacheEntry::Ready(items));
         !same
      });
      if changed {
         invalidate_discord_windows();
      }
   }

   fn ensure_worker(&'static self) -> &'static mpsc::Sender<u64> {
      self.worker.get_or_init(|| {
         let (tx, rx) = mpsc::channel::<u64>();
         let name = match self.place {
            Place::Members => "sunbird-members",
            Place::Headers => "sunbird-headers",
         };
         let _ = std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
               while let Ok(id) = rx.recv() {
                  let mut batch = vec![id];
                  while let Ok(more) = rx.try_recv() {
                     if !batch.contains(&more) {
                        batch.push(more);
                     }
                  }
                  for uid in batch {
                     if self.active.load(Ordering::Acquire) == 0 {
                        break;
                     }
                     let items = phoenix_runtime::engine().map_or_default(|engine| {
                        crate::js::ui::render_decorations(&engine, self.place, uid)
                     });
                     self.set_rendered(uid, items);
                  }
               }
            });
         tx
      })
   }

   /// What the decorations drew for `user_id`, queueing them when unknown.
   pub fn lookup(&'static self, user_id: u64) -> Option<Vec<RenderedAccessory>> {
      if let Ok(map) = self.cache.read()
         && let Some(entry) = map.get(&user_id)
      {
         return match entry {
            CacheEntry::Ready(items) if !items.is_empty() => Some(items.clone()),
            _ => None,
         };
      }
      let queue = self.cache.write().is_ok_and(|mut map| {
         if map.contains_key(&user_id) {
            return false;
         }
         if map.len() > MAX_CACHE_ENTRIES {
            map.clear();
         }
         map.insert(user_id, CacheEntry::Pending);
         true
      });
      if queue {
         let _ = self.ensure_worker().send(user_id);
      }
      None
   }
}

static ORIG_NAME_CHILD: OnceLock<NameChildFn> = OnceLock::new();
static CLOSURE_SLOT: AtomicUsize = AtomicUsize::new(0);
static ENV_USER_ID: AtomicUsize = AtomicUsize::new(0);

/// Locates the member row and hooks the calls adding its name, from
/// `DllMain`.
pub fn install(image: &Image<'static>) {
   if !gpui::supports_accessories() {
      log::warn!("desktop members: GPUI text/push primitives unavailable, skipping");
      return;
   }
   // Without it, decorations show their text instead of icons.
   gpui::locate_svg(image);
   match desktop::member_row(image) {
      Ok(row) => install_name(image, &row.name),
      Err(err) => log::warn!("desktop members: {err}, skipping"),
   }
}

/// Points the calls adding the member's name to its row at
/// [`name_child_detour`], so decorations follow the name inside that row. The
/// islands hand the detour the row function's frame, where the closure's
/// environment leads to the user id.
fn install_name(image: &Image<'static>, name: &desktop::MemberName) {
   CLOSURE_SLOT.store(name.closure_slot, Ordering::Release);
   ENV_USER_ID.store(name.env, Ordering::Release);
   for &site in &name.sites {
      let Some(target) = image
         .read(site + 1, 4)
         .and_then(|rel| rel.try_into().ok())
         .map(i32::from_le_bytes)
         .map(|rel| (site + 5).wrapping_add_signed(rel as isize))
      else {
         continue;
      };
      // SAFETY: `target` is the call's own `Div::child`, the same for every
      // site, stored before a call can reach the detour.
      let _ = ORIG_NAME_CHILD.set(unsafe { std::mem::transmute::<usize, NameChildFn>(target) });
      // SAFETY: runs from `prepare` before Discord threads execute; the island
      // only sets `r9`, which the three-argument `Div::child` does not read.
      match unsafe {
         image.hook_call(
            site,
            &desktop::mov_r9_from(phoenix_hook::x86::Register::RBP),
            name_child_detour as *const () as usize,
         )
      } {
         Ok(_) => log::info!("desktop members: hooked the member name @ {site:#x}"),
         Err(err) => log::warn!("desktop members: member name hook failed: {err}"),
      }
   }
}

unsafe extern "C" fn name_child_detour(
   out: *mut c_void,
   div: *mut c_void,
   child: *mut c_void,
   frame: *const u8,
) -> *mut c_void {
   let Some(orig) = ORIG_NAME_CHILD.get().copied() else {
      return out;
   };
   // SAFETY: forwards `Div::child`'s own arguments.
   let row = unsafe { orig(out, div, child) };
   if !MEMBERS.active() || row.is_null() || frame.is_null() {
      return row;
   }
   // SAFETY: `frame` is the row function's `rbp`, whose decoded slot holds the
   // closure's environment, which holds a pointer to the member's user id.
   let user_id = unsafe {
      let env = frame
         .add(CLOSURE_SLOT.load(Ordering::Relaxed))
         .cast::<*const u8>()
         .read_unaligned();
      if env.is_null() {
         return row;
      }
      let id = env
         .add(ENV_USER_ID.load(Ordering::Relaxed))
         .cast::<*const u64>()
         .read_unaligned();
      if id.is_null() {
         return row;
      }
      id.read_unaligned()
   };
   if user_id < MIN_SNOWFLAKE {
      return row;
   }
   if let Some(lines) = lines_for(user_id) {
      let _ = std::panic::catch_unwind(|| gpui::append_pieces(row, &lines));
   }
   row
}

/// The text decorations drew for `user_id`, queueing them when unknown.
fn lines_for(user_id: u64) -> Option<Vec<gpui::Piece>> {
   let lines = MEMBERS
      .lookup(user_id)?
      .iter()
      .filter(|item| !item.text.is_empty() || !item.icons.is_empty())
      .map(RenderedAccessory::piece)
      .collect::<Vec<_>>();
   (!lines.is_empty()).then_some(lines)
}
