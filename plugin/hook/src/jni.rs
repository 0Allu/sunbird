//! Phoenix guest<->host bridge interception, without inline hooks (works on every ABI):
//! - Rust -> Java goes through JNIEnv function tables. ART's process-wide table (shared by all
//!   threads) gets CallVoidMethodA / CallObjectMethodA filters for GuestHost.applyOps([BI)V and
//!   GuestHost.invoke([B)[B. GuestHost.invoke treats a frame starting with 4 zero bytes as a UI-op
//!   batch (applyOps(frame[4:])), which is how almost all UI ops travel, so those go to OPS too.
//! - Java -> Rust: WasmRuntime.nativeDispatch(J[B)Z is re-bound with RegisterNatives to a wrapper
//!   around the exported Java_..._nativeDispatch.
//! Everything internal calls through the saved original table (`table`), bypassing the filters.
use crate::module::Module;
use crate::registry::Action;
pub use jni_sys;
use jni_sys::*;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicUsize, Ordering::Relaxed};
use std::sync::{Mutex, OnceLock};

pub const OPS: usize = 0;
pub const INVOKE: usize = 1;
pub const INVOKE_RESULT: usize = 2;
pub const DISPATCH: usize = 3;
static LISTEN: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];

/// Bridge listener: (kind, a, b) -> action. For INVOKE_RESULT a = request, b = response and
/// Replace swaps the response; otherwise b is None.
pub type BridgeFn = fn(kind: usize, a: &[u8], b: Option<&[u8]>) -> Action;
static SINK: OnceLock<BridgeFn> = OnceLock::new();
pub fn set_sink(f: BridgeFn) { let _ = SINK.set(f); }
pub fn listen(kind: usize, delta: i32) { if let Some(l) = LISTEN.get(kind) { l.fetch_add(delta, Relaxed); } }
fn on(kind: usize) -> bool { LISTEN[kind].load(Relaxed) > 0 }
fn sink(kind: usize, a: &[u8], b: Option<&[u8]>) -> Action { SINK.get().map_or(Action::Pass, |f| f(kind, a, b)) }

struct Table(JNINativeInterface_);
unsafe impl Send for Table {}
unsafe impl Sync for Table {} // a copy of ART's table: plain function pointers
static SAVED: OnceLock<Table> = OnceLock::new();
static PATCH_LOCK: Mutex<()> = Mutex::new(());

/// The original (unfiltered) JNI table.
pub unsafe fn table(env: *mut JNIEnv) -> &'static JNINativeInterface_ {
    SAVED.get().map(|t| &t.0).unwrap_or_else(|| &**(env as *const *const JNINativeInterface_))
}

/// `jcall!(env, FindClass, name)`: call a JNI function through the unfiltered table.
#[macro_export]
macro_rules! jcall {
    ($env:expr, $f:ident $(, $a:expr)* $(,)?) => { ($crate::jni::table($env).$f.unwrap())($env $(, $a)*) };
}

