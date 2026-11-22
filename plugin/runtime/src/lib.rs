//! The JS plugin runtime (rquickjs / QuickJS-ng). Natives live on the hidden global `__phx`; the
//! public `phoenix` API is built on top of them in prelude.js (types: phoenix.d.ts).
//!
//! Threads: sync listeners run on Phoenix's store thread (inside the gateway handler), bridge
//! listeners on whichever thread crosses the JNI bridge, everything else (observe, timers,
//! fetch callbacks, promise jobs) on the "phoenix-js" thread. One runtime lock serialises them.
mod fetch;
mod native;

use hook::registry::{self, Action};
use hook::{info, log, ERROR};
use rquickjs::{function::Opt, CatchResultExt, Context, Ctx, Function, Object, Runtime, TypedArray, Value};
use std::cell::Cell;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
/// Built in after the prelude: the UI-op codec, Settings row templates and the Settings page.
const MODULES: [(&str, &str); 3] = [
    ("uiops", include_str!("../../ui/uiops.js")),
    ("settings_tpl", include_str!("settings_tpl.js")),
    ("settings", include_str!("settings.js")),
];

pub struct Config {
    /// files/phoenix-js
    pub dir: String,
    /// JSON [{id, source?, url?}] shipped in the APK, if any
    pub bundle: Option<String>,
    pub host_base: usize,
    /// (`<Me as Serialize>` address, Me offset in the store), from the sigs table
    pub me: Option<(usize, usize)>,
}

struct Js {
    _rt: Runtime,
    cx: Context,
    cfg: Config,
    start: Instant,
}
static JS: OnceLock<Js> = OnceLock::new();

enum Job {
    Picked(i32, Option<String>, Option<String>, Option<String>),
    Confirmed(i32, bool),
    Observe(usize, String),
    Fetched(i32, i32, Option<String>),
    Wake,
}
static QUEUE: Mutex<VecDeque<Job>> = Mutex::new(VecDeque::new());
static QCV: Condvar = Condvar::new();
fn push(j: Job) {
    QUEUE.lock().unwrap().push_back(j);
    QCV.notify_one();
}

thread_local! { static IN_JS: Cell<bool> = const { Cell::new(false) }; }

/// Runs f inside the context, then drains promise jobs. None when this thread is already inside
/// JS (a JS call led back into a hook, e.g. phoenix.dispatch -> handler -> JNI bridge): the
/// runtime lock is not recursive, so the nested hook passes through untouched.
pub(crate) fn with<R>(f: impl FnOnce(Ctx) -> R) -> Option<R> {
    let js = JS.get()?;
    if IN_JS.with(|c| c.replace(true)) { return None; }
    let r = js.cx.with(|ctx| {
        let r = f(ctx.clone());
        while ctx.execute_pending_job() {}
        r
    });
    IN_JS.with(|c| c.set(false));
    Some(r)
}

pub(crate) fn call<'js, R: rquickjs::FromJs<'js>>(ctx: &Ctx<'js>, name: &str, args: impl rquickjs::function::IntoArgs<'js>) -> Option<R> {
    let f: Function = ctx.globals().get(name).ok()?;
    f.call(args).catch(ctx).map_err(|e| log(ERROR, &format!("{name}: {e}"))).ok()
}

fn safe_path(dir: &str, name: &str) -> Option<String> {
    (!name.is_empty() && !name.contains('/') && name != "..").then(|| format!("{dir}/{name}"))
}

fn bytes_of(v: Value) -> Option<Vec<u8>> {
    let t = TypedArray::<u8>::from_value(v).ok()?;
    Some(t.as_raw().map(|p| unsafe { p.as_ref() }.to_vec()).unwrap_or_default())
}

