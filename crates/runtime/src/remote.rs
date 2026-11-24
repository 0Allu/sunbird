//! Localhost-only HTTP debug shell on `127.0.0.1:4242` for ADB development
//! (`adb forward tcp:4242 tcp:4242`). Off by default; gated behind Debug →
//! Remote debug shell in Phoenix Unlocker settings.

use std::{
   io::{
      BufRead,
      BufReader,
      Read,
      Write,
   },
   net::{
      TcpListener,
      TcpStream,
   },
   sync::atomic::{
      AtomicBool,
      Ordering,
   },
   thread,
   time::Duration,
};
#[cfg(target_os = "android")]
use std::{
   collections::VecDeque,
   sync::{
      LazyLock,
      Mutex,
      PoisonError,
   },
};

#[cfg(target_os = "android")]
use phoenix_ui_mobile::{
   Op,
   Prop,
   inspect::WireJson,
};
use serde::Serialize;
#[cfg(target_os = "android")] use serde_json::value::RawValue;

use crate::PluginInfo;

pub const ADDR: &str = "127.0.0.1:4242";
const MAX_BODY: usize = 4 << 20;
#[cfg(target_os = "android")] const MAX_OPS_LOG: usize = 64;
#[cfg(target_os = "android")] const MAX_EVENTS_LOG: usize = 128;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "android")]
static OPS_LOG: LazyLock<Mutex<VecDeque<Box<RawValue>>>> =
   LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_OPS_LOG)));
#[cfg(target_os = "android")]
static EVENTS_LOG: LazyLock<Mutex<VecDeque<Box<RawValue>>>> =
   LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_EVENTS_LOG)));

/// A view or host event for the debug ring buffer. Never put HTTP headers or
/// credentials in one.
#[cfg(target_os = "android")]
#[derive(Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
pub enum Trace<'a> {
   HostCall {
      module: u16,
      method: u16,
   },
   HostEvent {
      event: u16,
   },
   View {
      node:    i32,
      event:   Prop,
      aid:     Option<&'a str>,
      payload: WireJson<'a>,
   },
}

/// Records a decoded Phoenix UI-op batch into the debug ring buffer when the
/// remote debug shell is enabled. Zero-cost atomic check when disabled.
#[cfg(target_os = "android")]
pub fn record_ops(ops: &[Op]) {
   #[derive(Serialize)]
   struct Batch<'a> {
      count: usize,
      ops:   &'a [Op],
   }

   if !enabled() || ops.is_empty() {
      return;
   }
   let batch = Batch {
      count: ops.len(),
      ops,
   };
   push(&OPS_LOG, MAX_OPS_LOG, &batch);
}

/// Records `trace` into the debug ring buffer when the remote debug shell is
/// enabled.
#[cfg(target_os = "android")]
pub fn record(trace: &Trace<'_>) {
   if enabled() {
      push(&EVENTS_LOG, MAX_EVENTS_LOG, trace);
   }
}

#[cfg(target_os = "android")]
fn push(log: &Mutex<VecDeque<Box<RawValue>>>, max: usize, entry: &impl Serialize) {
   let entry = match serde_json::value::to_raw_value(entry) {
      Ok(entry) => entry,
      Err(err) => {
         log::warn!("remote: a debug entry did not serialize: {err}");
         return;
      },
   };
   let mut log = log.lock().unwrap_or_else(PoisonError::into_inner);
   if log.len() == max {
      log.pop_front();
   }
   log.push_back(entry);
}

#[must_use]
pub fn enabled() -> bool {
   ENABLED.load(Ordering::Relaxed)
}

pub fn set_flag(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
}

pub fn set_enabled(on: bool) {
   ENABLED.store(on, Ordering::Relaxed);
   if on {
      ensure_started();
   }
}