extern "C" {
    fn dlsym(h: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlopen(name: *const c_char, flags: c_int) -> *mut c_void;
}

pub fn vm() -> *mut JavaVM {
    static VM: AtomicPtr<JavaVM> = AtomicPtr::new(std::ptr::null_mut());
    let v = VM.load(Relaxed);
    if !v.is_null() { return v; }
    type F = unsafe extern "system" fn(*mut *mut JavaVM, jsize, *mut jsize) -> jint;
    unsafe {
        let mut f = dlsym(std::ptr::null_mut(), c"JNI_GetCreatedJavaVMs".as_ptr());
        if f.is_null() {
            let h = dlopen(c"libnativehelper.so".as_ptr(), 2);
            if !h.is_null() { f = dlsym(h, c"JNI_GetCreatedJavaVMs".as_ptr()); }
        }
        if f.is_null() { return v; }
        let (mut vm, mut n): (*mut JavaVM, jsize) = (std::ptr::null_mut(), 0);
        if std::mem::transmute::<*mut c_void, F>(f)(&mut vm, 1, &mut n) == JNI_OK && n > 0 { VM.store(vm, Relaxed); }
        vm
    }
}

/// JNIEnv of the current thread, attaching it if needed (attached threads stay attached).
pub unsafe fn env() -> Option<*mut JNIEnv> {
    let vm = vm();
    if vm.is_null() { return None; }
    let mut env: *mut c_void = std::ptr::null_mut();
    if ((**vm).GetEnv.unwrap())(vm, &mut env, JNI_VERSION_1_6) == JNI_OK { return Some(env as *mut JNIEnv); }
    (((**vm).AttachCurrentThread.unwrap())(vm, &mut env, std::ptr::null_mut()) == JNI_OK).then_some(env as *mut JNIEnv)
}

/// Detach before a thread we attached exits (ART aborts on exit while attached).
pub unsafe fn detach() {
    let vm = vm();
    if !vm.is_null() { ((**vm).DetachCurrentThread.unwrap())(vm); }
}

pub unsafe fn clear_exc(env: *mut JNIEnv) -> bool {
    let t = jcall!(env, ExceptionCheck) != 0;
    if t { jcall!(env, ExceptionClear); }
    t
}

pub unsafe fn get_bytes(env: *mut JNIEnv, a: jbyteArray, lim: Option<jsize>) -> Vec<u8> {
    if a.is_null() { return vec![]; }
    let mut n = jcall!(env, GetArrayLength, a);
    if let Some(l) = lim { if l >= 0 && l < n { n = l; } }
    let mut v = vec![0u8; n.max(0) as usize];
    jcall!(env, GetByteArrayRegion, a, 0, n, v.as_mut_ptr() as *mut jbyte);
    v
}
pub unsafe fn new_bytes(env: *mut JNIEnv, b: &[u8]) -> jbyteArray {
    let a = jcall!(env, NewByteArray, b.len() as jsize);
    if !a.is_null() { jcall!(env, SetByteArrayRegion, a, 0, b.len() as jsize, b.as_ptr() as *const jbyte); }
    a
}
pub unsafe fn jstr(env: *mut JNIEnv, s: jstring) -> Option<String> {
    if s.is_null() { return None; }
    let p = jcall!(env, GetStringUTFChars, s, std::ptr::null_mut());
    if p.is_null() { return None; }
    let r = CStr::from_ptr(p).to_string_lossy().into_owned();
    jcall!(env, ReleaseStringUTFChars, s, p);
    Some(r)
}

/// Patches ART's shared JNI table once (called from every hooked thread; cheap after the first).
pub fn attach() {
    if SAVED.get().is_some() { return; }
    unsafe {
        let vm = vm();
        if vm.is_null() { return; }
        let mut env: *mut c_void = std::ptr::null_mut();
        if ((**vm).GetEnv.unwrap())(vm, &mut env, JNI_VERSION_1_6) != JNI_OK || env.is_null() { return; }
        let _g = PATCH_LOCK.lock().unwrap();
        if SAVED.get().is_some() { return; }
        let t = *(env as *const *mut JNINativeInterface_);
        let saved = *t;
        let (lo, hi) = (t as usize, t as usize + std::mem::size_of::<JNINativeInterface_>());
        if !crate::protect(lo, hi, crate::PROT_READ | crate::PROT_WRITE) {
            crate::warn!("bridge: JNI table mprotect failed");
            return;
        }
        let _ = SAVED.set(Table(saved));
        (*t).CallVoidMethodA = Some(w_call_void_method_a);
        (*t).CallObjectMethodA = Some(w_call_object_method_a);
        crate::protect(lo, hi, crate::PROT_READ);
        crate::info!("bridge: JNI table patched");
    }
}

static MIDS: Mutex<Vec<(usize, i32)>> = Mutex::new(Vec::new());
const KIND_OPS: i32 = OPS as i32;
const KIND_INVOKE: i32 = INVOKE as i32;

/// -1 other method, OPS (GuestHost.applyOps) or INVOKE (GuestHost.invoke). Cached per jmethodID.
unsafe fn method_kind(env: *mut JNIEnv, obj: jobject, mid: jmethodID) -> i32 {
    if let Some(k) = MIDS.lock().unwrap().iter().find(|m| m.0 == mid as usize) { return k.1; }
    let mut kind = -1;
    let cls = jcall!(env, GetObjectClass, obj);
    let m = jcall!(env, ToReflectedMethod, cls, mid, JNI_FALSE);
    if !m.is_null() {
        let cm = jcall!(env, GetObjectClass, m);
        let get_name = jcall!(env, GetMethodID, cm, c"getName".as_ptr(), c"()Ljava/lang/String;".as_ptr());
        let n = jstr(env, jcall!(env, CallObjectMethodA, m, get_name, std::ptr::null()));
        let cc = jcall!(env, FindClass, c"java/lang/Class".as_ptr());
        let cget = jcall!(env, GetMethodID, cc, c"getName".as_ptr(), c"()Ljava/lang/String;".as_ptr());
        let c = jstr(env, jcall!(env, CallObjectMethodA, cls, cget, std::ptr::null()));
        if c.is_some_and(|c| c.contains("nativeapp.bridge.GuestHost")) {
            kind = match n.as_deref() { Some("applyOps") => KIND_OPS, Some("invoke") => KIND_INVOKE, _ => -1 };
        }
    }
    clear_exc(env);
    let mut g = MIDS.lock().unwrap();
    if g.len() < 64 { g.push((mid as usize, kind)); }
    kind
}

unsafe fn orig() -> &'static JNINativeInterface_ { &SAVED.get().unwrap().0 }

unsafe extern "system" fn w_call_void_method_a(env: *mut JNIEnv, obj: jobject, mid: jmethodID, args: *const jvalue) {
    let kind = if on(OPS) || !NATIVES_BOUND.load(Relaxed) { method_kind(env, obj, mid) } else { -1 };
    if kind >= 0 { bind_natives(env, obj); }
    if kind == KIND_OPS && on(OPS) {
        let (a0, a1) = (*args, *args.add(1));
        let b = get_bytes(env, a0.l, Some(a1.i));
        match sink(OPS, &b, None) {
            Action::Drop => return,
            Action::Replace(v) => {
                let arr = new_bytes(env, &v);
                let a2 = [jvalue { l: arr }, jvalue { i: v.len() as jint }];
                (orig().CallVoidMethodA.unwrap())(env, obj, mid, a2.as_ptr());
                jcall!(env, DeleteLocalRef, arr);
                return;
            }
            Action::Pass => {}
        }
    }
    (orig().CallVoidMethodA.unwrap())(env, obj, mid, args)
}

unsafe extern "system" fn w_call_object_method_a(env: *mut JNIEnv, obj: jobject, mid: jmethodID, args: *const jvalue) -> jobject {
    let call = |a: *const jvalue| (orig().CallObjectMethodA.unwrap())(env, obj, mid, a);
    let watch = on(OPS) || on(INVOKE) || on(INVOKE_RESULT);
    let kind = if watch || !NATIVES_BOUND.load(Relaxed) { method_kind(env, obj, mid) } else { -1 };
    if kind >= 0 { bind_natives(env, obj); }
    if !watch || kind != KIND_INVOKE { return call(args); }
    let req = get_bytes(env, (*args).l, None);
    if req.len() >= 4 && req[..4] == [0; 4] {
        // UI-op batch: invoke returns null for these
        if !on(OPS) { return call(args); }
        return match sink(OPS, &req[4..], None) {
            Action::Drop => std::ptr::null_mut(),
            Action::Pass => call(args),
            Action::Replace(v) => {
                let mut f = vec![0u8; 4];
                f.extend_from_slice(&v);
                let arr = new_bytes(env, &f);
                let r = call([jvalue { l: arr }].as_ptr());
                jcall!(env, DeleteLocalRef, arr);
                r
            }
        };
    }
    let mut req_used = req;
    let mut tmp: jobject = std::ptr::null_mut();
    if on(INVOKE) {
        if let Action::Replace(v) = sink(INVOKE, &req_used, None) {
            tmp = new_bytes(env, &v);
            req_used = v;
        }
    }
    let resp = if tmp.is_null() { call(args) } else { call([jvalue { l: tmp }].as_ptr()) };
    if !tmp.is_null() { jcall!(env, DeleteLocalRef, tmp); }
    if resp.is_null() || !on(INVOKE_RESULT) || jcall!(env, ExceptionCheck) != 0 { return resp; }
    let r = get_bytes(env, resp, None);
    match sink(INVOKE_RESULT, &req_used, Some(&r)) {
        Action::Replace(v) => {
            jcall!(env, DeleteLocalRef, resp);
            new_bytes(env, &v)
        }
        _ => resp,
    }
}

// ---- Java -> Rust: nativeDispatch -------------------------------------------------------------
static NATIVES_BOUND: AtomicBool = AtomicBool::new(false);
static ORIG_DISPATCH: AtomicUsize = AtomicUsize::new(0);
type DispatchFn = unsafe extern "system" fn(*mut JNIEnv, jclass, jlong, jbyteArray) -> jboolean;

unsafe extern "system" fn w_native_dispatch(env: *mut JNIEnv, cls: jclass, handle: jlong, bytes: jbyteArray) -> jboolean {
    attach(); // main/UI thread etc.
    let orig: DispatchFn = std::mem::transmute(ORIG_DISPATCH.load(Relaxed));
    if on(DISPATCH) {
        let b = get_bytes(env, bytes, None);
        match sink(DISPATCH, &b, None) {
            Action::Drop => return JNI_TRUE,
            Action::Replace(v) => {
                let arr = new_bytes(env, &v);
                let r = orig(env, cls, handle, arr);
                jcall!(env, DeleteLocalRef, arr);
                return r;
            }
            Action::Pass => {}
        }
    }
    orig(env, cls, handle, bytes)
}

unsafe fn bind_natives(env: *mut JNIEnv, guest_host: jobject) {
    if NATIVES_BOUND.swap(true, Relaxed) { return; }
    let Some(f) = Module::find("liblibdiscord_wasm_host.so").and_then(|m| m.dlsym("Java_com_discord_nativeapp_bridge_WasmRuntime_nativeDispatch")) else {
        crate::warn!("bridge: nativeDispatch symbol not found");
        return;
    };
    ORIG_DISPATCH.store(f, Relaxed);
    // WasmRuntime through the app class loader (GuestHost's)
    let Some(wr) = load_class(env, guest_host, c"com.discord.nativeapp.bridge.WasmRuntime") else {
        crate::warn!("bridge: WasmRuntime class not found");
        return;
    };
    let m = JNINativeMethod {
        name: c"nativeDispatch".as_ptr() as *mut c_char,
        signature: c"(J[B)Z".as_ptr() as *mut c_char,
        fnPtr: w_native_dispatch as *mut c_void,
    };
    if jcall!(env, RegisterNatives, wr, &m, 1) != 0 {
        clear_exc(env);
        crate::warn!("bridge: RegisterNatives failed");
        return;
    }
    crate::info!("bridge: nativeDispatch re-bound");
}

/// Loads `name` (dotted) through the class loader of `anchor`'s class (the app's loader).
pub unsafe fn load_class(env: *mut JNIEnv, anchor: jobject, name: &CStr) -> Option<jclass> {
    let ac = jcall!(env, GetObjectClass, anchor);
    let cc = jcall!(env, FindClass, c"java/lang/Class".as_ptr());
    let gl = jcall!(env, GetMethodID, cc, c"getClassLoader".as_ptr(), c"()Ljava/lang/ClassLoader;".as_ptr());
    let loader = jcall!(env, CallObjectMethodA, ac, gl, std::ptr::null());
    if loader.is_null() { clear_exc(env); return None; }
    let cl = jcall!(env, FindClass, c"java/lang/ClassLoader".as_ptr());
    let lc = jcall!(env, GetMethodID, cl, c"loadClass".as_ptr(), c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr());
    let a = [jvalue { l: jcall!(env, NewStringUTF, name.as_ptr()) }];
    let c = jcall!(env, CallObjectMethodA, loader, lc, a.as_ptr());
    (!clear_exc(env) && !c.is_null()).then_some(c as jclass)
}
