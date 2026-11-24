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

#[cfg(windows)]
pub fn prepare(image: &phoenix_hook::image::Image<'_>) {
   imp::prepare(image);
}

#[cfg(all(target_os = "android", target_arch = "aarch64"))]
mod imp {
   use std::{
      ffi::c_void,
      ptr,
      sync::OnceLock,
   };

   use phoenix_hook::image::Image;
   use phoenix_sigs::StaffField as Field;

   const STAFF: u32 = 1;

   static LOCATED: OnceLock<Option<Field>> = OnceLock::new();

   /// Finds the `flag_override` field off the main thread, since the scan
   /// reads most of the host library.
   pub(super) fn start(image: &'static Image<'static>) {
      std::thread::spawn(|| {
         LOCATED.get_or_init(|| locate(image));
      });
   }

   fn locate(image: &Image<'_>) -> Option<Field> {
      phoenix_sigs::mobile::staff_aarch64(image)
         .inspect_err(|err| log::warn!("staff: {err}, leaving flag_override as is"))
         .ok()
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

#[cfg(all(target_os = "android", target_arch = "arm"))]
mod imp {
   use std::{
      ffi::{
         c_int,
         c_void,
      },
      ptr::NonNull,
      sync::OnceLock,
   };

   use phoenix_hook::{
      Hook,
      Inline,
      image::Image,
   };

   static HOOK: OnceLock<Option<Inline>> = OnceLock::new();

   /// Hooks the check from the loader's constructor, before any thread can be
   /// inside it, because the hook rewrites its first 8 or 10 bytes.
   pub(super) fn start(image: &Image<'_>) {
      HOOK.get_or_init(|| hook(image));
   }

   fn hook(image: &Image<'_>) -> Option<Inline> {
      let target = phoenix_sigs::mobile::staff_arm(image)
         .inspect_err(|err| log::warn!("staff: {err}"))
         .ok()?;
      // ShadowHook picks ARM or Thumb by bit 0, and the check is Thumb.
      let target = NonNull::new((target | 1) as *mut c_void)?;
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

   extern "C" fn is_staff(store: *const c_void, force: c_int) -> bool {
      if super::enabled() {
         return true;
      }
      let Some(hook) = HOOK.wait() else {
         return false;
      };
      // SAFETY: the trampoline runs the original check, which has this ABI.
      let original: extern "C" fn(*const c_void, c_int) -> bool =
         unsafe { std::mem::transmute(hook.original().as_ptr()) };
      original(store, force)
   }
}

#[cfg(all(windows, target_arch = "x86_64"))]
mod imp {
   use std::{
      ffi::c_void,
      ptr,
      sync::{
         OnceLock,
         atomic::{
            AtomicBool,
            Ordering,
         },
      },
   };

   use phoenix_hook::image::Image;
   use phoenix_sigs::StaffField as Field;

   const STAFF: u64 = 1;

   static LOCATED: OnceLock<Option<Field>> = OnceLock::new();
   static LOGGED: AtomicBool = AtomicBool::new(false);

   /// Scans `Discord.exe` synchronously during `DllMain` so `flag_override` is
   /// ready before the first gateway event fires.
   pub(super) fn prepare(image: &Image<'_>) {
      LOCATED.get_or_init(|| locate(image));
   }

   pub(super) fn start(image: &'static Image<'static>) {
      LOCATED.get_or_init(|| locate(image));
   }

   fn locate(image: &Image<'_>) -> Option<Field> {
      let field = phoenix_sigs::desktop::staff(image)
         .inspect_err(|err| log::warn!("staff: {err}, leaving flag_override as is"))
         .ok()?;
      log::info!("staff: located flag_override at tag={:#x} value={:#x}", field.tag, field.value);
      Some(field)
   }

   /// # Safety
   ///
   /// `state` is the `&StoreState` a gateway handler was called with, and the
   /// caller runs inside that handler.
   pub unsafe fn apply(state: *mut c_void) {
      let Some(Some(field)) = LOCATED.get() else {
         return;
      };
      if state.is_null() {
         return;
      }
      // SAFETY: the flag read loads `tag` as `u32` (`1` == `Some`) and `value`
      // as `u64` from `&StoreState`, which every gateway handler receives first.
      unsafe {
         let tag = state.byte_add(field.tag).cast::<u32>();
         let value = state.byte_add(field.value).cast::<u64>();
         let had = ptr::read(tag) == 1;
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
         ptr::write(tag, u32::from(wanted != 0));
      }
      if super::enabled() && !LOGGED.swap(true, Ordering::AcqRel) {
         log::info!("staff: unlocked (flag_override bit 0 set)");
      }
   }
}

#[cfg(not(any(
   all(target_os = "android", any(target_arch = "aarch64", target_arch = "arm")),
   all(windows, target_arch = "x86_64")
)))]
mod imp {
   use std::ffi::c_void;

   use phoenix_hook::image::Image;

   #[cfg(windows)]
   pub(super) fn prepare(_image: &Image<'_>) {}

   pub(super) fn start(_image: &'static Image<'static>) {}

   /// # Safety
   ///
   /// No-op on targets without a staff pattern yet.
   pub unsafe fn apply(_state: *mut c_void) {}
}

#[cfg(not(all(target_os = "android", target_arch = "arm")))] pub use imp::apply;
