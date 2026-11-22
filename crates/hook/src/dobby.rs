use std::{
   ffi::c_void,
   ptr::NonNull,
};

use crate::{
   Error,
   Hook,
};

pub struct Dobby {
   target:   NonNull<c_void>,
   original: NonNull<c_void>,
}

// SAFETY: both pointers name code, which never moves and is callable from any
// thread.
unsafe impl Send for Dobby {}
// SAFETY: see the Send impl.
unsafe impl Sync for Dobby {}

impl Hook for Dobby {
   unsafe fn attach(target: NonNull<c_void>, detour: NonNull<c_void>) -> Result<Self, Error> {
      let attach_error = |message: String| {
         Error::Attach {
            backend: "dobby",
            target: target.addr().get(),
            message,
         }
      };
      // SAFETY: forwarded from the caller.
      let original = unsafe { dobby_rs::hook(target.as_ptr(), detour.as_ptr()) }
         .map_err(|err| attach_error(err.to_string()))?;
      let original =
         NonNull::new(original).ok_or_else(|| attach_error("null trampoline".into()))?;
      Ok(Self { target, original })
   }

   fn original(&self) -> NonNull<c_void> {
      self.original
   }

   unsafe fn detach(self) -> Result<(), Error> {
      // SAFETY: forwarded from the caller.
      unsafe { dobby_rs::unhook(self.target.as_ptr()) }.map_err(|err| {
         Error::Detach {
            backend: "dobby",
            target:  self.target.addr().get(),
            message: err.to_string(),
         }
      })
   }
}
