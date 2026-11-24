//! Native GPUI message accessories (`phoenix.ui.addMessageAccessory`) for
//! Desktop Phoenix.
//!
//! Hooks Discord's message row GPUI renderer (`MessageRow::render`) during
//! [`crate::prepare`] using build-independent anchors:
//! 1. Locate the unique Rust format template `\x10message-content-\xc0\x00` in
//!    `.rdata` and its `.text` `lea` xrefs (the two message-content helpers).
//! 2. Intersect their direct callers' `.pdata` functions to find the single
//!    `MessageRow::render` function (`sret: *mut RawAnyElement`, `row: *const c_void`,
//!    `r8`, `r9`, plus 7 stack qwords).
//! 3. Decode `message_id: u64`'s byte offset inside `row` from the
//!    `lea rax, [rcx + disp32]` right before the `message-content-` xref.
//! 4. After calling the original renderer, if any plugin returned accessory
//!    lines or a tint for `message_id`, wrap the rendered `AnyElement` in a
//!    native GPUI `Div` with colored text lines underneath and an optional
//!    tinted left accent bar — without ever modifying `message.content`.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   ffi::c_void,
   ptr,
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

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

use crate::desktop_gpui::{
   self,
   RawAnyElement,
};

const DEFAULT_MSG_ID_OFFSET: usize = 0x7d8;
/// Offset of the pending/sending message text color inside `RowColors`
/// (verified at `0x14a741c1f`: `mov edx, [r12 + 0x34]`).
const ROW_COLORS_MUTED_DELTA: usize = 0x34;

/// `MessageRow::render` & `build_message_body` prologue: 8 callee-saved pushes +
/// `mov eax, imm32` (17 bytes, no RIP-relative operands; stops right before `call __chkstk`).
const ROW_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 B8 ?? ?? ?? ??");
const ROW_PROLOGUE_LEN: usize = 17;

/// `lea rax, [rcx + disp32]` loading `&row.message_id` right before formatting
/// `message-content-{id}`.
const MSG_ID_LEA: Pattern = Pattern::new("48 8D 81 ?? ?? ?? ??");

/// `mov rcx, [rcx + disp32]` loading `msg.message_id` near the entry of
/// `build_message_body`.
const BODY_MSG_ID_MOV: Pattern = Pattern::new("48 8B 89 ?? ?? ?? ??");

/// `mov eax, imm32; mov ecx, imm32; mov edx, imm32` loading consecutive
/// `RowColors` field offsets (`+0x0c`, `+0x08`, `+0x04`) in `build_message_body`.
const ROW_COLORS_TRIPLE: Pattern =
   Pattern::new("B8 ?? ?? ?? ?? B9 ?? ?? ?? ?? BA ?? ?? ?? ??");

/// Same-client optimistic delete (`Store::delete_message`) prologue:
/// 8 callee-saved pushes + `sub rsp, imm32` (19 bytes, no RIP-relative operands).
const OPT_DEL_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 48 81 EC ?? ?? ?? ??");
const OPT_DEL_PROLOGUE_LEN: usize = 19;

type OptimisticDeleteFn =
   unsafe extern "C" fn(store: *mut c_void, args: *const *const u64, meta: *const c_void) -> u8;

type BuildMessageBodyFn = unsafe extern "C" fn(
   msg: *const c_void,
   config: *mut c_void,
   a2: u64,
   a3: u64,
   s0: u64,
   s1: u64,
) -> *mut c_void;

type RenderRowFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   row: *const c_void,
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

/// One rendered plugin message accessory line for Desktop GPUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedAccessory {
   pub priority: i32,
   pub text:     String,
   pub color:    Option<u32>,
   pub tint:     Option<u32>,
}

enum CacheEntry {
   Pending,
   Ready(Vec<RenderedAccessory>),
}

