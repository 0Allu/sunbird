//! Injects a **Sunbird** tab directly into Desktop Phoenix's native GPUI User
//! Settings sidebar and synchronizes an embedded WebView2 pane when selected.
//!
//! All three patches are build-independent signature/anchor lookups run from
//! [`crate::prepare`] inside `DllMain` before Discord initializes:
//! 1. Relabel settings page `24` (`Other -> Phoenix`, right above `Log Out`) to
//!    `Sunbird` by replacing the unique 22-byte `.rdata` string
//!    `PhoenixDESKTOP_PHOENIX` in-place with `SunbirdDESKTOP_SUNBIRD`.
//! 2. Unconditionally show page `24` in `is_page_visible` by tightening the
//!    `cmp r8d, 0xf` range guard (`pages 9..=24`) to `0x0e` (`pages 9..=23`),
//!    so page `24` falls through to `return true`.
//! 3. Inline-hook `host::section` (located via the unique `account-loading`
//!    string xref and `.pdata` function bounds) to read the active
//!    `SettingsPage` discriminant (`u8`) and show/hide the embedded Sunbird
//!    WebView2 pane inside Discord's User Settings content column.

use std::{
   ffi::c_void,
   sync::{
      OnceLock,
      atomic::{
         AtomicUsize,
         Ordering,
      },
   },
};

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

const SUNBIRD_PAGE: u8 = 24;
const DEFAULT_PAGE_OFFSET: usize = 0x61f;

/// `is_page_visible(page: SettingsPage) -> bool`:
/// `movzx ecx, cl; mov al, 1; lea r8d, [rcx-9]; cmp r8d, 0xf; ja ...`
const VISIBILITY_GUARD: Pattern =
   Pattern::new("0F B6 C9 B0 01 44 8D 41 F7 41 83 F8 0F 77");
const VISIBILITY_LIMIT_OFFSET: usize = 12;

/// `host::section` prologue: 8 pushes + `sub rsp, imm32` (19 bytes, no
/// RIP-relative operands).
const SECTION_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 48 81 EC ?? ?? ?? ??");
const SECTION_PROLOGUE_LEN: usize = 19;

/// `mov bl, byte ptr [r15 + disp32]` loading `self.active_page` near the top
/// of `host::section`.
const ACTIVE_PAGE_LOAD: Pattern = Pattern::new("41 8A 9F ?? ?? ?? ??");

type SectionFn = unsafe extern "C" fn(
   sret: *mut c_void,
   host_state: *mut c_void,
   window: *mut c_void,
   cx: *mut c_void,
   arg5: u64,
   arg6: u64,
) -> *mut c_void;

static ORIG_SECTION: OnceLock<SectionFn> = OnceLock::new();
static PAGE_OFFSET: AtomicUsize = AtomicUsize::new(DEFAULT_PAGE_OFFSET);

/// Installs the Desktop User Settings sidebar label, visibility patch, and
/// active-page observer hook.
pub fn install(image: &Image<'static>) {
   patch_sidebar_label(image);
   patch_page_visibility(image);
   hook_section_render(image);
}

fn patch_sidebar_label(image: &Image<'static>) {
   const OLD: &[u8] = b"PhoenixDESKTOP_PHOENIX";
   const NEW: &[u8] = b"SunbirdDESKTOP_SUNBIRD";
   const _: () = assert!(OLD.len() == NEW.len());

   let found = image.find_bytes(OLD);
   let [addr] = found[..] else {
      log::warn!(
         "desktop settings: label anchor matched {} times, leaving sidebar label",
         found.len()
      );
      return;
   };
   // SAFETY: exact-length `.rdata` string patch executed from `DllMain` before
   // Discord threads run.
   match unsafe { image.write_code(addr, NEW) } {
      Ok(()) => log::info!("desktop settings: relabeled page 24 to Sunbird"),
      Err(err) => log::warn!("desktop settings: relabel failed: {err}"),
   }
}

