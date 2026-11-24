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
      SocketAddr,
      TcpListener,
      TcpStream,
   },
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
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
use serde::{
   Deserialize,
   Serialize,
};
#[cfg(target_os = "android")] use serde_json::value::RawValue;

use crate::{
   Chosen,
   Control,
   PluginInfo,
   Setting,
   update::{
      self,
      Catalog,
      Listed,
   },
};

#[cfg_attr(target_os = "android", allow(dead_code))]
pub const ADDR: &str = "127.0.0.1:4242";
const MAX_BODY: usize = 4 << 20;
#[cfg(target_os = "android")] const MAX_OPS_LOG: usize = 64;
#[cfg(target_os = "android")] const MAX_EVENTS_LOG: usize = 128;

static ENABLED: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);
static BOUND_ADDR: OnceLock<String> = OnceLock::new();
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

/// Returns the actual bound `host:port` of the localhost server, falling back
/// to [`ADDR`] if not started yet.
#[cfg_attr(target_os = "android", allow(dead_code))]
#[must_use]
pub fn bound_addr() -> &'static str {
   BOUND_ADDR.get().map_or(ADDR, String::as_str)
}

fn bind_localhost() -> std::io::Result<(TcpListener, SocketAddr)> {
   for port in 4242u16..=4252 {
      if let Ok(listener) = TcpListener::bind(("127.0.0.1", port))
         && let Ok(addr) = listener.local_addr()
      {
         return Ok((listener, addr));
      }
   }
   let listener = TcpListener::bind("127.0.0.1:0")?;
   let addr = listener.local_addr()?;
   Ok((listener, addr))
}