static ORIG_RENDER_ROW: OnceLock<RenderRowFn> = OnceLock::new();
static ORIG_BUILD_BODY: OnceLock<BuildMessageBodyFn> = OnceLock::new();
static ORIG_OPT_DELETE: OnceLock<OptimisticDeleteFn> = OnceLock::new();
static MSG_ID_OFFSET: AtomicUsize = AtomicUsize::new(DEFAULT_MSG_ID_OFFSET);
static BODY_MSG_ID_OFFSET: AtomicUsize = AtomicUsize::new(0);
static ROW_COLORS_OFFSET: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_COUNT: AtomicUsize = AtomicUsize::new(0);
static CACHE: LazyLock<RwLock<HashMap<u64, CacheEntry>>> =
   LazyLock::new(|| RwLock::new(HashMap::new()));
static WORKER_TX: OnceLock<mpsc::Sender<u64>> = OnceLock::new();

const MAX_CACHE_ENTRIES: usize = 8192;

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

/// Stores freshly evaluated accessories for `message_id` and requests a repaint
/// of Discord's window so edits/deletes appear immediately.
pub fn set_rendered(message_id: u64, mut items: Vec<RenderedAccessory>) {
   items.sort_by_key(|a| a.priority);
   let changed = if let Ok(mut map) = CACHE.write() {
      if map.len() > MAX_CACHE_ENTRIES {
         map.clear();
      }
      let prev_same = matches!(map.get(&message_id), Some(CacheEntry::Ready(old)) if *old == items);
      map.insert(message_id, CacheEntry::Ready(items));
      !prev_same
   } else {
      false
   };
   if changed {
      invalidate_discord_windows();
   }
}

fn ensure_worker() -> &'static mpsc::Sender<u64> {
   WORKER_TX.get_or_init(|| {
      let (tx, rx) = mpsc::channel::<u64>();
      let _ = std::thread::Builder::new()
         .name("sunbird-msg-acc".to_owned())
         .spawn(move || {
            while let Ok(id) = rx.recv() {
               // Coalesce burst requests for the same message ID.
               let mut batch = vec![id];
               while let Ok(more) = rx.try_recv() {
                  if !batch.contains(&more) {
                     batch.push(more);
                  }
               }
               let mut any_visible = false;
               for mid in batch {
                  if ACTIVE_COUNT.load(Ordering::Acquire) == 0 {
                     break;
                  }
                  let mut items = crate::render_desktop_accessories(mid);
                  items.sort_by_key(|a| a.priority);
                  let non_empty = !items.is_empty();
                  if let Ok(mut map) = CACHE.write() {
                     if map.len() > MAX_CACHE_ENTRIES {
                        map.clear();
                     }
                     map.insert(mid, CacheEntry::Ready(items));
                  }
                  if non_empty {
                     any_visible = true;
                  }
               }
               if any_visible {
                  invalidate_discord_windows();
               }
            }
         });
      tx
   })
}

fn lookup_cached(message_id: u64) -> Option<Vec<RenderedAccessory>> {
   if let Ok(map) = CACHE.read()
      && let Some(entry) = map.get(&message_id)
   {
      return match entry {
         CacheEntry::Pending => None,
         CacheEntry::Ready(items) if items.is_empty() => None,
         CacheEntry::Ready(items) => Some(items.clone()),
      };
   }
   let should_queue = if let Ok(mut map) = CACHE.write() {
      if map.contains_key(&message_id) {
         false
      } else {
         if map.len() > MAX_CACHE_ENTRIES {
            map.clear();
         }
         map.insert(message_id, CacheEntry::Pending);
         true
      }
   } else {
      false
   };
   if should_queue {
      let _ = ensure_worker().send(message_id);
   }
   None
}

/// Fast read-only tint lookup for `build_body_detour` (called inside `MessageRow::render`).
/// Cache misses are queued right afterward by `render_row_detour`, which triggers a repaint.
fn lookup_cached_tint(message_id: u64) -> Option<u32> {
   match CACHE.read().ok()?.get(&message_id)? {
      CacheEntry::Ready(items) => items.iter().rev().find_map(|item| item.tint),
      CacheEntry::Pending => None,
   }
}

