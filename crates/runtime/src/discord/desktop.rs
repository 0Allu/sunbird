//! Desktop Discord HTTP transport and request/response interception.
//!
//! Desktop Phoenix routes all Discord REST traffic through `libdiscord/crates/http`'s
//! `dyn Transport` (`ReqwestTransport::send(&self, req) -> Pin<Box<dyn Future>>`).
//! [`install`] locates both the `Transport::send` and `SendFuture::poll` vtable
//! slots in `.rdata` (build-independently via [`DesktopHttpTransport::locate`])
//! and swaps them with [`Image::swap_words`] — zero inline code patching.
//!
//! Captured session headers stay in memory only and `authorization` is never
//! logged or persisted.

use std::{
   borrow::Cow,
   collections::HashMap,
   ffi::c_void,
   panic::{
      self,
      AssertUnwindSafe,
   },
   ptr,
   slice,
   str,
   sync::{
      Arc,
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicPtr,
         AtomicUsize,
         Ordering,
      },
      mpsc,
   },
   time::{
      Duration,
      Instant,
   },
};

use phoenix_hook::image::Image;
#[cfg(windows)] use phoenix_sigs::http::DesktopHttpTransport;

use super::{
   Authorization,
   DiscordError,
   REQUEST_HOOK_COUNT,
   RESPONSE_HOOK_COUNT,
   RequestAction,
   api_base,
   api_path,
   matching_request_hooks,
   matching_response_hooks,
};
use crate::fetch::{
   self,
   Request,
   Response,
};

/// Headers set per-request by Discord that plugin API calls must not inherit.
const PER_REQUEST: [&str; 4] = [
   "content-type",
   "content-length",
   "x-context-properties",
   "x-audit-log-reason",
];

/// Standard HTTP/1.x `http::header::StandardHeader` names in discriminant
/// order (`0..=78`), matching `Discord.exe`'s compiled `http` crate.
const STANDARD_HEADERS: [&str; 79] = [
   "accept",
   "accept-charset",
   "accept-encoding",
   "accept-language",
   "accept-ranges",
   "access-control-allow-credentials",
   "access-control-allow-headers",
   "access-control-allow-methods",
   "access-control-allow-origin",
   "access-control-expose-headers",
   "access-control-max-age",
   "access-control-request-headers",
   "access-control-request-method",
   "age",
   "alt-svc",
   "authorization",
   "cache-control",
   "cache-status",
   "cdn-cache-control",
   "connection",
   "content-disposition",
   "content-encoding",
   "content-language",
   "content-length",
   "content-location",
   "content-range",
   "content-security-policy",
   "content-security-policy-report-only",
   "content-type",
   "cookie",
   "dnt",
   "date",
   "etag",
   "expect",
   "expires",
   "forwarded",
   "from",
   "host",
   "if-match",
   "if-modified-since",
   "if-none-match",
   "if-range",
   "if-unmodified-since",
   "last-modified",
   "link",
   "max-forwards",
   "origin",
   "pragma",
   "proxy-authenticate",
   "proxy-authorization",
   "public-key-pins",
   "public-key-pins-report-only",
   "range",
   "referer",
   "referrer-policy",
   "refresh",
   "retry-after",
   "sec-websocket-accept",
   "sec-websocket-extensions",
   "sec-websocket-key",
   "sec-websocket-protocol",
   "sec-websocket-version",
   "server",
   "set-cookie",
   "strict-transport-security",
   "te",
   "trailer",
   "transfer-encoding",
   "user-agent",
   "upgrade",
   "upgrade-insecure-requests",
   "vary",
   "via",
   "warning",
   "www-authenticate",
   "x-content-type-options",
   "x-dns-prefetch-control",
   "x-frame-options",
   "x-xss-protection",
];

const STANDARD_METHODS: [&str; 10] = [
   "OPTIONS", "GET", "POST", "PUT", "DELETE", "HEAD", "TRACE", "CONNECT", "PATCH", "QUERY",
];

#[derive(Clone)]
struct Session {
   base:    String,
   headers: Vec<(String, String)>,
}

