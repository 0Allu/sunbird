//! Discord's HTTP API through Phoenix's own `Networking` host module, so a
//! plugin's request leaves on the app's OkHttp client with the app's session
//! headers.
//!
//! - Phoenix's Rust side sets every header, `authorization` included, and Java
//!   only sends them. [`observe_call`] keeps the latest set and the API base
//!   from Phoenix's own requests, in memory only.
//! - [`send`] issues `Networking.request` with ids from [`FIRST_ID`], and
//!   [`filter`] takes the `@http` replies for those ids out of the dispatch
//!   stream before Phoenix sees them.
//!
//! Module, method and field numbers index the host's name table `hh.jb`,
//! versionCode 356207.

use std::{
   collections::HashMap,
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
      atomic::{
         AtomicI32,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   time::Duration,
};

use phoenix_wire::{
   DispatchEvent,
   HostCall,
   Key,
   Value,
   decode_dispatch,
   without_frames,
};

use crate::{
   bridge::{
      self,
      Action,
   },
   fetch::Response,
};

const NETWORKING: u16 = 43;
const REQUEST: u16 = 168;
const HTTP_EVENT: u16 = 321;

const ERROR: Key = Key::new(403);
const ID: Key = Key::new(417);
const METHOD: Key = Key::new(421);
const URL: Key = Key::new(422);
const HEADERS: Key = Key::new(423);
const STATUS: Key = Key::new(430);

/// Far above the counter Phoenix numbers its own requests with.
const FIRST_ID: i32 = 0x5000_0000;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Headers Phoenix sets per request, which a plugin's request must not
/// inherit.
const PER_REQUEST: [&str; 4] = [
   "content-type",
   "content-length",
   "x-context-properties",
   "x-audit-log-reason",
];

#[derive(Debug, flub::Error)]
pub enum DiscordError {
   #[error("Phoenix has not made an API request yet, so there is no session")]
   NoSession,
   #[error("{path}: not a path under the API base")]
   Path { path: String },
   #[error("Phoenix's bridge is not up yet")]
   NoBridge,
   #[error("{path}: no answer in {} seconds", TIMEOUT.as_secs())]
   Timeout { path: String },
   #[error("{path}: {message}")]
   Network { path: String, message: String },
}

/// What Phoenix's own API requests carry.
struct Session {
   /// Up to and including the version, like `https://discord.com/api/v9`.
   base:    String,
   headers: Vec<(String, String)>,
}

/// `@http`'s fields and body.
struct Reply {
   status: i32,
   error:  Option<String>,
   body:   Vec<u8>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);
static PENDING: LazyLock<Mutex<HashMap<i32, Sender<Reply>>>> = LazyLock::new(Mutex::default);
static NEXT_ID: AtomicI32 = AtomicI32::new(FIRST_ID);

/// `Networking.request`'s `headers`, an array of `[name, value]` pairs.
fn headers(args: &Value) -> Vec<(String, String)> {
   let Some(Value::Arr(pairs)) = args.field(HEADERS) else {
      return Vec::new();
   };
   pairs
      .iter()
      .filter_map(|pair| {
         let Value::Arr(parts) = pair else {
            return None;
         };
         match parts.as_slice() {
            [Value::Str(name), Value::Str(value)] => Some((name.clone(), value.clone())),
            _ => None,
         }
      })
      .collect()
}

/// `https://host/api/v9` out of an API URL.
fn api_base(url: &str) -> Option<&str> {
   let after = url.find("/api/v")? + "/api/v".len();
   let digits = url[after..]
      .find(|char: char| !char.is_ascii_digit())
      .unwrap_or(url.len() - after);
   (url.starts_with("https://") && digits > 0).then(|| &url[..after + digits])
}

/// Whether an `invoke` frame starting with `head` is one [`observe_call`]
/// reads.
pub fn wants(head: &[u8]) -> bool {
   HostCall::target(head) == Some((NETWORKING, REQUEST))
}

/// Keeps the session of one of Phoenix's own API requests.
pub fn observe_call(frame: &[u8]) {
   let Ok(call) = HostCall::decode(frame) else {
      return;
   };
   let Some(base) = call
      .args
      .field(URL)
      .and_then(Value::as_str)
      .and_then(api_base)
   else {
      return;
   };
   let headers = headers(&call.args)
      .into_iter()
      .filter(|(name, _)| !PER_REQUEST.contains(&name.to_ascii_lowercase().as_str()))
      .collect::<Vec<_>>();
   if !headers
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
      base: base.to_owned(),
      headers,
   });
}

/// Takes the `@http` replies to phoenix's requests out of a `nativeDispatch`
/// call.
pub fn filter(bytes: &[u8]) -> Action {
   if PENDING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_empty()
   {
      return Action::Pass;
   }
   let Ok(items) = decode_dispatch(bytes) else {
      return Action::Pass;
   };
   let count = items.len();
   let taken = items
      .into_iter()
      .filter_map(|item| {
         let DispatchEvent::Host {
            event: HTTP_EVENT,
            args,
            body,
         } = item.event
         else {
            return None;
         };
         let id = args
            .field(ID)
            .and_then(Value::as_i32)
            .filter(|id| *id >= FIRST_ID)?;
         let waiter = PENDING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
         // With no waiter the request timed out, and the reply goes nowhere.
         if let Some(waiter) = waiter {
            let _ = waiter.send(Reply {
               status: args
                  .field(STATUS)
                  .and_then(Value::as_i32)
                  .unwrap_or_default(),
               error: args.field(ERROR).and_then(Value::as_str).map(str::to_owned),
               body,
            });
         }
         Some(item.offset)
      })
      .collect::<Vec<usize>>();

   match taken.len() {
      0 => Action::Pass,
      all if all == count => Action::Drop,
      _ => Action::Replace(without_frames(bytes, &taken)),
   }
}

/// Sends `method path` with Phoenix's session headers, any of them replaced
/// by `headers`, and waits for the answer. Runs on a worker thread.
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
   let (url, mut merged) = {
      let session = SESSION.lock().unwrap_or_else(PoisonError::into_inner);
      let session = session.as_ref().ok_or(DiscordError::NoSession)?;
      (format!("{}{path}", session.base), session.headers.clone())
   };
   merged.retain(|(name, _)| {
      !headers
         .iter()
         .any(|(own, _)| own.eq_ignore_ascii_case(name))
   });
   merged.extend(headers);

   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let pairs = merged
      .into_iter()
      .map(|(name, value)| Value::Arr(vec![Value::Str(name), Value::Str(value)]))
      .collect();
   let call = HostCall {
      module: NETWORKING,
      method: REQUEST,
      args: Value::Obj(vec![
         (ID, Value::I32(id)),
         (METHOD, Value::Str(method.to_owned())),
         (URL, Value::Str(url)),
         (HEADERS, Value::Arr(pairs)),
      ]),
      body,
   };

   let (sender, receiver) = mpsc::channel();
   PENDING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .insert(id, sender);
   let forget = || {
      PENDING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .remove(&id);
   };
   if !bridge::invoke(&call.encode()) {
      forget();
      return Err(DiscordError::NoBridge);
   }
   let Ok(reply) = receiver.recv_timeout(TIMEOUT) else {
      forget();
      return Err(DiscordError::Timeout {
         path: path.to_owned(),
      });
   };
   if reply.status == 0 {
      return Err(DiscordError::Network {
         path:    path.to_owned(),
         message: reply.error.unwrap_or_else(|| "no response".to_owned()),
      });
   }
   Ok(Response {
      status: reply.status,
      body:   reply.body,
   })
}
