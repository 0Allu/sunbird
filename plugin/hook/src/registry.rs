//! Phoenix's core gateway registry: an array of {name ptr, name len, handler} (stride 24), with
//!   Result handler(Store *store, const Event *ev, Ctx *ctx)   (Result: 32 bytes, Ok = w[0] == -1)
//! The 32-byte struct return goes through the hidden result pointer (rdi on x86_64, x8 on arm64),
//! which is what `extern "C" fn -> Res` produces too. Every handler is swapped for one generic
//! wrapper; events nobody listens to go straight to the original without taking any lock.
use crate::module::Module;
use std::cell::Cell;
use std::ffi::c_void;
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering::Relaxed};
use std::sync::OnceLock;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Str {
    pub p: *const u8,
    pub n: usize,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Event {
    pub name: Str,
    pub data: Str,
    pub rest: [u64; 4],
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Res {
    pub w: [u64; 4],
}
pub type Handler = unsafe extern "C" fn(store: *mut c_void, ev: *const Event, ctx: *mut c_void) -> Res;
const OK: Res = Res { w: [u64::MAX, 0, 0, 0] };

pub enum Action {
    Pass,
    Drop,
    Replace(Vec<u8>),
}

/// Where events go: `sync` runs on Phoenix's store thread before the original handler,
/// `observe` gets a copy for later (it must not block).
pub trait Sink: Send + Sync {
    fn sync(&self, slot: usize, data: &[u8]) -> Action;
    fn observe(&self, slot: usize, data: &[u8]);
}

struct Slot {
    name: String,
    orig: Handler,
    sync: AtomicI32,
    observe: AtomicI32,
}
static SLOTS: OnceLock<Vec<Slot>> = OnceLock::new();
static SINK: OnceLock<Box<dyn Sink>> = OnceLock::new();
static STORE: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static CTX: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static REST: [AtomicU64; 4] = [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)];
thread_local! { static IN_HANDLER: Cell<bool> = const { Cell::new(false) }; }

unsafe fn entry(host: &Module, e: usize) -> Option<(String, *mut Handler)> {
    if !host.contains(e) || !host.contains(e + 23) { return None; }
    let (name, len, h) = (*(e as *const usize), *((e + 8) as *const usize), *((e + 16) as *const usize));
    if !(1..48).contains(&len) || !host.contains(name) || !host.is_code(h) { return None; }
    let s = std::slice::from_raw_parts(name as *const u8, len);
    s.iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_')
        .then(|| (String::from_utf8_lossy(s).into_owned(), (e + 16) as *mut Handler))
}

/// Hooks the whole registry around `anchor` (address of one entry's handler field). Returns the
/// number of events hooked.
pub unsafe fn install(host: &Module, anchor: usize) -> usize {
    let mut first = anchor - 16;
    if entry(host, first).is_none() { return 0; }
    while entry(host, first - 24).is_some() { first -= 24; }
    let mut list = Vec::new();
    let mut e = first;
    while let Some(x) = entry(host, e) { list.push(x); e += 24; }
    let (lo, hi) = (first, e);
    if !crate::protect(lo, hi, crate::PROT_READ | crate::PROT_WRITE) {
        crate::warn!("registry: mprotect failed");
        return 0;
    }
    let slots = list.iter().map(|(name, p)| {
        let orig = **p;
        **p = w_any;
        Slot { name: name.clone(), orig, sync: AtomicI32::new(0), observe: AtomicI32::new(0) }
    }).collect::<Vec<_>>();
    crate::protect(lo, hi, crate::PROT_READ);
    let n = slots.len();
    let _ = SLOTS.set(slots);
    n
}

pub fn set_sink(s: Box<dyn Sink>) { let _ = SINK.set(s); }
pub fn names() -> Vec<String> { SLOTS.get().map_or(vec![], |s| s.iter().map(|x| x.name.clone()).collect()) }

/// Adjusts the listener count for `name` (kind 0 sync, 1 observe). False if no such event.
pub fn listen(name: &str, kind: i32, delta: i32) -> bool {
    let Some(s) = SLOTS.get().and_then(|s| s.iter().find(|x| x.name == name)) else { return false };
    if kind == 0 { &s.sync } else { &s.observe }.fetch_add(delta, Relaxed);
    true
}

/// True while sync listeners run on the store thread (store reads are only safe there).
pub fn in_handler() -> bool { IN_HANDLER.with(|c| c.get()) }
pub fn store() -> usize { STORE.load(Relaxed) as usize }

/// Runs `data` through the original handler of `name`. None: unknown event or no store yet.
pub unsafe fn dispatch(name: &str, data: &[u8]) -> Option<bool> {
    let s = SLOTS.get()?.iter().find(|x| x.name == name)?;
    let store = STORE.load(Relaxed);
    if store.is_null() { return None; }
    let ev = Event {
        name: Str { p: s.name.as_ptr(), n: s.name.len() },
        data: Str { p: data.as_ptr(), n: data.len() },
        rest: std::array::from_fn(|i| REST[i].load(Relaxed)),
    };
    Some((s.orig)(store, &ev, CTX.load(Relaxed)).w[0] == u64::MAX)
}

/// Runs one of Phoenix's serde_json Serialize impls on a store field:
///   Box<Error>* ser(const T *value, Vec<u8> **out)   (Vec = {cap, ptr, len}; allocator = malloc)
/// Err: not on the store thread inside a handler. Ok(None): the serializer failed.
pub unsafe fn serialize(ser: usize, val_off: usize) -> Result<Option<Vec<u8>>, ()> {
    let store = STORE.load(Relaxed) as usize;
    if !in_handler() || store == 0 { return Err(()); }
    #[repr(C)]
    struct V { cap: usize, ptr: *mut u8, len: usize }
    let mut v = V { cap: 0, ptr: 1 as *mut u8, len: 0 };
    let mut slot = &mut v as *mut V;
    let f: unsafe extern "C" fn(*const u8, *mut *mut V) -> *mut c_void = std::mem::transmute(ser);
    let err = f((store + val_off) as *const u8, &mut slot);
    let out = (err.is_null()).then(|| std::slice::from_raw_parts(v.ptr, v.len).to_vec());
    if v.cap != 0 { crate::free(v.ptr as *mut c_void); }
    Ok(out)
}

unsafe extern "C" fn w_any(store: *mut c_void, ev: *const Event, ctx: *mut c_void) -> Res {
    crate::jni::attach();
    let ev = &*ev;
    STORE.store(store, Relaxed);
    CTX.store(ctx, Relaxed);
    for (r, v) in REST.iter().zip(ev.rest) { r.store(v, Relaxed); }
    let name = std::slice::from_raw_parts(ev.name.p, ev.name.n);
    let Some((i, s)) = SLOTS.get().and_then(|v| v.iter().enumerate().find(|(_, x)| x.name.as_bytes() == name)) else {
        return OK; // cannot happen: every wrapped entry is a slot
    };
    let (sync, obs) = (s.sync.load(Relaxed) > 0, s.observe.load(Relaxed) > 0);
    if !sync && !obs { return (s.orig)(store, ev, ctx); }
    let Some(sink) = SINK.get() else { return (s.orig)(store, ev, ctx) };
    let data = std::slice::from_raw_parts(ev.data.p, ev.data.n);
    if obs { sink.observe(i, data); }
    if !sync { return (s.orig)(store, ev, ctx); }
    IN_HANDLER.with(|c| c.set(true));
    let act = sink.sync(i, data);
    IN_HANDLER.with(|c| c.set(false));
    match act {
        Action::Drop => OK,
        Action::Pass => (s.orig)(store, ev, ctx),
        Action::Replace(d) => {
            let e2 = Event { data: Str { p: d.as_ptr(), n: d.len() }, ..*ev };
            (s.orig)(store, &e2, ctx)
        }
    }
}