struct InFlight {
   method: String,
   path:   String,
   hooks:  Vec<u64>,
   at:     Instant,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static IN_FLIGHT: LazyLock<Mutex<HashMap<usize, InFlight>>> = LazyLock::new(Mutex::default);
static IN_FLIGHT_COUNT: AtomicUsize = AtomicUsize::new(0);

// ---------------------------------------------------------------------------
// ABI mirror of Desktop Phoenix's `libdiscord/crates/http` transport types
// (verified against Windows x86_64 `Discord.exe`).
// ---------------------------------------------------------------------------

#[repr(C)]
struct RawVec {
   cap: usize,
   ptr: *mut u8,
   len: usize,
}

#[repr(C)]
struct RawBytesVtable {
   clone:     unsafe extern "C" fn(&AtomicPtr<()>, *const u8, usize) -> RawBytes,
   to_vec:    unsafe extern "C" fn(*mut (), *const u8, usize) -> RawVec,
   to_mut:    unsafe extern "C" fn(*mut (), *const u8, usize) -> RawBytes,
   is_unique: unsafe extern "C" fn(&AtomicPtr<()>) -> bool,
   drop:      unsafe extern "C" fn(*mut (), *const u8, usize),
}

#[repr(C)]
struct RawBytes {
   vtable: *const RawBytesVtable,
   ptr:    *const u8,
   len:    usize,
   data:   AtomicPtr<()>,
}

#[repr(C)]
struct RawHeaderBucket {
   tag:   u64,
   _pad0: [u64; 2],
   value: RawBytes,
   _sens: u64,
   name:  RawBytes,
   _pad1: u64,
}
const _: () = assert!(size_of::<RawHeaderBucket>() == 0x68);

#[repr(C)]
struct RawHeaderMap {
   _head:       [u64; 3],
   entries_cap: usize,
   entries_ptr: *const RawHeaderBucket,
   entries_len: usize,
   _tail:       [u64; 6],
}
const _: () = assert!(size_of::<RawHeaderMap>() == 0x60);

#[repr(C)]
struct RawBody {
   tag:   u64,
   bytes: RawBytes,
   _rest: [u64; 2],
}
const _: () = assert!(size_of::<RawBody>() == 0x38);

#[repr(C)]
struct RawMethod {
   bytes: [u8; 24],
}
const _: () = assert!(size_of::<RawMethod>() == 0x18);

#[repr(C)]
struct RawTransportRequest {
   headers:  RawHeaderMap,
   body:     RawBody,
   url:      RawVec,
   _timeout: [u64; 2],
   method:   RawMethod,
   _extra:   [u64; 3],
}
const _: () = assert!(size_of::<RawTransportRequest>() == 0xF0);

#[repr(C)]
#[derive(Clone, Copy)]
struct RawPinnedFuture {
   data:   *mut c_void,
   vtable: *const c_void,
}

#[repr(C)]
struct RawPollResult {
   headers: [u64; 12],
   body:    RawVec,
   status:  u16,
   _pad:    [u8; 6],
}
const _: () = assert!(size_of::<RawPollResult>() == 0x80);

impl RawPollResult {
   const PENDING_TAG: u64 = u64::MAX - 1;
   const ERR_TAG: u64 = u64::MAX;

   const fn is_pending(&self) -> bool {
      self.headers[0] == Self::PENDING_TAG
   }

