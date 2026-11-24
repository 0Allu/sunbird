//! Desktop Discord HTTP transport and request/response interception.
//!
//! Desktop Phoenix drives Discord REST traffic natively through
//! `libdiscord/crates/http` rather than Android's JNI `Networking` module.
//! This module keeps the captured API base and session headers in memory only
//! (never logged or persisted), fulfills `phoenix.discord.*` requests via the
//! native platform HTTP stack, and runs matching `onRequest`, `onResponse`,
//! and `messages.onSend` hooks on intercepted API calls.

use std::{
   sync::{
      Mutex,
      PoisonError,
      atomic::Ordering,
      mpsc,
   },
   time::Duration,
};

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

#[derive(Clone)]
struct Session {
   base:    String,
   headers: Vec<(String, String)>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

/// Runs matching `phoenix.discord.onResponse` hooks on an incoming desktop
/// Discord API reply and returns the final body (or the original on timeout).
#[allow(dead_code)]
pub fn observe_response(method: &str, url: &str, status: i32, body: Vec<u8>) -> Vec<u8> {
   if RESPONSE_HOOK_COUNT.load(Ordering::Relaxed) == 0 {
      return body;
   }
   let Some(path) = api_path(url) else {
      return body;
   };
   let matched = matching_response_hooks(method, path);
   if matched.is_empty() {
      return body;
   }
   let fallback = body.clone();
   let (sender, receiver) = mpsc::channel::<Vec<u8>>();
   crate::intercept_response(
      matched,
      method.to_owned(),
      path.to_owned(),
      status,
      body,
      sender,
   );
   receiver.recv_timeout(Duration::from_secs(4)).unwrap_or_else(|_| {
      log::warn!("discord onResponse: {method} {path} timed out, delivering original");
      fallback
   })
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
}