fn patch_page_visibility(image: &Image<'static>) {
   let found = image.scan(VISIBILITY_GUARD);
   let [addr] = found[..] else {
      log::warn!(
         "desktop settings: visibility guard matched {} times, leaving it",
         found.len()
      );
      return;
   };
   // Tightening `cmp r8d, 0xf` -> `0x0e` excludes page 24 (`24 - 9 = 15`) from
   // the gated switch so `is_page_visible(24)` returns `true` unconditionally.
   // SAFETY: single-byte immediate patch in `.text` executed from `DllMain`.
   match unsafe { image.write_code(addr + VISIBILITY_LIMIT_OFFSET, &[0x0E]) } {
      Ok(()) => log::info!("desktop settings: page 24 visibility unlocked"),
      Err(err) => log::warn!("desktop settings: visibility patch failed: {err}"),
   }
}

/// Finds every RIP-relative `lea reg, [rip + disp32]` in executable segments
/// whose target equals `target`.
fn find_lea_xrefs(image: &Image<'_>, target: usize) -> Vec<usize> {
   let mut hits = Vec::new();
   for seg in image.segments.iter().filter(|s| s.executable) {
      let bytes = seg.bytes.as_ref();
      if bytes.len() < 7 {
         continue;
      }
      for idx in 1..=bytes.len() - 6 {
         if bytes[idx] != 0x8D {
            continue;
         }
         if (bytes[idx - 1] & 0xF0) != 0x40 || (bytes[idx + 1] & 0xC7) != 0x05 {
            continue;
         }
         let disp = i32::from_le_bytes([
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
            bytes[idx + 5],
         ]);
         let next_rip = seg.addr + idx + 6;
         if next_rip.wrapping_add_signed(disp as isize) == target {
            hits.push(seg.addr + idx - 1);
         }
      }
   }
   hits
}

fn decode_page_offset(image: &Image<'_>, fn_start: usize) -> usize {
   let Some(head) = image.read(fn_start, 0x160) else {
      return DEFAULT_PAGE_OFFSET;
   };
   for at in ACTIVE_PAGE_LOAD.find_in(head) {
      if let Some(bytes) = head.get(at + 3..at + 7) {
         let disp = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
         if (0x100..=0x2000).contains(&disp) {
            return disp;
         }
      }
   }
   DEFAULT_PAGE_OFFSET
}

fn hook_section_render(image: &Image<'static>) {
   let strings = image.find_bytes(b"account-loading");
   let [str_addr] = strings[..] else {
      log::warn!(
         "desktop settings: account-loading matched {} times, skipping section hook",
         strings.len()
      );
      return;
   };
   let xrefs = find_lea_xrefs(image, str_addr);
   let [lea_addr] = xrefs[..] else {
      log::warn!(
         "desktop settings: account-loading xrefs matched {} times, skipping section hook",
         xrefs.len()
      );
      return;
   };
   let Some((fn_start, _fn_end)) = image.function_bounds(lea_addr) else {
      log::warn!("desktop settings: no .pdata bounds for section xref {lea_addr:#x}");
      return;
   };

   let offset = decode_page_offset(image, fn_start);
   PAGE_OFFSET.store(offset, Ordering::Release);

   // SAFETY: runs from `DllMain` before Discord threads execute; `section_detour`
   // matches `host::section`'s exact 6-argument sret ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         fn_start,
         SECTION_PROLOGUE,
         SECTION_PROLOGUE_LEN,
         section_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop settings: section hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays `fn_start`'s prologue and jumps to `fn_start + 19`.
   let orig = unsafe { std::mem::transmute::<usize, SectionFn>(trampoline) };
   let _ = ORIG_SECTION.set(orig);
   log::info!(
      "desktop settings: hooked section render @ {fn_start:#x} (page offset {offset:#x})"
   );
}

unsafe extern "C" fn section_detour(
   sret: *mut c_void,
   host_state: *mut c_void,
   window: *mut c_void,
   cx: *mut c_void,
   arg5: u64,
   arg6: u64,
) -> *mut c_void {
   if !host_state.is_null() {
      let _ = std::panic::catch_unwind(|| {
         let offset = PAGE_OFFSET.load(Ordering::Acquire);
         // SAFETY: `host_state` is Discord's live `&mut HostState` for this render call.
         let page = unsafe { *host_state.cast::<u8>().add(offset) };
         phoenix_ui_desktop::set_embedded_settings_visible(page == SUNBIRD_PAGE);
      });
   }
   let Some(orig) = ORIG_SECTION.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original `host::section`.
   unsafe { orig(sret, host_state, window, cx, arg5, arg6) }
}