   const fn is_ok(&self) -> bool {
      self.headers[0] != Self::PENDING_TAG && self.headers[0] != Self::ERR_TAG
   }
}

type SendFn = unsafe extern "C" fn(*const c_void, *mut RawTransportRequest) -> RawPinnedFuture;
type PollFn = unsafe extern "C" fn(*mut c_void, *mut c_void) -> RawPollResult;
type HostAllocFn = unsafe extern "C" fn(usize, usize) -> *mut u8;
type HostDeallocFn = unsafe extern "C" fn(*mut u8, usize, usize);

#[cfg_attr(not(windows), allow(dead_code))]
struct Installed {
   send:         SendFn,
   poll:         PollFn,
   host_alloc:   Option<HostAllocFn>,
   host_dealloc: Option<HostDeallocFn>,
}

static INSTALLED: OnceLock<Installed> = OnceLock::new();

// ---------------------------------------------------------------------------
// Allocator-isolated `bytes::Bytes` vtable for replaced request bodies.
// ---------------------------------------------------------------------------

static OWNED_BYTES_VTABLE: RawBytesVtable = RawBytesVtable {
   clone:     owned_bytes_clone,
   to_vec:    owned_bytes_to_vec,
   to_mut:    owned_bytes_to_mut,
   is_unique: owned_bytes_is_unique,
   drop:      owned_bytes_drop,
};

fn make_owned_bytes(data: Vec<u8>) -> RawBytes {
   let arc = Arc::new(data);
   let ptr = arc.as_ptr();
   let len = arc.len();
   let raw = Arc::into_raw(arc).cast_mut().cast::<()>();
   RawBytes {
      vtable: &raw const OWNED_BYTES_VTABLE,
      ptr,
      len,
      data: AtomicPtr::new(raw),
   }
}

unsafe extern "C" fn owned_bytes_clone(
   data: &AtomicPtr<()>,
   ptr: *const u8,
   len: usize,
) -> RawBytes {
   let raw = data.load(Ordering::Relaxed);
   if !raw.is_null() {
      // SAFETY: `raw` came from `Arc::into_raw` in `make_owned_bytes`.
      unsafe { Arc::increment_strong_count(raw.cast::<Vec<u8>>()) };
   }
   RawBytes {
      vtable: &raw const OWNED_BYTES_VTABLE,
      ptr,
      len,
      data: AtomicPtr::new(raw),
   }
}

unsafe extern "C" fn owned_bytes_to_vec(data: *mut (), ptr: *const u8, len: usize) -> RawVec {
   let out = if len > 0
      && !ptr.is_null()
      && let Some(alloc) = INSTALLED.get().and_then(|inst| inst.host_alloc)
   {
      // SAFETY: allocate using the host's own `__rust_alloc(len, 1)`.
      let dst = unsafe { alloc(len, 1) };
      if dst.is_null() {
         RawVec {
            cap: 0,
            ptr: ptr::null_mut(),
            len: 0,
         }
      } else {
         // SAFETY: `ptr` is valid for `len` bytes and `dst` is freshly allocated.
         unsafe { ptr::copy_nonoverlapping(ptr, dst, len) };
         RawVec {
            cap: len,
            ptr: dst,
            len,
         }
      }
   } else {
      RawVec {
         cap: 0,
         ptr: ptr::null_mut(),
         len: 0,
      }
   };
   // SAFETY: `to_vec` consumes the `Bytes` instance.
   unsafe { owned_bytes_drop(data, ptr, len) };
   out
}

unsafe extern "C" fn owned_bytes_to_mut(data: *mut (), ptr: *const u8, len: usize) -> RawBytes {
   // Fallback: keep our owned bytes intact; callers only invoke `to_mut` if
   // they try to mutate a request body after `send`, which `reqwest` does not.
   RawBytes {
      vtable: &raw const OWNED_BYTES_VTABLE,
      ptr,
      len,
      data: AtomicPtr::new(data),
   }
}

unsafe extern "C" fn owned_bytes_is_unique(_data: &AtomicPtr<()>) -> bool {
   false
}

unsafe extern "C" fn owned_bytes_drop(data: *mut (), _ptr: *const u8, _len: usize) {
   if !data.is_null() {
      // SAFETY: matches the `Arc::into_raw` / `increment_strong_count` pair.
      drop(unsafe { Arc::from_raw(data.cast::<Vec<u8>>()) });
   }
}

// ---------------------------------------------------------------------------
// Vtable installation & detours
// ---------------------------------------------------------------------------

/// Locates Desktop Phoenix's `ReqwestTransport` and `SendFuture` vtable slots
/// and installs request/response interception.
pub fn install(image: &Image<'static>) {
   #[cfg(windows)]
   install_windows(image);
   #[cfg(not(windows))]
   let _ = image;
}

#[cfg(windows)]
fn install_windows(image: &Image<'static>) {
   let Some(found) = DesktopHttpTransport::locate(image) else {
      log::warn!("discord desktop: HTTP transport vtable not found");
      return;
   };
   // SAFETY: `DesktopHttpTransport::locate` verified every target against `.pdata`.
   let installed = Installed {
      send:         unsafe { std::mem::transmute::<usize, SendFn>(found.send_fn) },
      poll:         unsafe { std::mem::transmute::<usize, PollFn>(found.poll_fn) },
      host_alloc:   found
         .host_alloc
         .map(|addr| unsafe { std::mem::transmute::<usize, HostAllocFn>(addr) }),
      host_dealloc: found
         .host_dealloc
         .map(|addr| unsafe { std::mem::transmute::<usize, HostDeallocFn>(addr) }),
   };
   if INSTALLED.set(installed).is_err() {
      return;
   }

   let mut writes = Vec::with_capacity(found.send_slots.len() + found.poll_slots.len());
   for &slot in &found.send_slots {
      writes.push((slot, send_detour as *const () as usize));
   }
   for &slot in &found.poll_slots {
      writes.push((slot, poll_detour as *const () as usize));
   }
   // SAFETY: every slot is an aligned `.rdata` vtable entry pointing to `send_fn`
   // or `poll_fn`, and both detours match their exact Windows x64 ABI.
   match unsafe { image.swap_words(&writes) } {
      Ok(_) => {
         log::info!(
            "discord desktop: hooked HTTP transport ({} send, {} poll vtable slots)",
            found.send_slots.len(),
            found.poll_slots.len(),
         );
      },
      Err(err) => {
         log::error!("discord desktop: failed to swap HTTP vtable slots: {err}");
      },
   }
}

/// # Safety
///
/// Reads a live `RawVec` (Rust `String`) owned by the host.
unsafe fn read_str<'a>(raw: &RawVec) -> Option<&'a str> {
   if raw.len == 0 {
      return Some("");
   }
   if raw.ptr.is_null() {
      return None;
   }
   // SAFETY: host guarantees valid UTF-8 `String` bytes.
   str::from_utf8(unsafe { slice::from_raw_parts(raw.ptr, raw.len) }).ok()
}

/// # Safety
///
/// Reads a live `RawBytes` owned by the host.
unsafe fn read_bytes<'a>(raw: &RawBytes) -> &'a [u8] {
   if raw.len == 0 || raw.ptr.is_null() {
      return &[];
   }
   // SAFETY: host guarantees `raw.ptr` is valid for `raw.len` bytes.
   unsafe { slice::from_raw_parts(raw.ptr, raw.len) }
}

/// # Safety
///
/// Reads a live `http::Method` owned by the host.
unsafe fn read_method(raw: &RawMethod) -> Cow<'static, str> {
   let tag = raw.bytes[0];
   if let Some(&name) = STANDARD_METHODS.get(usize::from(tag)) {
      return Cow::Borrowed(name);
   }
   if tag == 10 {
      let len = usize::from(raw.bytes[16]).min(15);
      return Cow::Owned(String::from_utf8_lossy(&raw.bytes[1..1 + len]).into_owned());
   }
   if tag == 11 {
      let ptr = usize::from_le_bytes(raw.bytes[8..16].try_into().unwrap_or([0; 8])) as *const u8;
      let len = usize::from_le_bytes(raw.bytes[16..24].try_into().unwrap_or([0; 8]));
      if !ptr.is_null() && len <= 64 {
         // SAFETY: `ExtensionAllocated` stores a valid UTF-8 `Box<str>`.
         let bytes = unsafe { slice::from_raw_parts(ptr, len) };
         return Cow::Owned(String::from_utf8_lossy(bytes).into_owned());
      }
   }
   Cow::Borrowed("GET")
}