/// Converts plugin ARGB (`0xAARRGGBB`, with `0x00RRGGBB` treated as opaque)
/// into Discord GPUI's packed `0xRRGGBBAA` `u32` token format.
const fn argb_to_rgba(argb: u32) -> u32 {
   let raw_a = (argb >> 24) & 0xFF;
   let a = if raw_a == 0 && argb != 0 { 0xFF } else { raw_a };
   ((argb & 0x00FF_FFFF) << 8) | a
}

// ---------------------------------------------------------------------------
// Win32 Repaint Trigger
// ---------------------------------------------------------------------------

#[link(name = "user32")]
unsafe extern "system" {
   fn EnumWindows(
      cb: unsafe extern "system" fn(isize, isize) -> i32,
      lparam: isize,
   ) -> i32;
   fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
   fn IsWindowVisible(hwnd: isize) -> i32;
   fn InvalidateRect(hwnd: isize, rect: *const c_void, erase: i32) -> i32;
}

unsafe extern "system" fn invalidate_own_window(hwnd: isize, lparam: isize) -> i32 {
   let want_pid = lparam as u32;
   let mut pid = 0u32;
   // SAFETY: Win32 window query + repaint request for this process's own visible windows.
   unsafe {
      GetWindowThreadProcessId(hwnd, &raw mut pid);
      if pid == want_pid && IsWindowVisible(hwnd) != 0 {
         InvalidateRect(hwnd, ptr::null(), 0);
      }
   }
   1
}

/// Invalidates all visible top-level windows in this process so GPUI repaints
/// updated message accessories on the next vsync.
pub fn invalidate_discord_windows() {
   let pid = std::process::id();
   // SAFETY: callback only queries PID/visibility and calls `InvalidateRect`.
   unsafe {
      EnumWindows(invalidate_own_window, pid as isize);
   }
}

// ---------------------------------------------------------------------------
// Anchor Locator & Inline Hook
// ---------------------------------------------------------------------------

fn find_lea_xrefs(image: &Image<'_>, target: usize) -> Vec<usize> {
   let mut hits = Vec::new();
   for seg in image.segments.iter().filter(|s| s.executable) {
      let bytes = seg.bytes.as_ref();
      for idx in 0..bytes.len().saturating_sub(6) {
         let rex = bytes[idx];
         if (rex != 0x48 && rex != 0x4C) || bytes[idx + 1] != 0x8D {
            continue;
         }
         if (bytes[idx + 2] & 0xC7) != 0x05 {
            continue;
         }
         let disp = i32::from_le_bytes([
            bytes[idx + 3],
            bytes[idx + 4],
            bytes[idx + 5],
            bytes[idx + 6],
         ]);
         let next_rip = seg.addr + idx + 7;
         if next_rip.wrapping_add_signed(disp as isize) == target {
            hits.push(seg.addr + idx);
         }
      }
   }
   hits
}

fn find_call_parents(image: &Image<'_>, callee: usize) -> HashSet<usize> {
   let mut parents = HashSet::new();
   for seg in image.segments.iter().filter(|s| s.executable) {
      let bytes = seg.bytes.as_ref();
      for idx in 0..bytes.len().saturating_sub(4) {
         if bytes[idx] != 0xE8 {
            continue;
         }
         let disp = i32::from_le_bytes([
            bytes[idx + 1],
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
         ]);
         let next_rip = seg.addr + idx + 5;
         if next_rip.wrapping_add_signed(disp as isize) == callee
            && let Some((parent_start, _)) = image.function_bounds(seg.addr + idx)
         {
            parents.insert(parent_start);
         }
      }
   }
   parents
}

fn decode_msg_id_offset(image: &Image<'_>, lea_site: usize) -> Option<usize> {
   let window_start = lea_site.saturating_sub(0x60);
   let bytes = image.read(window_start, lea_site - window_start)?;
   for at in MSG_ID_LEA.find_in(bytes).into_iter().rev() {
      if let Some(d) = bytes.get(at + 3..at + 7) {
         let disp = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
         if (0x100..=0x2000).contains(&disp) {
            return Some(disp);
         }
      }
   }
   None
}

