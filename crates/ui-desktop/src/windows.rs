use std::{
   ffi::c_void,
   ptr,
};

use crate::Error;

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const SW_SHOWNORMAL: i32 = 1;

#[link(name = "user32")]
unsafe extern "system" {
   fn OpenClipboard(hwnd: *mut c_void) -> i32;
   fn CloseClipboard() -> i32;
   fn EmptyClipboard() -> i32;
   fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
   fn GlobalLock(mem: *mut c_void) -> *mut c_void;
   fn GlobalUnlock(mem: *mut c_void) -> i32;
   fn GlobalFree(mem: *mut c_void) -> *mut c_void;
}

#[link(name = "shell32")]
unsafe extern "system" {
   fn ShellExecuteW(
      hwnd: *mut c_void,
      operation: *const u16,
      file: *const u16,
      parameters: *const u16,
      directory: *const u16,
      show_cmd: i32,
   ) -> isize;
}

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_err(context: &str) -> Error {
   Error::Platform {
      message: format!("{context}: {}", std::io::Error::last_os_error()),
   }
}

pub fn copy(text: &str) -> Result<(), Error> {
   let units = wide(text);
   let bytes = units.len() * size_of::<u16>();

   // SAFETY: Win32 clipboard APIs with checked return values; memory is freed
   // on any error before SetClipboardData takes ownership.
   unsafe {
      if OpenClipboard(ptr::null_mut()) == 0 {
         return Err(os_err("OpenClipboard"));
      }
      if EmptyClipboard() == 0 {
         let err = os_err("EmptyClipboard");
         CloseClipboard();
         return Err(err);
      }
      let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
      if mem.is_null() {
         let err = os_err("GlobalAlloc");
         CloseClipboard();
         return Err(err);
      }
      let locked = GlobalLock(mem);
      if locked.is_null() {
         let err = os_err("GlobalLock");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      ptr::copy_nonoverlapping(units.as_ptr(), locked.cast::<u16>(), units.len());
      GlobalUnlock(mem);
      if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
         let err = os_err("SetClipboardData");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      CloseClipboard();
   }
   Ok(())
}

pub fn open_url(url: &str) -> Result<(), Error> {
   let op = wide("open");
   let target = wide(url);
   // SAFETY: NUL-terminated UTF-16 strings passed to ShellExecuteW.
   let result = unsafe {
      ShellExecuteW(
         ptr::null_mut(),
         op.as_ptr(),
         target.as_ptr(),
         ptr::null(),
         ptr::null(),
         SW_SHOWNORMAL,
      )
   };
   if result <= 32 {
      return Err(Error::Platform {
         message: format!("ShellExecuteW failed with code {result}"),
      });
   }
   Ok(())
}