fn install(ctx: &Ctx) -> rquickjs::Result<()> {
    let js = JS.get().unwrap();
    let p = Object::new(ctx.clone())?;
    fn f<'js>(p: &Object<'js>, name: &str, v: rquickjs::Result<Function<'js>>) -> rquickjs::Result<()> { p.set(name, v?) }
    f(&p, "log", Function::new(ctx.clone(), |lvl: i32, s: String| log([4, 4, 5, 6, 3][lvl.clamp(0, 4) as usize], &s)))?;
    f(&p, "listen", Function::new(ctx.clone(), |name: String, kind: i32, delta: i32| registry::listen(&name, kind, delta)))?;
    f(&p, "dispatch", Function::new(ctx.clone(), |name: String, data: String| unsafe { registry::dispatch(&name, data.as_bytes()) }))?;
    let store_json = |ctx: Ctx, ser: usize, off: usize| -> rquickjs::Result<Option<String>> {
        match unsafe { registry::serialize(ser, off) } {
            Err(()) => Err(rquickjs::Exception::throw_type(&ctx, "store reads only work inside a phoenix.on handler")),
            Ok(v) => Ok(v.map(|b| String::from_utf8_lossy(&b).into_owned())),
        }
    };
    let base = js.cfg.host_base;
    f(&p, "storeJson", Function::new(ctx.clone(), move |ctx: Ctx, ser: f64, off: f64| store_json(ctx, base + ser as usize, off as usize)))?;
    let me = js.cfg.me;
    f(&p, "me", Function::new(ctx.clone(), move |ctx: Ctx| match me {
        Some((ser, off)) => store_json(ctx, ser, off),
        None => Err(rquickjs::Exception::throw_type(&ctx, "the patcher found no Me store in this build")),
    }))?;
    f(&p, "scan", Function::new(ctx.clone(), move |ctx: Ctx, pat: String| -> rquickjs::Result<Vec<f64>> {
        let pat = hook::scan::Pattern::parse(&pat).map_err(|e| rquickjs::Exception::throw_type(&ctx, &e))?;
        let host = hook::module::Module::find("liblibdiscord_wasm_host.so").ok_or_else(|| rquickjs::Exception::throw_type(&ctx, "host lib not loaded"))?;
        Ok(host.code().iter().flat_map(|(a, b)| pat.find_all(b).map(move |o| (a + o - host.base) as f64)).take(64).collect())
    }))?;
    f(&p, "fetch", Function::new(ctx.clone(), |url: String, method: String, headers: String, body: Opt<Option<String>>| {
        fetch::start(fetch::Req { url, method, headers, body: body.0.flatten() }, |id, st, b| push(Job::Fetched(id, st, b)))
    }))?;
    let dir = js.cfg.dir.clone();
    f(&p, "readFile", Function::new(ctx.clone(), move |name: String| {
        safe_path(&dir, &name).and_then(|p| std::fs::read(p).ok()).map(|b| String::from_utf8_lossy(&b).into_owned())
    }))?;
    let dir = js.cfg.dir.clone();
    f(&p, "writeFile", Function::new(ctx.clone(), move |name: String, data: String| {
        let Some(p) = safe_path(&dir, &name) else { return false };
        let tmp = format!("{p}.tmp");
        std::fs::write(&tmp, data).and_then(|_| std::fs::rename(&tmp, &p)).is_ok()
    }))?;
    let dir = js.cfg.dir.clone();
    f(&p, "removeFile", Function::new(ctx.clone(), move |name: String| {
        safe_path(&dir, &name).is_some_and(|p| std::fs::remove_file(p).is_ok())
    }))?;
    let dir = js.cfg.dir.clone();
    f(&p, "listDir", Function::new(ctx.clone(), move || {
        let mut v: Vec<String> = std::fs::read_dir(&dir).into_iter().flatten().flatten()
            .filter_map(|e| e.file_name().into_string().ok()).filter(|n| !n.starts_with('.')).collect();
        v.sort();
        v
    }))?;
    f(&p, "now", Function::new(ctx.clone(), || JS.get().unwrap().start.elapsed().as_secs_f64() * 1e3))?;
    f(&p, "wake", Function::new(ctx.clone(), || push(Job::Wake)))?;
    // UI ops straight to Java's UIManager. Not on the main thread (see hook::jni::apply_ops).
    f(&p, "uiApply", Function::new(ctx.clone(), |v: Value| {
        extern "C" { fn gettid() -> i32; fn getpid() -> i32; }
        if unsafe { gettid() == getpid() } { return false; }
        bytes_of(v).is_some_and(|b| hook::jni::apply_ops(&b))
    }))?;
    // pickFile(id, mime): Android's document picker; the result comes back as __phx_picked(id, name, text, error)
    f(&p, "pickFile", Function::new(ctx.clone(), |ctx: Ctx, id: i32, mime: String| -> rquickjs::Result<()> {
        hook::jni::pick_file(id, &mime).map_err(|e| rquickjs::Exception::throw_message(&ctx, &e))
    }))?;
    f(&p, "toast", Function::new(ctx.clone(), |ctx: Ctx, text: String, long: Opt<bool>| -> rquickjs::Result<()> {
        hook::jni::toast(&text, long.0.unwrap_or(false)).map_err(|e| rquickjs::Exception::throw_message(&ctx, &e))
    }))?;
    f(&p, "confirm", Function::new(ctx.clone(), |ctx: Ctx, id: i32, title: String, message: String, ok: String, cancel: String| -> rquickjs::Result<()> {
        hook::jni::confirm(id, &title, &message, &ok, &cancel).map_err(|e| rquickjs::Exception::throw_message(&ctx, &e))
    }))?;
    f(&p, "uiReady", Function::new(ctx.clone(), hook::jni::ui_ready))?;
    f(&p, "inHandler", Function::new(ctx.clone(), registry::in_handler))?;
    f(&p, "wantStore", Function::new(ctx.clone(), registry::request_store_thread))?;
    f(&p, "bridgeListen", Function::new(ctx.clone(), |k: usize, d: i32| hook::jni::listen(k, d)))?;
    p.set("events", registry::names())?;
    native::install(ctx, &p)?;
    ctx.globals().set("__phx", p)?;
    ctx.eval::<(), _>(PRELUDE)?;
    for (name, src) in MODULES {
        if let Err(e) = ctx.eval::<(), _>(src).catch(ctx) { log(ERROR, &format!("{name}: {e}")); }
    }
    Ok(())
}

