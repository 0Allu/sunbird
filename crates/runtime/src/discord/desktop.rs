//! Desktop Discord HTTP transport.
//!
//! Desktop Phoenix drives Discord REST traffic natively through
//! `libdiscord/crates/http` (`reqwest` / `hyper`) rather than Android's JNI
//! `Networking` module. Hook points into `libdiscord/crates/http` plug in here.

use super::{
   Authorization,
   DiscordError,
};
use crate::fetch::Response;

pub fn send(
   _method: &str,
   path: &str,
   _headers: Vec<(String, String)>,
   _body: Vec<u8>,
) -> Result<Response, DiscordError> {
   if !path.starts_with('/') {
      return Err(DiscordError::Path {
         path: path.to_owned(),
      });
   }
   Err(DiscordError::NoSession)
}

pub fn authorize(_request: &Authorization) -> Result<String, DiscordError> {
   Err(DiscordError::NoBridge)
}
