//! Inline hooks through ShadowHook's intercept API (arm64 only): a pre-callback with the full
//! register context at a function entry or at any instruction (e.g. a `ret`, to see results).
use std::ffi::{c_char, c_int, c_void, CStr};

#[repr(C)]
pub struct CpuContext {
    pub regs: [u64; 31], // x0-x30
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    vregs: [[u64; 2]; 32],
    fpsr: u64,
    fpcr: u64,
}
pub type Interceptor = unsafe extern "C" fn(ctx: *mut CpuContext, data: *mut c_void);

#[cfg(target_arch = "aarch64")]
mod sys {
    use super::*;
    extern "C" {
        pub fn shadowhook_init(mode: c_int, debuggable: bool) -> c_int;
        pub fn shadowhook_intercept_func_addr(addr: *mut c_void, pre: Interceptor, data: *mut c_void, flags: u32, ...) -> *mut c_void;
        pub fn shadowhook_intercept_instr_addr(addr: *mut c_void, pre: Interceptor, data: *mut c_void, flags: u32, ...) -> *mut c_void;
        pub fn shadowhook_unintercept(stub: *mut c_void) -> c_int;
        pub fn shadowhook_get_errno() -> c_int;
        pub fn shadowhook_to_errmsg(e: c_int) -> *const c_char;
    }
}

pub const AVAILABLE: bool = cfg!(target_arch = "aarch64");

#[cfg(target_arch = "aarch64")]
fn err() -> String {
    unsafe {
        let e = sys::shadowhook_get_errno();
        format!("ShadowHook error {e}: {}", CStr::from_ptr(sys::shadowhook_to_errmsg(e)).to_string_lossy())
    }
}

#[cfg(target_arch = "aarch64")]
fn init() -> Result<(), String> {
    static INIT: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INIT.get_or_init(|| match unsafe { sys::shadowhook_init(0 /* shared */, false) } {
        0 => { crate::info!("ShadowHook 2.0.1 ready"); Ok(()) }
        _ => Err(err()),
    }).clone()
}

/// Intercepts `addr` (`func`: a function entry; otherwise one instruction). Returns the stub
/// for [`unintercept`]. `cb` runs on whichever thread executes the code, before it.
pub fn intercept(addr: usize, func: bool, cb: Interceptor, data: usize) -> Result<usize, String> {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        init()?;
        let f = if func { sys::shadowhook_intercept_func_addr } else { sys::shadowhook_intercept_instr_addr };
        let stub = f(addr as *mut c_void, cb, data as *mut c_void, 0);
        if stub.is_null() { Err(err()) } else { Ok(stub as usize) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = (addr, func, cb, data);
        Err("inline hooks need arm64 (ShadowHook); on x86_64 use the Frida option".into())
    }
}

pub fn unintercept(stub: usize) -> Result<(), String> {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        if sys::shadowhook_unintercept(stub as *mut c_void) == 0 { Ok(()) } else { Err(err()) }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = stub;
        Err("no inline hooks on this ABI".into())
    }
}
