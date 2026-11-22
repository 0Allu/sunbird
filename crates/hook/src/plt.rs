use std::{
   ffi::{
      CStr,
      c_char,
      c_int,
      c_void,
   },
   ptr::{
      self,
      NonNull,
   },
   sync::{
      OnceLock,
      atomic::{
         AtomicPtr,
         Ordering,
      },
   },
};

use crate::Error;

#[allow(
   dead_code,
   non_camel_case_types,
   non_upper_case_globals,
   clippy::all,
   clippy::pedantic
)]
mod sys {
   include!(concat!(env!("OUT_DIR"), "/bytehook.rs"));
}

/// A GOT rewrite of one imported symbol in one caller library, through
/// `ByteHook`.
pub struct PltHook {
   stub:     NonNull<c_void>,
   original: &'static AtomicPtr<c_void>,
}

// SAFETY: the stub is an opaque token bytehook accepts from any thread.
unsafe impl Send for PltHook {}
// SAFETY: see the Send impl.
unsafe impl Sync for PltHook {}

fn init() -> Result<(), Error> {
   static INIT: OnceLock<c_int> = OnceLock::new();
   #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
   crate::shadowhook::init()?;
   // SAFETY: plain FFI call. Manual mode lets detours call the saved original
   // directly.
   let status = *INIT.get_or_init(|| unsafe {
      sys::bytehook_init(sys::BYTEHOOK_MODE_MANUAL.cast_signed(), false)
   });
   if status == 0 {
      Ok(())
   } else {
      Err(Error::Init {
         backend: "bytehook",
         message: format!("status {status}"),
      })
   }
}

unsafe extern "C" fn hooked(
   _stub: sys::bytehook_stub_t,
   status: c_int,
   _caller: *const c_char,
   _symbol: *const c_char,
   _new: *mut c_void,
   prev: *mut c_void,
   slot: *mut c_void,
) {
   if status == 0 {
      // SAFETY: `slot` is the leaked AtomicPtr passed to bytehook_hook_single.
      unsafe { &*slot.cast::<AtomicPtr<c_void>>() }.store(prev, Ordering::Release);
   }
}

impl PltHook {
   /// Rewrites `caller`'s import of `symbol` to `detour`. `ByteHook` applies
   /// the rewrite when `caller` loads, so [`PltHook::original`] stays `None`
   /// until then.
   ///
   /// # Safety
   ///
   /// `detour` has the ABI of `symbol`.
   pub unsafe fn single(
      caller: &CStr,
      exporter: Option<&CStr>,
      symbol: &CStr,
      detour: NonNull<c_void>,
   ) -> Result<Self, Error> {
      init()?;
      let original: &'static AtomicPtr<c_void> =
         Box::leak(Box::new(AtomicPtr::new(ptr::null_mut())));
      // SAFETY: every string is NUL-terminated, the slot outlives the hook.
      let stub = unsafe {
         sys::bytehook_hook_single(
            caller.as_ptr(),
            exporter.map_or(ptr::null(), CStr::as_ptr),
            symbol.as_ptr(),
            detour.as_ptr(),
            Some(hooked),
            ptr::from_ref(original).cast_mut().cast(),
         )
      };
      let stub = NonNull::new(stub).ok_or_else(|| {
         Error::Attach {
            backend: "bytehook",
            target:  0,
            message: format!(
               "{} in {}",
               symbol.to_string_lossy(),
               caller.to_string_lossy()
            ),
         }
      })?;
      Ok(Self { stub, original })
   }

   #[must_use]
   pub fn original(&self) -> Option<NonNull<c_void>> {
      NonNull::new(self.original.load(Ordering::Acquire))
   }

   /// # Safety
   ///
   /// No thread is inside the detour or holds the [`PltHook::original`]
   /// pointer.
   pub unsafe fn detach(self) -> Result<(), Error> {
      // SAFETY: the stub came from bytehook_hook_single and is unhooked once.
      let status = unsafe { sys::bytehook_unhook(self.stub.as_ptr()) };
      if status == 0 {
         return Ok(());
      }
      Err(Error::Detach {
         backend: "bytehook",
         target:  0,
         message: format!("status {status}"),
      })
   }
}