/// Locates and hooks Discord's GPUI `MessageRow::render` from `DllMain`.
pub fn install(image: &Image<'static>) {
   if !desktop_gpui::supports_accessories() {
      log::warn!("desktop accessories: GPUI text/push primitives unavailable, skipping");
      return;
   }

   let anchors = image.find_bytes(b"\x10message-content-\xc0\x00");
   let [anchor] = anchors[..] else {
      log::warn!(
         "desktop accessories: message-content template matched {} times, skipping",
         anchors.len()
      );
      return;
   };

   let leas = find_lea_xrefs(image, anchor);
   if leas.is_empty() {
      log::warn!("desktop accessories: no xrefs to message-content template");
      return;
   }

   let mut helper_funcs = Vec::new();
   let mut msg_id_off: Option<usize> = None;
   for &lea in &leas {
      if msg_id_off.is_none() {
         msg_id_off = decode_msg_id_offset(image, lea);
      }
      if let Some((fn_start, _)) = image.function_bounds(lea)
         && !helper_funcs.contains(&fn_start)
      {
         helper_funcs.push(fn_start);
      }
   }

   let mut common: Option<HashSet<usize>> = None;
   for &helper in &helper_funcs {
      let parents = find_call_parents(image, helper);
      if parents.is_empty() {
         continue;
      }
      common = Some(match common {
         None => parents,
         Some(prev) => prev.intersection(&parents).copied().collect(),
      });
   }

   let candidates: Vec<usize> = common.into_iter().flatten().collect();
   let [render_row] = candidates[..] else {
      log::warn!(
         "desktop accessories: expected 1 common MessageRow renderer, found {}",
         candidates.len()
      );
      return;
   };

   let offset = msg_id_off.unwrap_or(DEFAULT_MSG_ID_OFFSET);
   MSG_ID_OFFSET.store(offset, Ordering::Release);

   // SAFETY: runs from `DllMain` before Discord threads execute; `render_row_detour`
   // matches `MessageRow::render`'s exact 11-argument sret ABI (4 reg + 7 stack u64s).
   let trampoline = match unsafe {
      image.hook_entry(
         render_row,
         ROW_PROLOGUE,
         ROW_PROLOGUE_LEN,
         render_row_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop accessories: MessageRow hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the 17-byte prologue and jumps to `render_row + 17`.
   let orig = unsafe { std::mem::transmute::<usize, RenderRowFn>(trampoline) };
   let _ = ORIG_RENDER_ROW.set(orig);
   log::info!(
      "desktop accessories: hooked MessageRow render @ {render_row:#x} (msg_id +{offset:#x})"
   );

   install_body_tint(image, render_row);
}

fn decode_body_msg_id_offset(image: &Image<'_>, body_fn: usize) -> Option<usize> {
   let bytes = image.read(body_fn, 0x100)?;
   for at in BODY_MSG_ID_MOV.find_in(bytes) {
      if let Some(d) = bytes.get(at + 3..at + 7) {
         let disp = u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize;
         if (0x100..=0x800).contains(&disp) {
            return Some(disp);
         }
      }
   }
   None
}

fn decode_row_colors_offset(image: &Image<'_>, body_fn: usize) -> Option<usize> {
   let bytes = image.read(body_fn, 0x120)?;
   let imm = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize);
   for at in ROW_COLORS_TRIPLE.find_in(bytes) {
      let (Some(off_a), Some(off_c), Some(off_d)) = (imm(at + 1), imm(at + 6), imm(at + 11)) else {
         continue;
      };
      // Consecutive `RowColors` fields `(+0x0c, +0x08, +0x04)` -> base is `off_d - 4`.
      if off_a == off_c + 4 && off_c == off_d + 4 && (0x100..=0x400).contains(&off_d) {
         return Some(off_d - 4);
      }
   }
   None
}

/// Hooks Discord's inner message body builder (`build_message_body`, called once
/// per row from `MessageRow::render`) so message tints also recolor the rendered
/// message body text natively via `config.row_colors` (`0xRRGGBBAA`).
fn install_body_tint(image: &Image<'static>, render_row: usize) {
   let anchors = image.find_bytes(b"\x0fmessage-edited-\xc0\x00");
   let [anchor] = anchors[..] else {
      log::warn!(
         "desktop accessories: message-edited template matched {} times, skipping body tint",
         anchors.len()
      );
      return;
   };

   let mut candidates = Vec::new();
   for lea in find_lea_xrefs(image, anchor) {
      let Some((fn_start, _)) = image.function_bounds(lea) else {
         continue;
      };
      if candidates.contains(&fn_start) {
         continue;
      }
      let Some(head) = image.read(fn_start, ROW_PROLOGUE_LEN) else {
         continue;
      };
      if ROW_PROLOGUE.find_in(head).first().copied() != Some(0) {
         continue;
      }
      if find_call_parents(image, fn_start).contains(&render_row) {
         candidates.push(fn_start);
      }
   }

   let [body_fn] = candidates[..] else {
      log::warn!(
         "desktop accessories: expected 1 message body builder, found {}",
         candidates.len()
      );
      return;
   };

   // A guessed offset would write into the wrong stack slot, so a build
   // whose layout does not decode goes without body tints.
   let (Some(msg_id_off), Some(colors_off)) = (
      decode_body_msg_id_offset(image, body_fn),
      decode_row_colors_offset(image, body_fn),
   ) else {
      log::warn!("desktop accessories: message body layout did not decode, skipping body tint");
      return;
   };
   BODY_MSG_ID_OFFSET.store(msg_id_off, Ordering::Release);
   ROW_COLORS_OFFSET.store(colors_off, Ordering::Release);

   // SAFETY: runs from `DllMain` before Discord threads execute; `build_body_detour`
   // matches `build_message_body`'s verified 6-argument ABI (4 reg + 2 stack args).
   let trampoline = match unsafe {
      image.hook_entry(
         body_fn,
         ROW_PROLOGUE,
         ROW_PROLOGUE_LEN,
         build_body_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop accessories: message body tint hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the 17-byte prologue and jumps to `body_fn + 17`.
   let orig = unsafe { std::mem::transmute::<usize, BuildMessageBodyFn>(trampoline) };
   let _ = ORIG_BUILD_BODY.set(orig);
   log::info!(
      "desktop accessories: hooked message body tint @ {body_fn:#x} \
       (msg_id +{msg_id_off:#x}, row_colors +{colors_off:#x})"
   );
}

unsafe extern "C" fn build_body_detour(
   msg: *const c_void,
   config: *mut c_void,
   a2: u64,
   a3: u64,
   s0: u64,
   s1: u64,
) -> *mut c_void {
   let Some(orig) = ORIG_BUILD_BODY.get().copied() else {
      return ptr::null_mut();
   };
   if ACTIVE_COUNT.load(Ordering::Relaxed) == 0 || msg.is_null() || config.is_null() {
      // SAFETY: forwards all arguments verbatim to the original body builder.
      return unsafe { orig(msg, config, a2, a3, s0, s1) };
   }

   let msg_off = BODY_MSG_ID_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `msg` is Discord's live `&MessageViewData` and `msg_off` was decoded
   // from `build_message_body`'s own `message_id` read.
   let message_id = unsafe { ptr::read_unaligned(msg.cast::<u8>().add(msg_off).cast::<u64>()) };
   let Some(argb) = (message_id != 0)
      .then(|| lookup_cached_tint(message_id))
      .flatten()
   else {
      // SAFETY: no tint for this message; forward verbatim.
      return unsafe { orig(msg, config, a2, a3, s0, s1) };
   };

   let rgba = argb_to_rgba(argb);
   let base_off = ROW_COLORS_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `config` lives on `MessageRow::render`'s thread-local stack frame;
   // we temporarily override the two POD `u32` `0xRRGGBBAA` body text color slots
   // (`row_colors + 0x00` default and `+ 0x34` pending/muted) and restore them on return.
   unsafe {
      let default_ptr = config.cast::<u8>().add(base_off).cast::<u32>();
      let muted_ptr = config
         .cast::<u8>()
         .add(base_off + ROW_COLORS_MUTED_DELTA)
         .cast::<u32>();
      let saved_default = ptr::read(default_ptr);
      let saved_muted = ptr::read(muted_ptr);
      ptr::write(default_ptr, rgba);
      ptr::write(muted_ptr, rgba);
      let out = orig(msg, config, a2, a3, s0, s1);
      ptr::write(default_ptr, saved_default);
      ptr::write(muted_ptr, saved_muted);
      out
   }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn render_row_detour(
   sret: *mut RawAnyElement,
   row: *const c_void,
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
   let Some(orig) = ORIG_RENDER_ROW.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original `MessageRow::render`.
   let out = unsafe { orig(sret, row, a2, a3, s0, s1, s2, s3, s4, s5, s6) };

   if ACTIVE_COUNT.load(Ordering::Relaxed) == 0 || row.is_null() || sret.is_null() {
      return out;
   }

   let offset = MSG_ID_OFFSET.load(Ordering::Relaxed);
   // SAFETY: `row` is Discord's live `&MessageRow` and `offset` was decoded from
   // its `message-content-{id}` formatting read.
   let message_id = unsafe { ptr::read(row.cast::<u8>().add(offset).cast::<u64>()) };
   if message_id == 0 {
      return out;
   }

   let Some(items) = lookup_cached(message_id) else {
      return out;
   };

   let mut lines = Vec::new();
   let mut tint: Option<u32> = None;
   for item in &items {
      if item.tint.is_some() {
         tint = item.tint;
      }
      if !item.text.is_empty() {
         lines.push((item.text.clone(), item.color));
      }
   }
   if lines.is_empty() && tint.is_none() {
      return out;
   }

   let _ = std::panic::catch_unwind(|| {
      desktop_gpui::wrap_with_accessories(sret, &lines, tint);
   });
   out
}

// ---------------------------------------------------------------------------
// Same-Client Optimistic Delete Hook
// ---------------------------------------------------------------------------

fn calls_in_range(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   let Some(bytes) = image.read(start, end.saturating_sub(start)) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   for idx in 0..bytes.len().saturating_sub(4) {
      if bytes[idx] != 0xE8 {
         continue;
      }
      let disp = i32::from_le_bytes([
         bytes[idx + 1],
         bytes[idx + 2],
         bytes[idx + 3],
         bytes[idx + 4],
      ]);
      let target = (start + idx + 5).wrapping_add_signed(disp as isize);
      if image.is_code(target) && !out.contains(&target) {
         out.push(target);
      }
   }
   out
}

fn find_call_parents_multi(
   image: &Image<'_>,
   targets: &HashSet<usize>,
) -> HashMap<usize, HashSet<usize>> {
   let mut out: HashMap<usize, HashSet<usize>> = HashMap::new();
   for seg in image.segments.iter().filter(|s| s.executable) {
      let bytes = seg.bytes.as_ref();
      for idx in 0..bytes.len().saturating_sub(4) {
         if bytes[idx] != 0xE8 {
            continue;
         }
         let disp = i32::from_le_bytes([
            bytes[idx + 1],
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
         ]);
         let next_rip = seg.addr + idx + 5;
         let callee = next_rip.wrapping_add_signed(disp as isize);
         if targets.contains(&callee)
            && let Some((parent_start, _)) = image.function_bounds(seg.addr + idx)
         {
            out.entry(callee).or_default().insert(parent_start);
         }
      }
   }
   out
}

/// Locates and hooks Desktop Phoenix's same-client optimistic message delete
/// (`Store::delete_message`), so deleting a message from this client respects
/// `MESSAGE_DELETE` `before` hooks (such as `message-logger`).
pub fn install_optimistic_delete(image: &Image<'static>) {
   let Some(del_fn) = crate::gateway::original_handler("MESSAGE_DELETE") else {
      log::warn!("desktop accessories: MESSAGE_DELETE handler not found, skipping optimistic delete hook");
      return;
   };
   let Some((del_start, del_end)) = image.function_bounds(del_fn) else {
      log::warn!("desktop accessories: no .pdata bounds for MESSAGE_DELETE {del_fn:#x}");
      return;
   };

   let del_callees: HashSet<usize> = calls_in_range(image, del_start, del_end).into_iter().collect();
   let parents_by_callee = find_call_parents_multi(image, &del_callees);
   let mut candidates = Vec::new();
   for parents in parents_by_callee.values() {
      if parents.len() != 2 || !parents.contains(&del_start) {
         continue;
      }
      let Some(&other) = parents.iter().find(|&&p| p != del_start) else {
         continue;
      };
      if candidates.contains(&other) {
         continue;
      }
      let Some(head) = image.read(other, OPT_DEL_PROLOGUE_LEN) else {
         continue;
      };
      if OPT_DEL_PROLOGUE.find_in(head).first().copied() != Some(0) {
         continue;
      }
      let Some(body_head) = image.read(other, 0x80) else {
         continue;
      };
      let has_lock = body_head
         .windows(4)
         .any(|w| w == [0xF0, 0x48, 0x0F, 0xB1]);
      if !has_lock {
         continue;
      }
      let Some((o_start, o_end)) = image.function_bounds(other) else {
         continue;
      };
      let shared = calls_in_range(image, o_start, o_end)
         .into_iter()
         .filter(|c| del_callees.contains(c))
         .count();
      // Shares the in-memory map delete, secondary index delete, and cleanup helpers
      // with MESSAGE_DELETE (6 shared callees vs <= 2 for unrelated callers).
      if shared >= 4 {
         candidates.push(other);
      }
   }

   let [opt_del] = candidates[..] else {
      log::warn!(
         "desktop accessories: expected 1 optimistic delete candidate, found {}",
         candidates.len()
      );
      return;
   };

   // SAFETY: runs from `DllMain` before Discord threads execute; `optimistic_delete_detour`
   // matches `Store::delete_message`'s exact `(store, args, meta) -> bool` ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         opt_del,
         OPT_DEL_PROLOGUE,
         OPT_DEL_PROLOGUE_LEN,
         optimistic_delete_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop accessories: optimistic delete hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays the 19-byte prologue and jumps to `opt_del + 19`.
   let orig = unsafe { std::mem::transmute::<usize, OptimisticDeleteFn>(trampoline) };
   let _ = ORIG_OPT_DELETE.set(orig);
   log::info!("desktop accessories: hooked same-client optimistic delete @ {opt_del:#x}");
}

unsafe extern "C" fn optimistic_delete_detour(
   store: *mut c_void,
   args: *const *const u64,
   meta: *const c_void,
) -> u8 {
   if !args.is_null() {
      // SAFETY: `args` points to `[&channel_id, &message_id]` in `Command::DeleteMessage`.
      let (ch_ptr, msg_ptr) = unsafe { (ptr::read(args), ptr::read(args.add(1))) };
      if !ch_ptr.is_null() && !msg_ptr.is_null() {
         let (channel_id, message_id) = unsafe { (ptr::read(ch_ptr), ptr::read(msg_ptr)) };
         if channel_id != 0
            && message_id != 0
            && std::panic::catch_unwind(|| {
               crate::gateway::check_optimistic_delete(channel_id, message_id)
            })
            .unwrap_or(false)
         {
            return 0;
         }
      }
   }
   let Some(orig) = ORIG_OPT_DELETE.get().copied() else {
      return 0;
   };
   // SAFETY: forwards all arguments verbatim to the original optimistic delete.
   unsafe { orig(store, args, meta) }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn argb_to_rgba_preserves_channels_and_alpha() {
      assert_eq!(argb_to_rgba(0xfff2_3f43), 0xf23f_43ff);
      assert_eq!(argb_to_rgba(0x8058_65f2), 0x5865_f280);
      assert_eq!(argb_to_rgba(0x00f2_3f43), 0xf23f_43ff);
      assert_eq!(argb_to_rgba(0), 0);
   }
}