struct Sink;
impl registry::Sink for Sink {
    fn sync(&self, slot: usize, data: &[u8]) -> Action {
        let raw = String::from_utf8_lossy(data).into_owned();
        let name = registry::names().get(slot).cloned().unwrap_or_default();
        with(|ctx| match call::<Value>(&ctx, "__phx_emit", (name, raw)) {
            Some(v) if v.as_bool() == Some(false) => Action::Drop,
            Some(v) => v.as_string().and_then(|s| s.to_string().ok()).map_or(Action::Pass, |s| Action::Replace(s.into_bytes())),
            None => Action::Pass,
        }).unwrap_or(Action::Pass)
    }
    fn observe(&self, slot: usize, data: &[u8]) { push(Job::Observe(slot, String::from_utf8_lossy(data).into_owned())); }
    fn store_work(&self) { with(|ctx| call::<()>(&ctx, "__phx_storeWork", ())); }
}

fn bridge(kind: usize, a: &[u8], b: Option<&[u8]>) -> Action {
    with(|ctx| {
        let ta = TypedArray::<u8>::new_copy(ctx.clone(), a).ok()?;
        let tb = b.and_then(|b| TypedArray::<u8>::new_copy(ctx.clone(), b).ok());
        let v: Value = call(&ctx, "__phx_bridge", (kind as i32, ta, tb))?;
        if v.as_bool() == Some(false) { return Some(Action::Drop); }
        bytes_of(v).map(Action::Replace)
    }).flatten().unwrap_or(Action::Pass)
}

fn js_thread() {
    loop {
        let wait = with(|ctx| call::<f64>(&ctx, "__phx_tick", ()).unwrap_or(-1.0)).unwrap_or(-1.0);
        let jobs: Vec<Job> = {
            let mut q = QUEUE.lock().unwrap();
            if q.is_empty() {
                q = if wait < 0.0 { QCV.wait(q).unwrap() } else { QCV.wait_timeout(q, Duration::from_micros((wait * 1e3) as u64)).unwrap().0 };
            }
            q.drain(..).collect()
        };
        for j in jobs {
            with(|ctx| match j {
                Job::Observe(i, s) => call::<()>(&ctx, "__phx_observe", (i as i32, s)).unwrap_or(()),
                Job::Fetched(id, st, s) => call::<()>(&ctx, "__phx_fetched", (id, s, st)).unwrap_or(()),
                Job::Picked(id, n, t, e) => call::<()>(&ctx, "__phx_picked", (id, n, t, e)).unwrap_or(()),
                Job::Confirmed(id, ok) => call::<()>(&ctx, "__phx_confirmed", (id, ok)).unwrap_or(()),
                Job::Wake => {}
            });
        }
    }
}

/// Creates the runtime, loads plugins and starts the JS thread. Call after the registry is hooked.
pub fn start(cfg: Config) -> bool {
    let rt = match Runtime::new() { Ok(r) => r, Err(e) => { log(ERROR, &format!("runtime: {e}")); return false } };
    rt.set_max_stack_size(512 * 1024);
    // unhandled promise rejections go to the console like uncaught errors
    rt.set_host_promise_rejection_tracker(Some(Box::new(|ctx, _promise, reason, handled| {
        if !handled { call::<()>(&ctx, "__phx_unhandled", (reason,)); }
    })));
    let cx = match Context::full(&rt) { Ok(c) => c, Err(e) => { log(ERROR, &format!("context: {e}")); return false } };
    let bundle = cfg.bundle.clone();
    if JS.set(Js { _rt: rt, cx, cfg, start: Instant::now() }).is_err() { return false; }
    let ok = with(|ctx| {
        if let Err(e) = install(&ctx).catch(&ctx) { log(ERROR, &format!("prelude: {e}")); return false; }
        info!("runtime up (rquickjs): {} gateway events hooked, plugins in {}", registry::names().len(), JS.get().unwrap().cfg.dir);
        call::<()>(&ctx, "__phx_loadAll", (bundle,));
        true
    }).unwrap_or(false);
    if !ok { return false; }
    registry::set_sink(Box::new(Sink));
    hook::jni::set_sink(bridge);
    hook::jni::set_picked(|id, n, t, e| push(Job::Picked(id, n, t, e)));
    hook::jni::set_confirmed(|id, ok| push(Job::Confirmed(id, ok)));
    std::thread::Builder::new().name("phoenix-js".into()).spawn(js_thread).is_ok()
}