/// # Safety
///
/// Reads header `(name, value)` pairs from the host's `http::HeaderMap`.
unsafe fn read_headers(map: &RawHeaderMap) -> Vec<(String, String)> {
   if map.entries_ptr.is_null() || map.entries_len == 0 || map.entries_len > 256 {
      return Vec::new();
   }
   // SAFETY: `entries_ptr` holds `entries_len` initialized `RawHeaderBucket`s.
   let buckets = unsafe { slice::from_raw_parts(map.entries_ptr, map.entries_len) };
   let mut out = Vec::with_capacity(buckets.len());
   for bucket in buckets {
      if bucket.tag == 2 {
         continue;
      }
      let name = if bucket.name.vtable.is_null() {
         let idx = (bucket.name.ptr as usize) & 0xFF;
         let Some(&std_name) = STANDARD_HEADERS.get(idx) else {
            continue;
         };
         std_name.to_owned()
      } else {
         // SAFETY: non-null vtable means `Custom(Bytes)` holding valid ASCII header name.
         let bytes = unsafe { read_bytes(&bucket.name) };
         let Ok(text) = str::from_utf8(bytes) else {
            continue;
         };
         text.to_owned()
      };
      // SAFETY: `bucket.value` is a live `HeaderValue` starting with `Bytes`.
      let val_bytes = unsafe { read_bytes(&bucket.value) };
      if let Ok(value) = str::from_utf8(val_bytes) {
         out.push((name, value.to_owned()));
      }
   }
   out
}

