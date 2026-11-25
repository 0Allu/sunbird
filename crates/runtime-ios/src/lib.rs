//! phoenix on iOS Phoenix, `PhoenixFatClient.framework` inside `Discord.app`.
//! Plugins get the gateway hooks, HTTP and the store. Phoenix's own session
//! and UI wait on the host-call bridge, which iOS does not reach yet.

#![cfg(target_os = "ios")]

mod bridge;
mod fetch;

use std::{
   ffi::c_void,
   path::{
      Path,
      PathBuf,
   },
};

use phoenix_hook::image::Image;
use phoenix_runtime::{
   Error,
   Notice,
   Platform,
   Plugin,
   Started,
   discord::{
      Authorization,
      DiscordError,
   },
   fetch::{
      FetchError,
      Request,
      Response,
   },
   store::{
      Rows,
      StoreError,
   },
};
use rquickjs::{
   Ctx,
   Object,
};

struct Ios;

/// Starts phoenix inside iOS Phoenix.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   phoenix_runtime::start(&Ios, home, bundled)
}

impl Platform for Ios {
   fn send(&self, request: &Request) -> Result<Response, FetchError> {
      fetch::send(request)
   }

   fn query(&self, database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
      phoenix_runtime::store::sqlite(database, sql, args)
   }

   fn discord(
      &self,
      _method: &str,
      _path: &str,
      _headers: Vec<(String, String)>,
      _body: Vec<u8>,
   ) -> Result<Response, DiscordError> {
      Err(DiscordError::Unsupported)
   }

   fn authorize(&self, _request: &Authorization) -> Result<String, DiscordError> {
      Err(DiscordError::Unsupported)
   }

   fn init(&self, _ctx: &Ctx<'_>) -> rquickjs::Result<()> {
      Ok(())
   }

   fn api<'js>(
      &self,
      _ctx: &Ctx<'js>,
      _plugin: &str,
      _api: &Object<'js>,
      _messages: &Object<'js>,
   ) -> rquickjs::Result<()> {
      Ok(())
   }

   fn unload(&self, _ctx: &Ctx<'_>, _plugin: &str) {}

   // The site's iOS patcher unlocks staff in the IPA's bytes.
   fn staff(&self, _image: &'static Image<'static>) {}

   fn gateway(&self, _store: *mut c_void) {
      bridge::tap();
   }

   fn started(&self, _first_run: bool, _plugins: &[String]) {
      if let Some(state) = phoenix_runtime::state() {
         bridge::locate_host_table(&state.image);
         bridge::arm(&state.image);
      }
   }

   fn refresh(&self) {}

   fn toast(&self, message: &str) {
      log::info!("toast: {message}");
   }

   fn notify(&self, notice: Notice<'_>) {
      match notice {
         Notice::UpdateAvailable(update) => {
            log::info!("plugin updates available: {}", update.plugins.join(", "));
         },
         Notice::PluginsUpdated(names) => log::info!("plugins updated: {}", names.join(", ")),
      }
   }
}
