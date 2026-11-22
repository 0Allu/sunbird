//! phoenix.fetch: java.net.HttpURLConnection over JNI on a short-lived thread (no TLS stack of our own).
use hook::jcall;
use hook::jni::{clear_exc, env, get_bytes, jni_sys::*, new_bytes};
use std::ffi::CString;
use std::sync::atomic::{AtomicI32, Ordering::Relaxed};

pub struct Req {
    pub url: String,
    pub method: String,
    pub headers: String, // "Key: value\n" lines
    pub body: Option<String>,
}

/// Starts a request; `done(id, status, body)` runs on the fetch thread (status -1: failed).
pub fn start(r: Req, done: fn(i32, i32, Option<String>)) -> i32 {
    static ID: AtomicI32 = AtomicI32::new(0);
    let id = ID.fetch_add(1, Relaxed) + 1;
    let ok = std::thread::Builder::new().name("phoenix-fetch".into()).spawn(move || {
        let (status, body) = unsafe { run(&r) }.unwrap_or((-1, None));
        unsafe { hook::jni::detach() };
        done(id, status, body);
    });
    if ok.is_ok() { id } else { -1 }
}

unsafe fn run(r: &Req) -> Option<(i32, Option<String>)> {
    let env = env()?;
    let s = |x: &str| { let c = CString::new(x).unwrap_or_default(); jcall!(env, NewStringUTF, c.as_ptr()) };
    let mid = |c: jclass, n: &std::ffi::CStr, sig: &std::ffi::CStr| jcall!(env, GetMethodID, c, n.as_ptr(), sig.as_ptr());
    let fail = |env| { clear_exc(env); None };

    let c_url = jcall!(env, FindClass, c"java/net/URL".as_ptr());
    let url = jcall!(env, NewObjectA, c_url, mid(c_url, c"<init>", c"(Ljava/lang/String;)V"), [jvalue { l: s(&r.url) }].as_ptr());
    if clear_exc(env) { return fail(env); }
    let conn = jcall!(env, CallObjectMethodA, url, mid(c_url, c"openConnection", c"()Ljava/net/URLConnection;"), std::ptr::null());
    if clear_exc(env) || conn.is_null() { return fail(env); }
    let c_conn = jcall!(env, FindClass, c"java/net/HttpURLConnection".as_ptr());
    let void = |m: jmethodID, a: &[jvalue]| jcall!(env, CallVoidMethodA, conn, m, a.as_ptr());
    void(mid(c_conn, c"setRequestMethod", c"(Ljava/lang/String;)V"), &[jvalue { l: s(&r.method) }]);
    void(mid(c_conn, c"setConnectTimeout", c"(I)V"), &[jvalue { i: 15000 }]);
    void(mid(c_conn, c"setReadTimeout", c"(I)V"), &[jvalue { i: 30000 }]);
    let set = mid(c_conn, c"setRequestProperty", c"(Ljava/lang/String;Ljava/lang/String;)V");
    for line in r.headers.lines() {
        if let Some((k, v)) = line.split_once(':') { void(set, &[jvalue { l: s(k.trim()) }, jvalue { l: s(v.trim()) }]); }
    }
    if clear_exc(env) { return fail(env); }
    if let Some(b) = r.body.as_deref().filter(|b| !b.is_empty()) {
        void(mid(c_conn, c"setDoOutput", c"(Z)V"), &[jvalue { z: JNI_TRUE }]);
        let os = jcall!(env, CallObjectMethodA, conn, mid(c_conn, c"getOutputStream", c"()Ljava/io/OutputStream;"), std::ptr::null());
        if clear_exc(env) || os.is_null() { return fail(env); }
        let c_os = jcall!(env, FindClass, c"java/io/OutputStream".as_ptr());
        jcall!(env, CallVoidMethodA, os, mid(c_os, c"write", c"([B)V"), [jvalue { l: new_bytes(env, b.as_bytes()) }].as_ptr());
        jcall!(env, CallVoidMethodA, os, mid(c_os, c"close", c"()V"), std::ptr::null());
        if clear_exc(env) { return fail(env); }
    }
    let status = jcall!(env, CallIntMethodA, conn, mid(c_conn, c"getResponseCode", c"()I"), std::ptr::null());
    if clear_exc(env) { return fail(env); }
    let get = if status < 400 { c"getInputStream" } else { c"getErrorStream" };
    let is = jcall!(env, CallObjectMethodA, conn, mid(c_conn, get, c"()Ljava/io/InputStream;"), std::ptr::null());
    if clear_exc(env) || is.is_null() { return Some((status, None)); }
    let c_is = jcall!(env, FindClass, c"java/io/InputStream".as_ptr());
    let read = mid(c_is, c"read", c"([B)I");
    let buf = jcall!(env, NewByteArray, 65536);
    let mut out = Vec::new();
    loop {
        let k = jcall!(env, CallIntMethodA, is, read, [jvalue { l: buf }].as_ptr());
        if clear_exc(env) || k <= 0 || out.len() > 32 << 20 { break; }
        out.extend(get_bytes(env, buf, Some(k)));
    }
    Some((status, Some(String::from_utf8_lossy(&out).into_owned())))
}