pub fn ensure_started() {
   if !enabled() {
      return;
   }
   if RUNNING.swap(true, Ordering::AcqRel) {
      return;
   }
   thread::spawn(|| {
      let listener = match TcpListener::bind(ADDR) {
         Ok(listener) => listener,
         Err(err) => {
            log::error!("remote shell: bind {ADDR} failed: {err}");
            RUNNING.store(false, Ordering::Release);
            return;
         },
      };
      let _ = listener.set_nonblocking(true);
      log::info!("remote shell listening on http://{ADDR} (use `adb forward tcp:4242 tcp:4242`)");
      while enabled() {
         match listener.accept() {
            Ok((stream, _)) => {
               thread::spawn(move || handle(stream));
            },
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
               thread::sleep(Duration::from_millis(100));
            },
            Err(err) => {
               log::warn!("remote shell accept: {err}");
               thread::sleep(Duration::from_millis(250));
            },
         }
      }
      log::info!("remote shell stopped");
      RUNNING.store(false, Ordering::Release);
   });
}

fn handle(mut stream: TcpStream) {
   let _ = stream.set_nonblocking(false);
   let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
   let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

   let Ok((method, path, query, body)) = read_request(&stream) else {
      return;
   };
   let (status, content_type, response) = dispatch(&method, &path, &query, &body);
   let header = format!(
      "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: \
       close\r\n\r\n",
      response.len()
   );
   let _ = stream.write_all(header.as_bytes());
   let _ = stream.write_all(&response);
}

fn read_request(stream: &TcpStream) -> std::io::Result<(String, String, String, Vec<u8>)> {
   let mut reader = BufReader::new(stream);
   let mut request_line = String::new();
   if reader.read_line(&mut request_line)? == 0 {
      return Err(std::io::Error::new(
         std::io::ErrorKind::UnexpectedEof,
         "eof",
      ));
   }
   let mut parts = request_line.split_whitespace();
   let method = parts.next().unwrap_or_default().to_owned();
   let raw_path = parts.next().unwrap_or("/");
   let (path, query) = match raw_path.split_once('?') {
      Some((path, query)) => (path.to_owned(), query.to_owned()),
      None => (raw_path.to_owned(), String::new()),
   };

   let mut content_length: usize = 0;
   loop {
      let mut line = String::new();
      if reader.read_line(&mut line)? == 0 {
         break;
      }
      let trimmed = line.trim_end_matches(['\r', '\n']);
      if trimmed.is_empty() {
         break;
      }
      if let Some((name, value)) = trimmed.split_once(':')
         && name.trim().eq_ignore_ascii_case("content-length")
      {
         content_length = value.trim().parse::<usize>().unwrap_or(0).min(MAX_BODY);
      }
   }
   let mut body = vec![0u8; content_length];
   if content_length > 0 {
      reader.read_exact(&mut body)?;
   }
   Ok((method, path, query, body))
}

/// The percent-decoded value of `key`, as `encodeURIComponent` or a form wrote
/// it. A `%` not followed by two hex digits stays as it is.
#[cfg(target_os = "android")]
fn query_param(query: &str, key: &str) -> Option<String> {
   let (_, raw) = query
      .split('&')
      .filter_map(|pair| pair.split_once('='))
      .find(|(name, _)| *name == key)?;
   let bytes = raw.as_bytes();
   let mut out = Vec::with_capacity(bytes.len());
   let mut at = 0;
   while let Some(&byte) = bytes.get(at) {
      let escaped = bytes
         .get(at + 1..at + 3)
         .filter(|hex| hex.iter().all(u8::is_ascii_hexdigit))
         .and_then(|hex| u8::from_str_radix(str::from_utf8(hex).ok()?, 16).ok());
      match (byte, escaped) {
         (b'%', Some(decoded)) => {
            out.push(decoded);
            at += 3;
         },
         (b'+', _) => {
            out.push(b' ');
            at += 1;
         },
         _ => {
            out.push(byte);
            at += 1;
         },
      }
   }
   Some(String::from_utf8_lossy(&out).into_owned())
}