#[cfg(windows)]
unsafe extern "C" fn send_detour(
   self_ptr: *const c_void,
   req: *mut RawTransportRequest,
) -> RawPinnedFuture {
   let installed = INSTALLED
      .get()
      .expect("send_detour only runs after INSTALLED is set");

   let mut response_track: Option<(String, String, Vec<u64>)> = None;

   if !req.is_null() {
      let _ = panic::catch_unwind(AssertUnwindSafe(|| {
         // SAFETY: `req` is a live owned `TransportRequest` passed by `libdiscord_http`.
         let req_ref = unsafe { &mut *req };
         // SAFETY: reading live fields of `req_ref`.
         let url = unsafe { read_str(&req_ref.url) }.unwrap_or_default().to_owned();
         let method = unsafe { read_method(&req_ref.method) }.into_owned();
         let headers = unsafe { read_headers(&req_ref.headers) };
         let body = if req_ref.body.tag == 1 {
            // SAFETY: `tag == 1` is `Body::Bytes(RawBytes)`.
            unsafe { read_bytes(&req_ref.body.bytes) }
         } else {
            &[]
         };

         match observe_request(&method, &url, &headers, body) {
            RequestAction::Pass => {},
            RequestAction::Drop => {
               // Zeroing `url.len` keeps the host's `String` capacity/pointer
               // intact so `Url::parse("")` fails cleanly with zero network I/O
               // and the host drops every owned field with its own allocator.
               req_ref.url.len = 0;
               return;
            },
            RequestAction::Replace(new_body) => {
               if req_ref.body.tag == 1 && !req_ref.body.bytes.vtable.is_null() {
                  let old_vtable = req_ref.body.bytes.vtable;
                  let old_ptr = req_ref.body.bytes.ptr;
                  let old_len = req_ref.body.bytes.len;
                  let old_data = req_ref.body.bytes.data.load(Ordering::Relaxed);
                  // SAFETY: drops the old `Bytes` via its own vtable.
                  unsafe { ((*old_vtable).drop)(old_data, old_ptr, old_len) };
                  req_ref.body.bytes = make_owned_bytes(new_body);
               } else if req_ref.body.tag == 0 {
                  req_ref.body.tag = 1;
                  req_ref.body.bytes = make_owned_bytes(new_body);
               }
            },
         }

         if RESPONSE_HOOK_COUNT.load(Ordering::Relaxed) > 0
            && let Some(path) = api_path(&url)
         {
            let matched = matching_response_hooks(&method, path);
            if !matched.is_empty() {
               response_track = Some((method, path.to_owned(), matched));
            }
         }
      }));
   }

   // SAFETY: forward directly to the original `ReqwestTransport::send`.
   let future = unsafe { (installed.send)(self_ptr, req) };

   if let Some((method, path, hooks)) = response_track
      && !future.data.is_null()
   {
      let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
      if in_flight.len() >= 64 {
         in_flight.retain(|_, entry| entry.at.elapsed() < Duration::from_secs(60));
      }
      in_flight.insert(future.data as usize, InFlight {
         method,
         path,
         hooks,
         at: Instant::now(),
      });
      IN_FLIGHT_COUNT.store(in_flight.len(), Ordering::Relaxed);
   }

   future
}

#[cfg(windows)]
unsafe extern "C" fn poll_detour(future: *mut c_void, cx: *mut c_void) -> RawPollResult {
   let installed = INSTALLED
      .get()
      .expect("poll_detour only runs after INSTALLED is set");
   // SAFETY: forward to original `SendFuture::poll`.
   let mut result = unsafe { (installed.poll)(future, cx) };

   if result.is_pending() || IN_FLIGHT_COUNT.load(Ordering::Relaxed) == 0 {
      return result;
   }

   let tracked = {
      let mut in_flight = IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
      let entry = in_flight.remove(&(future as usize));
      IN_FLIGHT_COUNT.store(in_flight.len(), Ordering::Relaxed);
      entry
   };

   if let Some(in_flight) = tracked
      && result.is_ok()
   {
      let _ = panic::catch_unwind(AssertUnwindSafe(|| {
         let old_body = if result.body.len == 0 || result.body.ptr.is_null() {
            Vec::new()
         } else {
            // SAFETY: `Poll::Ready(Ok(resp)).body` is a live `Vec<u8>`.
            unsafe { slice::from_raw_parts(result.body.ptr, result.body.len) }.to_vec()
         };
         let orig_len = old_body.len();
         let new_body = run_response_hooks_sync(
            in_flight,
            i32::from(result.status),
            old_body,
         );
         // SAFETY: `result.body` is owned by the Ready response being returned.
         unsafe {
            replace_host_vec(&mut result.body, orig_len, &new_body, installed);
         }
      }));
   }

   result
}

