//! phoenix on Phoenix's Android client: the UI-op bridge, the settings pages,
//! the UI patches, and JNI for HTTP, SQL and the installer.

#![cfg(target_os = "android")]

mod bridge;
mod debug;
mod discord;
mod fetch;
mod host;
mod install;
mod js;
mod jvm;
mod loader;
mod patches;
mod picker;
mod settings;
mod socket;
mod staff;
mod store;

pub(crate) use phoenix_runtime::highlight;

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
   remote,
   store::{
      Rows,
      StoreError,
   },
   update::Update,
};
use rquickjs::{
   Ctx,
   Object,
};

use crate::patches::banners::{
   self,
   BannerAction,
};

struct Android;

/// Starts phoenix inside Phoenix's Android client.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   phoenix_runtime::start(&Android, home, bundled)
}

/// Phoenix Unlock's green, for update banners.
const ACCENT: i64 = 0xFF23_A55A;

impl Platform for Android {
   fn send(&self, request: &Request) -> Result<Response, FetchError> {
      fetch::send(request)
   }

   fn query(&self, database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
      store::query(database, sql, args)
   }

   fn discord(
      &self,
      method: &str,
      path: &str,
      headers: Vec<(String, String)>,
      body: Vec<u8>,
   ) -> Result<Response, DiscordError> {
      discord::send(method, path, headers, body)
   }

   fn authorize(&self, request: &Authorization) -> Result<String, DiscordError> {
      discord::authorize(request)
   }

   fn init(&self, ctx: &Ctx<'_>) -> rquickjs::Result<()> {
      ctx.store_userdata(js::Mobile::default())
         .map_err(|err| rquickjs::Exception::throw_internal(ctx, &err.to_string()))?;
      Ok(())
   }

   fn api<'js>(
      &self,
      ctx: &Ctx<'js>,
      plugin: &str,
      api: &Object<'js>,
      messages: &Object<'js>,
   ) -> rquickjs::Result<()> {
      js::messages::patch_api(ctx, plugin, api, messages)?;
      api.set("bridge", js::bridge::bridge_object(ctx)?)
   }

   fn unload(&self, ctx: &Ctx<'_>, plugin: &str) {
      js::unload(ctx, plugin);
   }

   fn staff(&self, image: &'static Image<'static>) {
      staff::start(image);
   }

   fn gateway(&self, store: *mut c_void) {
      bridge::attach();
      // SAFETY: every registry handler takes the store state first, and this
      // runs inside one.
      unsafe { staff::apply(store) };
   }

   fn rewritten(&self, original: String, shown: String) {
      patches::remember_original(shown, original);
   }

   fn before_plugins(&self, home: &Path) {
      loader::adopt_staged(home, &home.join("plugins"));
   }

   fn started(&self, _first_run: bool, _plugins: &[String]) {
      if let Some(state) = phoenix_runtime::state() {
         loader::forget_update(&state.files_dir());
      }
      std::thread::spawn(|| {
         if let Err(err) = host::load() {
            log::error!("host names: {err}");
         }
      });
      settings::start();
      patches::start();
      bridge::install(Box::new(socket::Socket));
   }

   fn refresh(&self) {
      settings::refresh();
   }

   fn logged(&self, target: &str) {
      // A warning from refreshing the page would refresh it again, forever.
      if !target.starts_with(concat!(module_path!(), "::settings")) {
         settings::log_changed();
      }
   }

   fn toast(&self, message: &str) {
      let _ = picker::toast(message);
   }

   fn notify(&self, notice: Notice<'_>) {
      match notice {
         Notice::UpdateAvailable(update) => {
            let body = update.loader.as_ref().map_or_else(
               || {
                  format!(
                     "Updated plugins: {}, tap to update now",
                     update.plugins.join(", ")
                  )
               },
               |version| format!("Build {version}, tap to update in-app"),
            );
            banners::show_banner(
               "Phoenix Unlock update available".to_owned(),
               Some(body),
               Some("res://ic_mana_puzzle_piece_retry".to_owned()),
               Some(ACCENT),
               8000,
               BannerAction::ApplyUpdate,
            );
         },
         Notice::PluginsUpdated(names) => {
            banners::show_banner(
               "Plugins updated".to_owned(),
               Some(format!("Reloaded: {}", names.join(", "))),
               Some("res://ic_mana_checkmark_large".to_owned()),
               Some(ACCENT),
               4500,
               BannerAction::None,
            );
         },
      }
   }

   fn updates_loader(&self) -> bool {
      true
   }

   fn update_loader(&self, version: &str, update: &Update) {
      loader::install(version, update);
   }

   fn route(&self, request: &remote::Request) -> Option<remote::Reply> {
      debug::route(request)
   }

   fn routes(&self) -> &'static str {
      debug::ROUTES
   }
}