fn dispatch(
   method: &str,
   path: &str,
   #[cfg_attr(not(target_os = "android"), allow(unused_variables))] query: &str,
   body: &[u8],
) -> (&'static str, &'static str, Vec<u8>) {
   match (method, path) {
      ("GET", "/" | "/status") => {
         let plugins = crate::plugins();
         ok_json(&Status {
            version:      crate::update::VERSION,
            staff:        crate::staff_enabled(),
            remote_shell: enabled(),
            plugins:      plugins.iter().map(PluginJson::from).collect(),
         })
      },
      ("GET", "/plugins") => {
         let plugins = crate::plugins();
         ok_json(&plugins.iter().map(PluginJson::from).collect::<Vec<_>>())
      },
      ("GET", "/logs") => {
         let mut lines = crate::console::lines();
         lines.reverse();
         let text = lines
            .into_iter()
            .map(|line| format!("{} [{}] {}", line.time, line.level, line.text))
            .collect::<Vec<_>>()
            .join("\n");
         ("200 OK", "text/plain; charset=utf-8", text.into_bytes())
      },
      ("POST", "/eval") => {
         let Ok(code) = std::str::from_utf8(body) else {
            return err("400 Bad Request", "eval body must be UTF-8");
         };
         match crate::evaluate_capture(code) {
            Ok(result) => ("200 OK", "text/plain; charset=utf-8", result.into_bytes()),
            Err(message) => err("500 Internal Server Error", &message),
         }
      },
      #[cfg(target_os = "android")]
      ("GET", "/ops") => recent(&OPS_LOG, query, 20),
      #[cfg(target_os = "android")]
      ("GET", "/events") => recent(&EVENTS_LOG, query, 50),
      #[cfg(target_os = "android")]
      ("POST", "/clear") => {
         OPS_LOG
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
         EVENTS_LOG
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
         ok_json(&Done::Cleared)
      },
      #[cfg(target_os = "android")]
      ("GET", "/tree") => {
         let depth = query_param(query, "depth")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(6);
         tree(None, depth)
      },
      #[cfg(target_os = "android")]
      ("GET", _) if path.starts_with("/tree/") => {
         let Ok(id) = path["/tree/".len()..].parse::<i32>() else {
            return err("400 Bad Request", "invalid node id");
         };
         let depth = query_param(query, "depth")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(6);
         tree(Some(id), depth)
      },
      ("GET", "/update") => update_state(),
      ("POST", "/update/check") => {
         crate::update::check();
         ok_json(&Done::Started)
      },
      ("POST", "/update/apply") => {
         crate::update::apply();
         ok_json(&Done::Started)
      },
      #[cfg(target_os = "android")]
      ("POST", "/update/self") => {
         crate::update::self_update_from_site();
         ok_json(&Done::Started)
      },
      ("POST", "/debug/staff") => {
         let text = String::from_utf8_lossy(body);
         let on = matches!(text.trim(), "1" | "true" | "on");
         crate::set_staff_enabled(on);
         crate::refresh_settings();
         ok_json(&Done::Staff(crate::staff_enabled()))
      },
      _ if path.starts_with("/plugins/") => plugin_route(method, &path["/plugins/".len()..], body),
      _ => {
         err(
            "404 Not Found",
            "endpoints: GET /status, GET /plugins, GET|PUT|DELETE /plugins/<name>, POST \
             /plugins/<name>/{reload,enable,disable}, POST /eval, GET /logs, GET \
             /ops[?match=&limit=], GET /events[?match=&limit=], GET /tree[/<id>][?depth=], POST \
             /clear",
         )
      },
   }
}

#[derive(Serialize)]
struct Status<'a> {
   version:      &'static str,
   staff:        bool,
   remote_shell: bool,
   plugins:      Vec<PluginJson<'a>>,
}

/// A plugin and its state, for `/status` and `/plugins`.
#[derive(Serialize)]
struct PluginJson<'a> {
   name:        &'a str,
   enabled:     bool,
   installed:   bool,
   error:       Option<&'a str>,
   version:     Option<&'a str>,
   author:      Option<&'a str>,
   description: Option<&'a str>,
}

impl<'a> From<&'a PluginInfo> for PluginJson<'a> {
   fn from(plugin: &'a PluginInfo) -> Self {
      Self {
         name:        &plugin.name,
         enabled:     plugin.enabled,
         installed:   plugin.installed,
         error:       plugin.error.as_deref(),
         version:     plugin.header.version.as_deref(),
         author:      plugin.header.author.as_deref(),
         description: plugin.header.description.as_deref(),
      }
   }
}