/// Replaces `vec`'s contents with `new_body` while staying 100% compatible
/// with `Discord.exe`'s custom `#[global_allocator]`.
///
/// # Safety
///
/// `vec` is an owned host `Vec<u8>`.
#[cfg(windows)]
unsafe fn replace_host_vec(
   vec: &mut RawVec,
   orig_len: usize,
   new_body: &[u8],
   installed: &Installed,
) {
   // Fast path: unchanged length and identical bytes.
   if new_body.len() == orig_len
      && (orig_len == 0
         || (!vec.ptr.is_null()
            // SAFETY: `vec.ptr` is valid for `orig_len` bytes.
            && unsafe { slice::from_raw_parts(vec.ptr, orig_len) } == new_body))
   {
      return;
   }
   // Fits inside existing host capacity: overwrite in place with zero alloc/dealloc.
   if new_body.len() <= vec.cap && !vec.ptr.is_null() {
      // SAFETY: `vec.ptr` has capacity >= `new_body.len()`.
      unsafe { ptr::copy_nonoverlapping(new_body.as_ptr(), vec.ptr, new_body.len()) };
      vec.len = new_body.len();
      return;
   }
   // Needs growth: use the host's own `__rust_alloc` and `__rust_dealloc`.
   let (Some(alloc), Some(dealloc)) = (installed.host_alloc, installed.host_dealloc) else {
      log::warn!("discord onResponse: cannot grow response body without host allocator");
      return;
   };
   let new_len = new_body.len();
   // SAFETY: calls host's `__rust_alloc(new_len, 1)`.
   let new_ptr = unsafe { alloc(new_len, 1) };
   if new_ptr.is_null() {
      return;
   }
   // SAFETY: copies `new_len` bytes into freshly allocated host buffer.
   unsafe { ptr::copy_nonoverlapping(new_body.as_ptr(), new_ptr, new_len) };
   if vec.cap > 0 && !vec.ptr.is_null() {
      // SAFETY: frees old host buffer via host's `__rust_dealloc(ptr, cap, 1)`.
      unsafe { dealloc(vec.ptr, vec.cap, 1) };
   }
   vec.cap = new_len;
   vec.ptr = new_ptr;
   vec.len = new_len;
}

#[cfg(windows)]
fn run_response_hooks_sync(in_flight: InFlight, status: i32, body: Vec<u8>) -> Vec<u8> {
   let fallback = body.clone();
   let (sender, receiver) = mpsc::channel::<Vec<u8>>();
   crate::intercept_response(
      in_flight.hooks,
      in_flight.method.clone(),
      in_flight.path.clone(),
      status,
      body,
      sender,
   );
   receiver
      .recv_timeout(Duration::from_secs(4))
      .unwrap_or_else(|_| {
         log::warn!(
            "discord onResponse: {} {} timed out, delivering original",
            in_flight.method,
            in_flight.path
         );
         fallback
      })
}

// ---------------------------------------------------------------------------
// High-level session capture, hook dispatch & `phoenix.discord.*` transport
// ---------------------------------------------------------------------------

/// Extracts the channel id from a send-message API path such as
/// `/channels/123456789/messages` (ignoring any query string).
#[must_use]
pub fn send_channel_id(path: &str) -> Option<&str> {
   let clean = path.split(['?', '#']).next().unwrap_or(path);
   let rest = clean.strip_prefix("/channels/")?;
   let id = rest.strip_suffix("/messages")?;
   (!id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit())).then_some(id)
}

/// Records Discord's API base and session headers from an observed desktop
/// request, in memory only. Never logs `authorization`.
pub fn record_session(url: &str, headers: &[(String, String)]) {
   let Some(base) = api_base(url) else {
      return;
   };
   let filtered = headers
      .iter()
      .filter(|(name, _)| !PER_REQUEST.contains(&name.to_ascii_lowercase().as_str()))
      .cloned()
      .collect::<Vec<_>>();
   if !filtered
      .iter()
      .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
   {
      return;
   }
   let mut session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
   if session.is_none() {
      log::info!("discord: API session found at {base}");
   }
   *session = Some(Session {
      base:    base.to_owned(),
      headers: filtered,
   });
}

