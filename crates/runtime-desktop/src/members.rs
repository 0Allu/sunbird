//! Native GPUI member list decorations (`phoenix.ui.addMemberDecoration`) for
//! Desktop Phoenix.
//!
//! Hooks the member list's row renderer, the function formatting
//! `member-{}-{}`, found by [`desktop::member_row`]. It takes `(sret, &item,
//! ..)` like `MessageRow::render`, and a member item keeps the user id at a
//! decoded offset. Plugin decorations lie over the row's right end.

use std::{
   collections::HashMap,
   sync::{
      LazyLock,
      OnceLock,
      RwLock,
      atomic::{
         AtomicU8,
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
   gpui::{
      self,
      RawAnyElement,
   },
};

/// The member row renderer: `sret`, the list item, two registers and seven
/// stack words, forwarded as they come.
type RenderMemberFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   item: *const u8,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
   s4: u64,
   s5: u64,
   s6: u64,
) -> *mut RawAnyElement;

enum CacheEntry {
   Pending,
   Ready(Vec<RenderedAccessory>),
}

const MAX_CACHE_ENTRIES: usize = 8192;
/// Snowflakes from 2015 on are far above this, so anything below is not an id.
const MIN_SNOWFLAKE: u64 = 1 << 40;

static ORIG_RENDER_MEMBER: OnceLock<RenderMemberFn> = OnceLock::new();
static USER_ID_OFFSET: AtomicUsize = AtomicUsize::new(0);
static MEMBER_TAG: AtomicU8 = AtomicU8::new(u8::MAX);
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static CACHE: LazyLock<RwLock<HashMap<u64, CacheEntry>>> =
   LazyLock::new(|| RwLock::new(HashMap::new()));
static WORKER_TX: OnceLock<mpsc::Sender<u64>> = OnceLock::new();

pub fn set_active_count(count: usize) {
   ACTIVE_COUNT.store(count, Ordering::Release);
   if count > 0 {
      ensure_worker();
   }
}

pub fn clear_cache() {
   if let Ok(mut map) = CACHE.write() {
      map.clear();
   }
}

/// Stores what the decorations drew for `user_id` and repaints when it
/// changed.
pub fn set_rendered(user_id: u64, mut items: Vec<RenderedAccessory>) {
   items.sort_by_key(|a| a.priority);
   let changed = CACHE.write().is_ok_and(|mut map| {
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

fn ensure_worker() -> &'static mpsc::Sender<u64> {
   WORKER_TX.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<u64>();
      let _ = std::thread::Builder::new()
         .name("sunbird-members".to_owned())
         .spawn(move || {
            while let Ok(id) = rx.recv() {
               let mut batch = vec![id];
               while let Ok(more) = rx.try_recv() {
                  if !batch.contains(&more) {
                     batch.push(more);
                  }
               }
               for uid in batch {
                  if ACTIVE_COUNT.load(Ordering::Acquire) == 0 {
                     break;
                  }
                  let items = phoenix_runtime::engine()
                     .map_or_default(|engine| crate::js::ui::render_members(&engine, uid));
                  set_rendered(uid, items);
               }
            }
         });
      tx
   })
}

fn lookup_cached(user_id: u64) -> Option<Vec<RenderedAccessory>> {
   if let Ok(map) = CACHE.read()
      && let Some(entry) = map.get(&user_id)
   {
      return match entry {
         CacheEntry::Ready(items) if !items.is_empty() => Some(items.clone()),
         _ => None,
      };
   }
   let queue = CACHE.write().is_ok_and(|mut map| {
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
      let _ = ensure_worker().send(user_id);
   }
   None
}

/// Locates and hooks the member row renderer from `DllMain`.
pub fn install(image: &Image<'static>) {
   if !gpui::supports_accessories() {
      log::warn!("desktop members: GPUI text/push primitives unavailable, skipping");
      return;
   }
   // Without it, decorations show their text instead of icons.
   gpui::locate_svg(image);
   let row = match desktop::member_row(image) {
      Ok(row) => row,
      Err(err) => {
         log::warn!("desktop members: {err}, skipping");
         return;
      },
   };
   USER_ID_OFFSET.store(row.user_id_offset, Ordering::Release);
   MEMBER_TAG.store(row.member_tag, Ordering::Release);
   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).row;
   // SAFETY: runs from `prepare` before Discord threads execute;
   // `render_member_detour` forwards the renderer's `sret`, two register and
   // seven stack arguments unchanged.
   let trampoline = match unsafe {
      image.hook_entry(
         row.func,
         pro.pattern,
         pro.len,
         render_member_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop members: member row hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the prologue and jumps past it.
   let orig = unsafe { std::mem::transmute::<usize, RenderMemberFn>(trampoline) };
   let _ = ORIG_RENDER_MEMBER.set(orig);
   log::info!(
      "desktop members: hooked the member row @ {:#x} (tag {}, user id +{:#x})",
      row.func,
      row.member_tag,
      row.user_id_offset
   );
}

/// The text decorations drew for `user_id`, queueing them when unknown.
fn lines_for(user_id: u64) -> Option<Vec<gpui::Piece>> {
   let lines = lookup_cached(user_id)?
      .iter()
      .filter(|item| !item.text.is_empty() || !item.icons.is_empty())
      .map(RenderedAccessory::piece)
      .collect::<Vec<_>>();
   (!lines.is_empty()).then_some(lines)
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn render_member_detour(
   sret: *mut RawAnyElement,
   item: *const u8,
   a2: usize,
   a3: usize,
   s0: u64,
   s1: u64,
   s2: u64,
   s3: u64,
   s4: u64,
   s5: u64,
   s6: u64,
) -> *mut RawAnyElement {
   let Some(orig) = ORIG_RENDER_MEMBER.get().copied() else {
      return sret;
   };
   // SAFETY: forwards every argument to the original renderer.
   let out = unsafe { orig(sret, item, a2, a3, s0, s1, s2, s3, s4, s5, s6) };
   if ACTIVE_COUNT.load(Ordering::Relaxed) == 0
      || item.is_null()
      || sret.is_null()
   {
      return out;
   }
   // SAFETY: `item` is the row's live list item; its first byte is the enum
   // tag, and member items keep the user id at the decoded offset.
   let user_id = unsafe {
      if item.read() != MEMBER_TAG.load(Ordering::Relaxed) {
         return out;
      }
      item
         .add(USER_ID_OFFSET.load(Ordering::Relaxed))
         .cast::<u64>()
         .read_unaligned()
   };
   if user_id < MIN_SNOWFLAKE {
      return out;
   }
   if let Some(lines) = lines_for(user_id) {
      let _ = std::panic::catch_unwind(|| gpui::wrap_with_overlay(sret, &lines, None));
   }
   out
}