/// What a mutating endpoint did.
#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum Done<'a> {
   #[cfg(target_os = "android")]
   Cleared,
   Started,
   Staff(bool),
   Installed(&'a str),
   Uninstalled(&'a str),
   Reloaded(&'a str),
   Enabled(&'a str),
   Disabled(&'a str),
}

/// The newest entries of `log`, filtered by the `match` and `limit` query
/// parameters.
#[cfg(target_os = "android")]
fn recent(
   log: &Mutex<VecDeque<Box<RawValue>>>,
   query: &str,
   default_limit: usize,
) -> (&'static str, &'static str, Vec<u8>) {
   let filter = query_param(query, "match");
   let limit = query_param(query, "limit")
      .and_then(|limit| limit.parse::<usize>().ok())
      .unwrap_or(default_limit);
   ok_json(
      &log
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .iter()
         .rev()
         .filter(|entry| {
            filter
               .as_ref()
               .is_none_or(|needle| entry.get().contains(needle.as_str()))
         })
         .take(limit)
         .collect::<Vec<_>>(),
   )
}

#[cfg(target_os = "android")]
fn tree(root: Option<i32>, depth: usize) -> (&'static str, &'static str, Vec<u8>) {
   crate::patches::with_tree(root, depth, |tree| {
      tree.map_or_else(
         || err("404 Not Found", "root node not seen yet"),
         |tree| ok_json(&tree),
      )
   })
}

fn update_state() -> (&'static str, &'static str, Vec<u8>) {
   #[derive(Serialize)]
   struct Found {
      loader:  Option<String>,
      plugins: Vec<String>,
   }

   #[derive(Serialize)]
   struct State {
      version: &'static str,
      busy:    bool,
      status:  Option<String>,
      found:   Option<Found>,
   }

   ok_json(&State {
      version: crate::update::VERSION,
      busy:    crate::update::busy(),
      status:  crate::update::status(),
      found:   crate::update::found().map(|found| {
         Found {
            loader:  found.loader,
            plugins: found.plugins,
         }
      }),
   })
}

fn plugin_route(method: &str, rest: &str, body: &[u8]) -> (&'static str, &'static str, Vec<u8>) {
   let (name, action) = rest
      .split_once('/')
      .map_or((rest, None), |(name, action)| (name, Some(action)));
   let valid = !name.is_empty()
      && name
         .bytes()
         .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
   if !valid {
      return err("400 Bad Request", "invalid plugin name");
   }
   match (method, action) {
      ("GET", None) => {
         crate::source(name).map_or_else(
            || err("404 Not Found", "plugin not found"),
            |source| {
               (
                  "200 OK",
                  "text/javascript; charset=utf-8",
                  source.into_bytes(),
               )
            },
         )
      },
      ("PUT" | "POST", None) => {
         let Ok(source) = String::from_utf8(body.to_vec()) else {
            return err("400 Bad Request", "plugin source must be UTF-8");
         };
         match crate::save_source(name, source) {
            Ok(()) => {
               crate::refresh_settings();
               ok_json(&Done::Installed(name))
            },
            Err(message) => err("500 Internal Server Error", &message.to_string()),
         }
      },
      ("DELETE", None) => {
         match crate::uninstall(name) {
            Ok(()) => {
               crate::refresh_settings();
               ok_json(&Done::Uninstalled(name))
            },
            Err(message) => err("500 Internal Server Error", &message.to_string()),
         }
      },
      ("POST", Some("reload")) => {
         crate::reload(name);
         crate::refresh_settings();
         ok_json(&Done::Reloaded(name))
      },
      ("POST", Some("enable")) => {
         crate::set_enabled(name, true);
         crate::refresh_settings();
         ok_json(&Done::Enabled(name))
      },
      ("POST", Some("disable")) => {
         crate::set_enabled(name, false);
         crate::refresh_settings();
         ok_json(&Done::Disabled(name))
      },
      _ => err("404 Not Found", "unknown plugin route"),
   }
}

fn ok_json(value: &impl Serialize) -> (&'static str, &'static str, Vec<u8>) {
   match serde_json::to_vec_pretty(value) {
      Ok(body) => ("200 OK", "application/json; charset=utf-8", body),
      Err(error) => err("500 Internal Server Error", &error.to_string()),
   }
}

fn err(status: &'static str, message: &str) -> (&'static str, &'static str, Vec<u8>) {
   (
      status,
      "text/plain; charset=utf-8",
      message.as_bytes().to_vec(),
   )
}
