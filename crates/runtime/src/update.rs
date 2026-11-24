//! Tells the user when the site ships a newer Phoenix Unlock: a newer loader,
//! or plugin bundles that differ from the installed ones. The site publishes
//! both in `version.json` (`web/tools/bundle-plugins.ts`).
//!
//! Applying an update works in-app:
//! - Plugin-only updates download each changed `plugins/<name>.js`, verify its
//!   CRC-32 against `version.json`, write it to `files/phoenix/plugins/`, and
//!   hot-reload it live with zero APK reinstall.
//! - Loader updates download `plugins/libphoenix-<abi>.so`, swap it into the
//!   installed APK with [`patcher::swap_loader`], which signs it again, and
//!   open Android's installer on it through Discord's own `FileProvider`. An
//!   install from separate splits cannot update from one APK, so it opens the
//!   site instead.

use std::{
   collections::BTreeMap,
   sync::{
      Mutex,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
};
#[cfg(target_os = "android")]
use std::{
   fs::{
      self,
      File,
   },
   io::{
      BufWriter,
      Write,
   },
   path::Path,
};

#[cfg(target_os = "android")] use memchr::memmem;

/// This build's version, `<UTC commit date>-<short hash>` (`build.rs`).
pub const VERSION: &str = env!("PHOENIX_VERSION");
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub const SITE: &str = "https://sunbird.tools/";
pub const ASSETS: &str = "https://assets.sunbird.tools/";

/// Where `bundle-plugins.ts` reads the version from the loader the site ships.
#[used]
static MARKER: &str = concat!("phoenix-version:", env!("PHOENIX_VERSION"), ";");

/// Where the signed update waits for Android's installer, under the app's
/// `files` directory, which Discord's `FileProvider` shares.
#[cfg(target_os = "android")] const UPDATE_APK: &str = "phoenix-update.apk";

#[cfg(target_os = "android")]
const LOADER_ASSET: &str = if cfg!(target_arch = "x86_64") {
   "plugins/libphoenix-x86_64.so"
} else if cfg!(target_arch = "arm") {
   "plugins/libphoenix-armv7.so"
} else {
   "plugins/libphoenix-arm64.so"
};

#[derive(serde::Deserialize)]
struct Site {
   loader:  String,
   plugins: BTreeMap<String, u32>,
}

/// What the site has that this install lacks.
#[derive(Clone)]
pub struct Update {
   /// The site's version, when it is newer than [`VERSION`].
   pub loader:      Option<String>,
   /// Installed plugins whose code differs from the site's.
   pub plugins:     Vec<String>,
   /// Expected CRC-32 for each plugin in [`Self::plugins`].
   pub plugin_crcs: BTreeMap<String, u32>,
}

/// A plugin the site offers, as `plugins/index.json` lists it.
#[derive(Clone, serde::Deserialize, serde::Serialize)]
pub struct Listed {
   pub name:        String,
   pub description: String,
   #[serde(default)]
   pub warning:     Option<String>,
}

/// The site's plugin list, fetched when the install page first opens.
#[derive(Clone)]
pub enum Catalog {
   Unfetched,
   Loading,
   Loaded(Vec<Listed>),
   Failed(String),
}

static CATALOG: Mutex<Catalog> = Mutex::new(Catalog::Unfetched);
static FOUND: Mutex<Option<Update>> = Mutex::new(None);
static STATUS: Mutex<Option<String>> = Mutex::new(None);
static BUSY: AtomicBool = AtomicBool::new(false);

/// The update the last check found, if any.
pub fn found() -> Option<Update> {
   FOUND.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Human-readable status of the current or last update attempt, if any.
pub fn status() -> Option<String> {
   STATUS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Whether an update download/install is currently running.
pub fn busy() -> bool {
   BUSY.load(Ordering::Relaxed)
}

fn set_status(message: Option<String>) {
   *STATUS.lock().unwrap_or_else(PoisonError::into_inner) = message;
   crate::refresh_settings();
}

pub fn catalog() -> Catalog {
   CATALOG
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
}

/// Fetches the site's plugin list in the background, unless it is loaded or
/// on its way. A failed fetch is retried on the next call.
pub fn fetch_catalog() {
   {
      let mut catalog = CATALOG.lock().unwrap_or_else(PoisonError::into_inner);
      if matches!(*catalog, Catalog::Loading | Catalog::Loaded(_)) {
         return;
      }
      *catalog = Catalog::Loading;
   }
   thread::spawn(|| {
      let fetched = crate::fetch::get(&format!("{ASSETS}plugins/index.json"))
         .map_err(|err| err.to_string())
         .and_then(|body| {
            serde_json::from_slice::<Vec<Listed>>(&body).map_err(|err| err.to_string())
         });
      *CATALOG.lock().unwrap_or_else(PoisonError::into_inner) = match fetched {
         Ok(listed) => Catalog::Loaded(listed),
         Err(err) => {
            log::info!("plugin list: {err}");
            Catalog::Failed(err)
         },
      };
      crate::refresh_settings();
   });
}

/// Installs a plugin the site lists. Site plugins can call API an older
/// loader lacks, so they wait until the loader is current, like updates do.
pub fn install_listed(name: &str) -> Result<String, crate::Error> {
   if let Some(version) = found().and_then(|update| update.loader) {
      return Err(crate::Error::LoaderOutdated { version });
   }
   crate::install(&format!("{ASSETS}plugins/{name}.js"))
}

/// Asks the site in the background. Failures (offline) are only logged.
pub fn check() {
   thread::spawn(|| {
      let site = match crate::fetch::get(&format!("{ASSETS}version.json")) {
         Ok(body) => serde_json::from_slice::<Site>(&body),
         Err(err) => {
            log::info!("update check: {err}");
            return;
         },
      };
      let site = match site {
         Ok(site) => site,
         Err(err) => {
            log::info!("update check: version.json: {err}");
            return;
         },
      };
      let loader = (site.loader.as_str() > VERSION).then_some(site.loader);
      let mut plugins = Vec::new();
      let mut plugin_crcs = BTreeMap::new();
      // The site's plugins are built against its loader, so they can call
      // API this one lacks. They wait until the loader is current.
      if loader.is_none() {
         for plugin in crate::plugins() {
            if let Some(&crc) = site.plugins.get(&plugin.name)
               && crate::source(&plugin.name)
                  .is_some_and(|source| crc32fast::hash(source.as_bytes()) != crc)
            {
               plugin_crcs.insert(plugin.name.clone(), crc);
               plugins.push(plugin.name);
            }
         }
      }
      let update = Update {
         loader,
         plugins,
         plugin_crcs,
      };
      if update.loader.is_none() && update.plugins.is_empty() {
         log::info!("up to date ({VERSION})");
         *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = None;
         crate::refresh_settings();
         return;
      }
      log::info!(
         "update available: loader {:?}, plugins {:?}",
         update.loader,
         update.plugins
      );
      let body = match &update.loader {
         Some(version) => format!("Build {version} — tap to update in-app"),
         None => {
            format!(
               "Updated plugins: {} — tap to update now",
               update.plugins.join(", ")
            )
         },
      };
      *FOUND.lock().unwrap_or_else(PoisonError::into_inner) = Some(update);
      crate::refresh_settings();
      #[cfg(target_os = "android")]
      crate::patches::banners::show_banner(
         "Phoenix Unlock update available".to_owned(),
         Some(body),
         Some("res://ic_mana_puzzle_piece_retry".to_owned()),
         Some(0xFF23_A55A),
         8000,
         crate::patches::banners::BannerAction::ApplyUpdate,
      );
      #[cfg(not(target_os = "android"))]
      phoenix_ui_desktop::notify("Sunbird update available", Some(&body));
   });
}

/// Applies the pending update found by [`check`].
///
/// Updates changed plugins over-the-air first (hot-reloading them live), then
/// runs the in-app APK self-updater if a newer loader is available.
pub fn apply() {
   let Some(update) = found() else {
      log::info!("update apply: no pending update");
      return;
   };
   if BUSY.swap(true, Ordering::AcqRel) {
      log::info!("update apply: already running");
      crate::toast_user("Update already in progress...");
      return;
   }
   thread::spawn(move || {
      let mut updated_plugins = Vec::new();
      if !update.plugins.is_empty() {
         set_status(Some(format!(
            "Updating {} plugin{}...",
            update.plugins.len(),
            if update.plugins.len() == 1 { "" } else { "s" }
         )));
         let mut failed = Vec::new();
         for name in &update.plugins {
            let expected_crc = update.plugin_crcs.get(name).copied();
            match update_plugin(name, expected_crc) {
               Ok(()) => {
                  log::info!("ota updated plugin {name}");
                  updated_plugins.push(name.clone());
               },
               Err(err) => {
                  log::error!("ota update {name} failed: {err}");
                  failed.push(format!("{name}: {err}"));
               },
            }
         }
         // Drop successfully updated plugins from FOUND.
         {
            let mut guard = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(pending) = guard.as_mut() {
               pending
                  .plugins
                  .retain(|name| !updated_plugins.contains(name));
               pending
                  .plugin_crcs
                  .retain(|name, _| !updated_plugins.contains(name));
               if pending.loader.is_none() && pending.plugins.is_empty() {
                  *guard = None;
               }
            }
         }
         if !failed.is_empty() && update.loader.is_none() {
            let msg = format!("Plugin update failed: {}", failed.join("; "));
            set_status(Some(msg.clone()));
            BUSY.store(false, Ordering::Release);
            crate::toast_user(&msg);
            return;
         }
      }

      #[cfg(target_os = "android")]
      if update.loader.is_some() {
         run_loader_self_update();
         return;
      }

      // Plugin-only OTA update completed cleanly.
      set_status(None);
      BUSY.store(false, Ordering::Release);
      crate::refresh_settings();
      #[cfg(target_os = "android")]
      crate::patches::banners::show_banner(
         "Plugins updated".to_owned(),
         Some(format!("Reloaded: {}", updated_plugins.join(", "))),
         Some("res://ic_mana_checkmark_large".to_owned()),
         Some(0xFF23_A55A),
         4500,
         crate::patches::banners::BannerAction::None,
      );
      #[cfg(not(target_os = "android"))]
      phoenix_ui_desktop::notify(
         "Plugins updated",
         Some(&format!("Reloaded: {}", updated_plugins.join(", "))),
      );
   });
}

/// Runs the in-app loader self-update directly against the site's current
/// `libphoenix-*.so` (also exposed to the gated localhost debug shell).
#[cfg(target_os = "android")]
pub fn self_update_from_site() {
   if BUSY.swap(true, Ordering::AcqRel) {
      log::info!("self-update: already running");
      return;
   }
   thread::spawn(run_loader_self_update);
}

fn update_plugin(name: &str, expected_crc: Option<u32>) -> Result<(), String> {
   let url = format!("{ASSETS}plugins/{name}.js");
   let bytes = crate::fetch::get(&url).map_err(|err| format!("fetch: {err}"))?;
   if let Some(expected) = expected_crc {
      let actual = crc32fast::hash(&bytes);
      if actual != expected {
         return Err(format!(
            "CRC mismatch (got {actual:#x}, expected {expected:#x})"
         ));
      }
   }
   let source = String::from_utf8(bytes).map_err(|err| format!("invalid UTF-8: {err}"))?;
   crate::save_source(name, source).map_err(|err| err.to_string())
}

#[cfg(target_os = "android")]
fn run_loader_self_update() {
   set_status(Some("Downloading loader...".to_owned()));
   let _ = crate::picker::toast("Downloading Phoenix Unlock update...");

   let url = format!("{ASSETS}{LOADER_ASSET}");
   let bytes = match crate::fetch::get(&url) {
      Ok(bytes) => bytes,
      Err(err) => {
         fail_self_update(&format!("Download failed: {err}"));
         return;
      },
   };
   if !bytes.starts_with(b"\x7fELF") || memmem::find(&bytes, b"phoenix-version:").is_none() {
      fail_self_update("Downloaded loader is invalid");
      return;
   }

   let loader = match patcher::Library::new(patcher::LOADER, bytes) {
      Ok(loader) => loader,
      Err(err) => {
         fail_self_update(&format!("Downloaded loader is invalid: {err}"));
         return;
      },
   };
   let base = match crate::host::base_apk() {
      Ok(base) => base,
      Err(err) => {
         fail_self_update(&format!("Self-update failed: {err}"));
         return;
      },
   };
   if installed_from_splits(&base) {
      fail_self_update("Split APK install, update from the site");
      let _ = crate::picker::open(SITE);
      return;
   }

   set_status(Some("Signing the update...".to_owned()));
   let Some(state) = crate::STATE.get() else {
      fail_self_update("Self-update failed: phoenix has not started");
      return;
   };
   let path = state.files_dir().join(UPDATE_APK);
   if let Err(err) = write_update(&base, &loader, &path) {
      fail_self_update(&format!("Self-update failed: {err}"));
      return;
   }
   set_status(Some("Opening Android's installer...".to_owned()));
   match crate::install::open(&path) {
      Ok(()) => {
         set_status(None);
         BUSY.store(false, Ordering::Release);
      },
      Err(err) => fail_self_update(&format!("Self-update failed: {err}")),
   }
}

/// Whether the app runs from split APKs, which sit next to `base`.
#[cfg(target_os = "android")]
fn installed_from_splits(base: &str) -> bool {
   Path::new(base)
      .parent()
      .and_then(|dir| fs::read_dir(dir).ok())
      .is_some_and(|entries| {
         entries.flatten().any(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
               name.starts_with("split_")
                  && Path::new(name)
                     .extension()
                     .is_some_and(|extension| extension.eq_ignore_ascii_case("apk"))
            })
         })
      })
}

/// Writes `base` with `loader` swapped in and signed to `path`.
#[cfg(target_os = "android")]
fn write_update(base: &str, loader: &patcher::Library, path: &Path) -> Result<(), String> {
   let map = crate::host::Map::open(base).map_err(|err| format!("{base}: {err}"))?;
   let apk = patcher::swap_loader(map.bytes(), loader).map_err(|err| err.to_string())?;
   let mut file =
      BufWriter::new(File::create(path).map_err(|err| format!("{}: {err}", path.display()))?);
   for part in apk.parts() {
      file.write_all(part).map_err(|err| err.to_string())?;
   }
   file.flush().map_err(|err| err.to_string())
}

/// Deletes an update Android's installer has had its chance at, since it is as
/// big as the app.
#[cfg(target_os = "android")]
pub fn forget_update(files: &Path) {
   let _ = fs::remove_file(files.join(UPDATE_APK));
}

#[cfg(target_os = "android")]
fn fail_self_update(message: &str) {
   log::error!("self-update: {message}");
   set_status(Some(message.to_owned()));
   BUSY.store(false, Ordering::Release);
   let _ = crate::picker::toast(message);
}
