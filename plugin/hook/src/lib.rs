//! Native side of the plugin host: loaded modules, signature scans, the gateway registry hooks
//! and the JNI bridge. Everything that touches Phoenix or ART goes through here.
pub mod inline;
pub mod jni;
pub mod module;
pub mod registry;
pub mod scan;

use std::ffi::{c_char, c_int, c_void, CString};

extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
    fn getpagesize() -> c_int;
    pub fn malloc(n: usize) -> *mut c_void;
    pub fn free(p: *mut c_void);
}

pub const INFO: i32 = 4;
pub const WARN: i32 = 5;
pub const ERROR: i32 = 6;

/// logcat, tag PhoenixJS (one tag for the whole host: `adb logcat -s PhoenixJS`).
pub fn log(prio: i32, s: &str) {
    let c = CString::new(s.replace('\0', "\\0")).unwrap_or_default();
    unsafe { __android_log_write(prio, c"PhoenixJS".as_ptr(), c.as_ptr()) };
}

#[macro_export]
macro_rules! info { ($($t:tt)*) => { $crate::log($crate::INFO, &format!($($t)*)) } }
#[macro_export]
macro_rules! warn { ($($t:tt)*) => { $crate::log($crate::WARN, &format!($($t)*)) } }

pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;

/// mprotect over the pages covering [a, b).
pub unsafe fn protect(a: usize, b: usize, prot: c_int) -> bool {
    let ps = getpagesize() as usize;
    let (lo, hi) = (a & !(ps - 1), (b + ps - 1) & !(ps - 1));
    mprotect(lo as *mut c_void, hi - lo, prot) == 0
}