/// Runs matching `phoenix.messages.onSend` and `phoenix.discord.onRequest`
/// hooks on an outgoing desktop Discord API request.
pub fn observe_request(
   method: &str,
   url: &str,
   headers: &[(String, String)],
   body: &[u8],
) -> RequestAction {
   record_session(url, headers);
   let Some(path) = api_path(url) else {
      return RequestAction::Pass;
   };

   let mut current = body;
   let mut rewritten: Option<Vec<u8>> = None;

   if method.eq_ignore_ascii_case("POST")
      && let Some(channel_id) = send_channel_id(path)
   {
      match crate::intercept_send_body(channel_id, current) {
         RequestAction::Pass => {},
         RequestAction::Drop => return RequestAction::Drop,
         RequestAction::Replace(next) => {
            rewritten = Some(next);
            current = rewritten.as_deref().unwrap_or(body);
         },
      }
   }

   if REQUEST_HOOK_COUNT.load(Ordering::Relaxed) > 0 {
      let matched = matching_request_hooks(method, path);
      if !matched.is_empty() {
         match crate::intercept_request(&matched, method, path, current) {
            RequestAction::Pass => {},
            RequestAction::Drop => return RequestAction::Drop,
            RequestAction::Replace(next) => return RequestAction::Replace(next),
         }
      }
   }

   rewritten.map_or(RequestAction::Pass, RequestAction::Replace)
}

pub fn send(
   method: &str,
   path: &str,
   headers: Vec<(String, String)>,
   body: Vec<u8>,
) -> Result<Response, DiscordError> {
   if !path.starts_with('/') {
      return Err(DiscordError::Path {
         path: path.to_owned(),
      });
   }
   let Session {
      base,
      headers: mut merged,
   } = SESSION
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
      .ok_or(DiscordError::NoSession)?;

   merged.retain(|(name, _)| {
      !headers
         .iter()
         .any(|(own, _)| own.eq_ignore_ascii_case(name))
   });
   merged.extend(headers);

   let request = Request {
      url:     format!("{base}{path}"),
      method:  method.to_owned(),
      headers: merged,
      body:    (!body.is_empty()).then_some(body),
   };
   fetch::send(&request).map_err(|err| DiscordError::Network {
      path:    path.to_owned(),
      message: err.to_string(),
   })
}

pub fn authorize(_request: &Authorization) -> Result<String, DiscordError> {
   Err(DiscordError::NoBridge)
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn extracts_send_channel_id() {
      assert_eq!(
         send_channel_id("/channels/123456789012345678/messages"),
         Some("123456789012345678")
      );
      assert_eq!(
         send_channel_id("/channels/42/messages?nonce=1"),
         Some("42")
      );
      assert_eq!(send_channel_id("/channels/abc/messages"), None);
      assert_eq!(send_channel_id("/channels/42/typing"), None);
      assert_eq!(send_channel_id("/users/@me"), None);
   }

   #[test]
   fn rejects_relative_path_without_leading_slash() {
      let err = send("GET", "users/@me", Vec::new(), Vec::new());
      assert!(matches!(err, Err(DiscordError::Path { .. })));
   }

   #[test]
   fn standard_header_indices_match_http_crate() {
      assert_eq!(STANDARD_HEADERS[0], "accept");
      assert_eq!(STANDARD_HEADERS[15], "authorization");
      assert_eq!(STANDARD_HEADERS[28], "content-type");
      assert_eq!(STANDARD_HEADERS[29], "cookie");
      assert_eq!(STANDARD_HEADERS[68], "user-agent");
      assert_eq!(STANDARD_HEADERS[78], "x-xss-protection");
   }

   #[test]
   fn owned_bytes_clone_and_drop_roundtrip() {
      let raw = make_owned_bytes(b"hello sunbird".to_vec());
      assert_eq!(raw.len, 13);
      // SAFETY: testing our own vtable clone + double drop.
      unsafe {
         let cloned = ((*raw.vtable).clone)(&raw.data, raw.ptr, raw.len);
         assert_eq!(read_bytes(&cloned), b"hello sunbird");
         ((*raw.vtable).drop)(
            raw.data.load(Ordering::Relaxed),
            raw.ptr,
            raw.len,
         );
         ((*cloned.vtable).drop)(
            cloned.data.load(Ordering::Relaxed),
            cloned.ptr,
            cloned.len,
         );
      }
   }
}
