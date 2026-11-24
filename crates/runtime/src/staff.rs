//! Phoenix's Staff Settings page reads a local flag computed as
//! `(user.flags | flag_override) & 1`, where `flag_override` is Phoenix's own
//! client-side field. Setting its low bit shows the page.
//!
//! On the 64-bit targets the read is inlined in about 30 places off the gateway
//! handler's first argument, so the gateway detour writes `flag_override` in
//! that state while the reducer holds it. On armeabi-v7a the read is outlined
//! into one check function, which is hooked to answer staff while the unlock is
//! on.

use std::sync::atomic::{
   AtomicBool,
   Ordering,
};

static UNLOCKED: AtomicBool = AtomicBool::new(true);

#[must_use]
pub fn enabled() -> bool {
   UNLOCKED.load(Ordering::Acquire)
}

pub fn set_enabled(on: bool) {
   UNLOCKED.store(on, Ordering::Release);
}

pub fn start(on: bool, image: &'static phoenix_hook::image::Image<'static>) {
   set_enabled(on);
   imp::start(image);
}

#[cfg(not(target_arch = "arm"))]
mod imp {
   use std::{
      ffi::c_void,
      ptr,
      sync::OnceLock,
   };

   use phoenix_hook::{
      image::Image,
      pattern::Pattern,
   };

   /// The tail of the flag read: `add x8, x8, #0x8000` then the three
   /// `flag_override` loads and the `csel` that ORs in the low bit. The offsets
   /// in the first two loads are wildcarded and read back from the match, so
   /// the write targets whatever field this build reads. 6 copies on 356207 and
   /// 357200, where every field offset moved.
   const STAFF_CHECK: Pattern = Pattern::new(
      "08 21 40 91 09 ?? ?? F9 3F 09 00 F1 61 00 00 54 F3 03 1F 2A ?? 00 00 14 0A ?? ?? B9 0B ?? \
       ?? B9 0C ?? ?? B9 5F 01 00 71 6A 11 9F 1A",
   );
   const ADD_BASE: usize = 0;
   const TAG_LOAD: usize = 24;
   const VALUE_LOAD: usize = 28;
   const STAFF: u32 = 1;

   static LOCATED: OnceLock<Option<Field>> = OnceLock::new();

   /// Finds the `flag_override` field off the main thread, since the scan
   /// reads most of the host library.
   pub(super) fn start(image: &'static Image<'static>) {
      std::thread::spawn(|| {
         LOCATED.get_or_init(|| locate(image));
      });
   }

   /// `flag_override: Option<u32>` in the store state, as byte offsets of its
   /// tag and value decoded from the flag read.
   #[derive(Clone, Copy)]
   struct Field {
      tag:   usize,
      value: usize,
   }

   /// The `flag_override` field decoded from the flag read, when every match
   /// agrees on the offsets.
   fn locate(image: &Image<'_>) -> Option<Field> {
      let mut field: Option<Field> = None;
      for base in image.scan(STAFF_CHECK) {
         let add = image.read_u32(base + ADD_BASE)?;
         let tag = image.read_u32(base + TAG_LOAD)?;
         let value = image.read_u32(base + VALUE_LOAD)?;
         let found = Field {
            tag:   add_offset(add) + ldr_offset(tag),
            value: add_offset(add) + ldr_offset(value),
         };
         if field.is_some_and(|seen| seen.tag != found.tag || seen.value != found.value) {
            log::warn!("staff: flag reads disagree on flag_override, leaving it as is");
            return None;
         }
         field = Some(found);
      }
      if field.is_none() {
         log::warn!("staff: flag read not found, leaving flag_override as is");
      }
      field
   }

   /// The byte offset an `add xd, xn, #imm{, lsl #12}` adds.
   const fn add_offset(word: u32) -> usize {
      let imm = ((word >> 10) & 0xFFF) as usize;
      imm << if (word >> 22) & 1 == 1 { 12 } else { 0 }
   }

   /// The byte offset a 32-bit `ldr wt, [xn, #imm]` loads from, scaled by 4.
   const fn ldr_offset(word: u32) -> usize {
      (((word >> 10) & 0xFFF) as usize) << 2
   }

   /// # Safety
   ///
   /// `state` is the store state a gateway handler was called with, and the
   /// caller runs inside that handler.
   pub unsafe fn apply(state: *mut c_void) {
      let Some(Some(field)) = LOCATED.get() else {
         return;
      };
      // SAFETY: the flag read loads both fields from this state as u32s.
      unsafe {
         let tag = state.byte_add(field.tag).cast::<u32>();
         let value = state.byte_add(field.value).cast::<u32>();
         let had = ptr::read(tag) != 0;
         let current = if had { ptr::read(value) } else { 0 };
         let wanted = if super::enabled() {
            current | STAFF
         } else {
            current & !STAFF
         };
         if wanted == current {
            return;
         }
         ptr::write(value, wanted);
         ptr::write(tag, u32::from(wanted != 0 || had));
      }
   }
}

#[cfg(target_arch = "arm")]
mod imp {
   use std::{
      ffi::c_void,
      ptr::NonNull,
      sync::OnceLock,
   };

   use phoenix_hook::{
      Hook,
      Inline,
      image::Image,
      pattern::Pattern,
   };

   /// The start of Phoenix's staff check, `bool f(&Store, bool force)`, which
   /// answers true when forced, then reads `(flags | flag_override) & 1` off
   /// the store. 19 callers on 356207 gate the Staff Settings section and the
   /// rest of the staff UI. The store offsets are wildcarded.
   const IS_STAFF: Pattern =
      Pattern::new("10 B5 84 B0 39 B9 03 46 00 68 ?? F6 ?? ?? 40 58 90 F8 ?? ?? 08 B1 01 24");

   static HOOK: OnceLock<Option<Inline>> = OnceLock::new();

   /// Hooks the check from the loader's constructor, before any thread can be
   /// inside it, because the hook rewrites its first 8 or 10 bytes.
   pub(super) fn start(image: &Image<'_>) {
      HOOK.get_or_init(|| hook(image));
   }

   fn hook(image: &Image<'_>) -> Option<Inline> {
      let found = image.scan(IS_STAFF);
      let [target] = found.as_slice() else {
         log::warn!("staff: {} matches for the staff check", found.len());
         return None;
      };
      // ShadowHook picks ARM or Thumb by bit 0, and the check is Thumb.
      let target = NonNull::new((*target | 1) as *mut c_void)?;
      let detour = NonNull::new(is_staff as *mut c_void)?;
      // SAFETY: the pattern matches the start of the check, and the detour has
      // its ABI.
      match unsafe { Inline::attach(target, detour) } {
         Ok(hook) => Some(hook),
         Err(err) => {
            log::warn!("staff: {err}");
            None
         },
      }
   }

   extern "C" fn is_staff(store: *const c_void, force: bool) -> bool {
      if super::enabled() {
         return true;
      }
      let Some(hook) = HOOK.wait() else {
         return false;
      };
      // SAFETY: the trampoline runs the original check, which has this ABI.
      let original: extern "C" fn(*const c_void, bool) -> bool =
         unsafe { std::mem::transmute(hook.original().as_ptr()) };
      original(store, force)
   }

   /// The hook answers every read, so the gateway has nothing to write.
   ///
   /// # Safety
   ///
   /// None, the signature matches the 64-bit `apply`.
   pub unsafe fn apply(_state: *mut c_void) {}
}

pub use imp::apply;