pub fn ensure_started() {
   if !enabled() {
      return;
   }
   if RUNNING.swap(true, Ordering::AcqRel) {
      return;
   }
   let (listener, addr) = match bind_localhost() {
      Ok(bound) => bound,
      Err(err) => {
         log::error!("remote shell: bind 127.0.0.1 failed: {err}");
         RUNNING.store(false, Ordering::Release);
         return;
      },
   };
   let addr_str = addr.to_string();
   let _ = BOUND_ADDR.set(addr_str.clone());
   let _ = listener.set_nonblocking(true);
   #[cfg(target_os = "android")]
   log::info!("remote shell listening on http://{addr_str} (use `adb forward tcp:4242 tcp:4242`)");
   #[cfg(not(target_os = "android"))]
   log::info!("Sunbird dashboard & API listening on http://{addr_str}/ui");

   thread::spawn(move || {
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

struct HttpRequest {
   method:         String,
   path:           String,
   query:          String,
   #[cfg_attr(target_os = "android", allow(dead_code))]
   accept:         String,
   host:           Option<String>,
   origin:         Option<String>,
   sec_fetch_site: Option<String>,
   body:           Vec<u8>,
}

fn is_loopback_host(host: &str) -> bool {
   let name = host.split(':').next().unwrap_or(host);
   name.eq_ignore_ascii_case("127.0.0.1") || name.eq_ignore_ascii_case("localhost")
}

fn is_allowed_origin(origin: &str) -> bool {
   origin
      .strip_prefix("http://")
      .is_some_and(is_loopback_host)
}

fn is_allowed_fetch_site(site: &str) -> bool {
   site.eq_ignore_ascii_case("same-origin")
      || site.eq_ignore_ascii_case("same-site")
      || site.eq_ignore_ascii_case("none")
}

fn handle(mut stream: TcpStream) {
   let _ = stream.set_nonblocking(false);
   let _ = stream.set_read_timeout(Some(Duration::from_secs(15)));
   let _ = stream.set_write_timeout(Some(Duration::from_secs(15)));

   let Ok(req) = read_request(&stream) else {
      return;
   };
   if req
      .host
      .as_deref()
      .is_some_and(|host| !is_loopback_host(host))
      || req
         .origin
         .as_deref()
         .is_some_and(|origin| !is_allowed_origin(origin))
      || req
         .sec_fetch_site
         .as_deref()
         .is_some_and(|site| !is_allowed_fetch_site(site))
   {
      let (status, content_type, response) = err("403 Forbidden", "loopback origin only");
      write_response(&mut stream, status, content_type, &response);
      return;
   }
   let (status, content_type, response) = dispatch(&req);
   write_response(&mut stream, status, content_type, &response);
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, response: &[u8]) {
   let header = format!(
      "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\nContent-Length: \
       {}\r\nConnection: close\r\n\r\n",
      response.len()
   );
   let _ = stream.write_all(header.as_bytes());
   let _ = stream.write_all(response);
}

fn read_request(stream: &TcpStream) -> std::io::Result<HttpRequest> {
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
   let mut accept = String::new();
   let mut host = None;
   let mut origin = None;
   let mut sec_fetch_site = None;
   loop {
      let mut line = String::new();
      if reader.read_line(&mut line)? == 0 {
         break;
      }
      let trimmed = line.trim_end_matches(['\r', '\n']);
      if trimmed.is_empty() {
         break;
      }
      if let Some((name, value)) = trimmed.split_once(':') {
         let name = name.trim();
         let value = value.trim();
         if name.eq_ignore_ascii_case("content-length") {
            content_length = value.parse::<usize>().unwrap_or(0).min(MAX_BODY);
         } else if name.eq_ignore_ascii_case("accept") {
            value.clone_into(&mut accept);
         } else if name.eq_ignore_ascii_case("host") {
            host = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value.to_owned());
         } else if name.eq_ignore_ascii_case("sec-fetch-site") {
            sec_fetch_site = Some(value.to_owned());
         }
      }
   }
   let mut body = vec![0u8; content_length];
   if content_length > 0 {
      reader.read_exact(&mut body)?;
   }
   Ok(HttpRequest {
      method,
      path,
      query,
      accept,
      host,
      origin,
      sec_fetch_site,
      body,
   })
}

/// Percent-decodes a URI component. A `%` not followed by two hex digits stays
/// as it is.
fn percent_decode(raw: &str) -> String {
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
   String::from_utf8_lossy(&out).into_owned()
}

/// The percent-decoded value of `key`, as `encodeURIComponent` or a form wrote
/// it.
#[cfg(target_os = "android")]
fn query_param(query: &str, key: &str) -> Option<String> {
   let (_, raw) = query
      .split('&')
      .filter_map(|pair| pair.split_once('='))
      .find(|(name, _)| *name == key)?;
   Some(percent_decode(raw))
}

fn dispatch(req: &HttpRequest) -> (&'static str, &'static str, Vec<u8>) {
   let method = req.method.as_str();
   let path = req.path.as_str();
   #[cfg(target_os = "android")]
   let query = req.query.as_str();
   let body = req.body.as_slice();
   let _ = &req.query;

   match (method, path) {
      #[cfg(not(target_os = "android"))]
      ("GET", "/ui") => (
         "200 OK",
         "text/html; charset=utf-8",
         phoenix_ui_desktop::DASHBOARD_HTML.as_bytes().to_vec(),
      ),
      #[cfg(not(target_os = "android"))]
      ("GET", "/") if req.accept.contains("text/html") => (
         "200 OK",
         "text/html; charset=utf-8",
         phoenix_ui_desktop::DASHBOARD_HTML.as_bytes().to_vec(),
      ),
      #[cfg(not(target_os = "android"))]
      ("POST", "/ui/devtools") => {
         phoenix_ui_desktop::open_devtools();
         ok_json(&Done::Started)
      },
      ("GET", "/favicon.ico") => ("204 No Content", "image/x-icon", Vec::new()),
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
      ("POST", "/plugins/reload-all") => {
         crate::reload_all();
         crate::refresh_settings();
         ok_json(&Done::Started)
      },
      ("GET", "/catalog") => catalog_state(),
      ("POST", "/catalog/refresh") => {
         update::fetch_catalog();
         ok_json(&Done::Started)
      },
      ("POST", "/catalog/install") => catalog_install(body),
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
         #[cfg(target_os = "android")]
         let help = "endpoints: GET /status, GET /plugins, GET|PUT|DELETE /plugins/<name>, POST \
                     /plugins/<name>/{reload,enable,disable,setting,press}, GET /catalog, POST \
                     /catalog/{refresh,install}, POST /eval, GET /logs, GET \
                     /ops[?match=&limit=], GET /events[?match=&limit=], GET /tree[/<id>][?depth=], \
                     POST /clear";
         #[cfg(not(target_os = "android"))]
         let help = "endpoints: GET /ui, GET /status, GET /plugins, POST /plugins/reload-all, \
                     GET|PUT|DELETE /plugins/<name>, POST \
                     /plugins/<name>/{reload,enable,disable,setting,press}, GET /catalog, POST \
                     /catalog/{refresh,install}, GET /update, POST /update/{check,apply}, POST \
                     /eval, GET /logs";
         err("404 Not Found", help)
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
   settings:    Vec<SettingJson<'a>>,
}

#[derive(Serialize)]
struct ChoiceJson<'a> {
   value: &'a str,
   label: &'a str,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ControlJson<'a> {
   Switch {
      value: bool,
   },
   Text {
      value: &'a str,
   },
   Secret {
      has_value: bool,
   },
   Select {
      value:   &'a str,
      options: Vec<ChoiceJson<'a>>,
   },
   Number {
      value: f64,
   },
   Button,
}

#[derive(Serialize)]
struct SettingJson<'a> {
   key:         &'a str,
   title:       &'a str,
   description: Option<&'a str>,
   #[serde(flatten)]
   control:     ControlJson<'a>,
}

impl<'a> From<&'a Setting> for SettingJson<'a> {
   fn from(setting: &'a Setting) -> Self {
      let control = match &setting.control {
         Control::Switch(value) => ControlJson::Switch { value: *value },
         Control::Text(value) => ControlJson::Text { value },
         Control::Secret(value) => ControlJson::Secret {
            has_value: !value.is_empty(),
         },
         Control::Select { options, chosen } => ControlJson::Select {
            value:   chosen,
            options: options
               .iter()
               .map(|choice| ChoiceJson {
                  value: &choice.value,
                  label: &choice.label,
               })
               .collect(),
         },
         Control::Number(value) => ControlJson::Number { value: *value },
         Control::Button => ControlJson::Button,
      };
      Self {
         key: &setting.key,
         title: &setting.title,
         description: setting.description.as_deref(),
         control,
      }
   }
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
         settings:    plugin.settings.iter().map(SettingJson::from).collect(),
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
   Saved(&'a str),
   Pressed(&'a str),
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
   let (raw_name, action) = rest
      .split_once('/')
      .map_or((rest, None), |(name, action)| (name, Some(action)));
   let name = percent_decode(raw_name);
   let name = name.as_str();
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
      ("POST", Some("setting")) => {
         #[derive(Deserialize)]
         struct SetRequest {
            key:   String,
            value: Chosen,
         }
         let Ok(req) = serde_json::from_slice::<SetRequest>(body) else {
            return err("400 Bad Request", "expected JSON {key, value}");
         };
         crate::set_setting(name, &req.key, req.value);
         crate::refresh_settings();
         ok_json(&Done::Saved(&req.key))
      },
      ("POST", Some("press")) => {
         #[derive(Deserialize)]
         struct PressRequest {
            key: String,
         }
         let Ok(req) = serde_json::from_slice::<PressRequest>(body) else {
            return err("400 Bad Request", "expected JSON {key}");
         };
         crate::press(name, &req.key);
         ok_json(&Done::Pressed(&req.key))
      },
      _ => err("404 Not Found", "unknown plugin route"),
   }
}

fn catalog_state() -> (&'static str, &'static str, Vec<u8>) {
   #[derive(Serialize)]
   #[serde(tag = "state", rename_all = "lowercase")]
   enum CatalogJson {
      Unfetched,
      Loading,
      Loaded { plugins: Vec<Listed> },
      Failed { error: String },
   }

   let current = update::catalog();
   if matches!(current, Catalog::Unfetched) {
      update::fetch_catalog();
   }
   match current {
      Catalog::Unfetched => ok_json(&CatalogJson::Unfetched),
      Catalog::Loading => ok_json(&CatalogJson::Loading),
      Catalog::Loaded(plugins) => ok_json(&CatalogJson::Loaded { plugins }),
      Catalog::Failed(error) => ok_json(&CatalogJson::Failed { error }),
   }
}

fn catalog_install(body: &[u8]) -> (&'static str, &'static str, Vec<u8>) {
   #[derive(Deserialize)]
   struct InstallRequest {
      name: Option<String>,
      url:  Option<String>,
   }

   let Ok(req) = serde_json::from_slice::<InstallRequest>(body) else {
      return err("400 Bad Request", "expected JSON {name} or {url}");
   };
   let outcome = match (req.name, req.url) {
      (Some(name), _) => update::install_listed(&name),
      (None, Some(url)) => crate::install(&url),
      (None, None) => return err("400 Bad Request", "provide either name or url"),
   };
   match outcome {
      Ok(installed) => {
         crate::refresh_settings();
         ok_json(&Done::Installed(&installed))
      },
      Err(message) => err("400 Bad Request", &message.to_string()),
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

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn validates_loopback_hosts_and_origins() {
      assert!(is_loopback_host("127.0.0.1:4242"));
      assert!(is_loopback_host("localhost:4242"));
      assert!(!is_loopback_host("evil.example:4242"));
      assert!(is_allowed_origin("http://127.0.0.1:4242"));
      assert!(is_allowed_origin("http://localhost:4242"));
      assert!(!is_allowed_origin("https://evil.example"));
      assert!(!is_allowed_origin("http://127.0.0.1.evil.example"));
      assert!(is_allowed_fetch_site("same-origin"));
      assert!(is_allowed_fetch_site("none"));
      assert!(!is_allowed_fetch_site("cross-site"));
   }

   #[test]
   fn decodes_percent_encoded_names() {
      assert_eq!(percent_decode("silent-typing"), "silent-typing");
      assert_eq!(percent_decode("my%20plugin"), "my plugin");
   }
}
