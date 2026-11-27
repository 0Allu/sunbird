//! Updates Discord itself to a build CI checked against the release the site
//! serves (`.github/workflows/discord.yml`), which lists it in
//! `web/discord.json` with each split's SHA-256.
//!
//! The splits this phone needs come from the tracker the file names, each
//! checked against its hash, and merge with the site's loader and the
//! installed APK's plugins into one APK, signed like the site's, under the
//! installed package and label, so it installs over this one and keeps the
//! login.

use std::{
   collections::BTreeMap,
   ffi::CStr,
   fmt::Write as _,
   sync::{
      Mutex,
      MutexGuard,
      PoisonError,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
   thread,
};

use patcher::{
   PackageName,
   Payload,
   Runtime,
   manifest::Info,
};
use phoenix_runtime::fetch;
use sha2::{
   Digest,
   Sha256,
};

/// Where CI publishes the builds that passed, read from main so a ready
/// build needs no site release.
const BUILDS: &str =
   "https://raw.githubusercontent.com/sunbird-tools/sunbird/main/web/discord.json";
/// Under [`fetch::LIMIT`], which caps one response.
const CHUNK: usize = 4 << 20;
const ABI: &str = if cfg!(target_arch = "x86_64") {
   "config.x86_64"
} else if cfg!(target_arch = "arm") {
   "config.armeabi_v7a"
} else {
   "config.arm64_v8a"
};
/// Android's density buckets, by split name.
const DENSITIES: [(&str, u32); 7] = [
   ("ldpi", 120),
   ("mdpi", 160),
   ("tvdpi", 213),
   ("hdpi", 240),
   ("xhdpi", 320),
   ("xxhdpi", 480),
   ("xxxhdpi", 640),
];

/// A build `web/discord.json` lists.
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Build {
   pub version_code: u32,
   pub version_name: String,
   url:              String,
   splits:           BTreeMap<String, Split>,
}

#[derive(Clone, serde::Deserialize)]
struct Split {
   sha256: String,
   size:   usize,
}

#[derive(Clone)]
pub enum State {
   /// Unchecked, or no newer build.
   Current,
   Available(Build),
   Running {
      build: Build,
      step:  String,
      done:  usize,
      total: usize,
   },
   Failed {
      build: Build,
      error: String,
   },
   /// Android's installer has it.
   Handed(Build),
}

static STATE: Mutex<State> = Mutex::new(State::Current);
/// Set by the banner, so the update page opens once Settings shows.
static OPEN_PAGE: AtomicBool = AtomicBool::new(false);

fn state_mut() -> MutexGuard<'static, State> {
   STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set(state: State) {
   *state_mut() = state;
   phoenix_runtime::refresh_settings();
}

#[must_use]
pub fn state() -> State {
   state_mut().clone()
}

/// Whether Settings should open the update page, once.
pub fn take_open_page() -> bool {
   OPEN_PAGE.swap(false, Ordering::AcqRel)
}

/// Opens Settings, which then shows the update page.
pub fn open_page() {
   OPEN_PAGE.store(true, Ordering::Release);
   if let Err(err) = crate::picker::open("discord://app/settings") {
      OPEN_PAGE.store(false, Ordering::Release);
      log::warn!("discord update: opening Settings: {err}");
   }
}

/// Looks for a newer checked build in the background, and offers it.
pub fn check() {
   thread::spawn(|| {
      match newer() {
         Ok(Some(build)) => {
            log::info!("discord update: {} is ready", build.version_code);
            let name = build.version_name.clone();
            set(State::Available(build));
            crate::patches::banners::show_banner(
               "Discord update available".to_owned(),
               Some(format!("{name}, checked for Sunbird. Tap to update")),
               Some("res://ic_mana_puzzle_piece_retry".to_owned()),
               Some(crate::ACCENT),
               8000,
               crate::patches::banners::BannerAction::OpenDiscordUpdate,
            );
         },
         Ok(None) => {},
         Err(err) => log::info!("discord update check: {err}"),
      }
   });
}

fn newer() -> Result<Option<Build>, String> {
   let body = fetch::get(BUILDS).map_err(|err| err.to_string())?;
   let build =
      serde_json::from_slice::<Build>(&body).map_err(|err| format!("discord.json: {err}"))?;
   if !build.url.starts_with("https://") {
      return Err(format!("discord.json: {} is not https", build.url));
   }
   Ok((build.version_code > installed()?.version_code).then_some(build))
}

/// What the running APK's manifest says.
fn installed() -> Result<Info, String> {
   let base = crate::host::base_apk().map_err(|err| err.to_string())?;
   let map = crate::host::Map::open(&base).map_err(|err| format!("{base}: {err}"))?;
   patcher::manifest_info(map.bytes()).map_err(|err| err.to_string())
}

/// Starts the update the page offers, unless one runs.
pub fn start() {
   let build = {
      let mut state = state_mut();
      let build = match &*state {
         State::Available(build) | State::Failed { build, .. } | State::Handed(build) => {
            build.clone()
         },
         State::Current | State::Running { .. } => return,
      };
      *state = running(&build, "Starting...", 0, 0);
      build
   };
   phoenix_runtime::refresh_settings();
   thread::spawn(move || {
      match update(&build) {
         Ok(()) => set(State::Handed(build)),
         Err(error) => {
            log::error!("discord update: {error}");
            set(State::Failed { build, error });
         },
      }
   });
}

fn running(build: &Build, step: &str, done: usize, total: usize) -> State {
   State::Running {
      build: build.clone(),
      step: step.to_owned(),
      done,
      total,
   }
}

fn update(build: &Build) -> Result<(), String> {
   let installed = installed()?;
   let names = wanted(build)?;
   let total = names.iter().map(|name| build.splits[name].size).sum();
   let mut done = 0;
   let mut apks = Vec::with_capacity(names.len());
   for name in &names {
      apks.push(download(build, name, &mut done, total)?);
   }

   set(running(build, "Downloading the loader...", done, total));
   let loader = crate::loader::download()?;
   // Installed plugins live in the app's files and outlast the update, so only
   // the bundled ones ride along.
   let plugins = phoenix_runtime::plugins()
      .into_iter()
      .filter_map(|plugin| {
         phoenix_runtime::bundled_source(&plugin.name).map(|source| (plugin.name, source))
      })
      .collect();
   let prepared = Payload {
      runtime: Some(Runtime {
         libraries: vec![loader],
         plugins,
      }),
      frida:   None,
   }
   .prepare()
   .map_err(|err| err.to_string())?;
   let package = installed
      .package
      .parse::<PackageName>()
      .map_err(|err| err.to_string())?;

   set(running(build, "Building the update...", done, total));
   let apk = patcher::merge(
      &mut apks,
      &prepared,
      &package,
      installed.label.as_deref(),
      &mut |line| log::info!("discord update: {line}"),
   )
   .map_err(|err| err.to_string())?;
   let path = crate::loader::update_path()?;
   crate::loader::write_apk(&apk, &path)?;
   drop(apk);
   drop(apks);

   set(running(
      build,
      "Opening Android's installer...",
      done,
      total,
   ));
   crate::install::open(&path)
}

/// The base, this phone's ABI and nearest density, and its language plus
/// English, as far as the build has them.
fn wanted(build: &Build) -> Result<Vec<String>, String> {
   let mut names = Vec::new();
   for need in ["base", ABI] {
      if !build.splits.contains_key(need) {
         return Err(format!(
            "Discord {} has no {need} split",
            build.version_code
         ));
      }
      names.push(need.to_owned());
   }
   let dpi = property(c"ro.sf.lcd_density")
      .and_then(|value| value.parse::<u32>().ok())
      .unwrap_or(480);
   if let Some((density, _)) = DENSITIES
      .iter()
      .filter(|(name, _)| build.splits.contains_key(&format!("config.{name}")))
      .min_by_key(|(_, bucket)| (bucket.abs_diff(dpi), u32::MAX - bucket))
   {
      names.push(format!("config.{density}"));
   }
   let locale = property(c"persist.sys.locale").or_else(|| property(c"ro.product.locale"));
   let language = locale
      .as_deref()
      .and_then(|locale| locale.split(['-', '_']).next())
      .unwrap_or("en");
   for language in [language, "en"] {
      let name = format!("config.{language}");
      if build.splits.contains_key(&name) && !names.contains(&name) {
         names.push(name);
      }
   }
   Ok(names)
}

/// A system property, or `None` when unset.
fn property(name: &CStr) -> Option<String> {
   let mut value = [0u8; libc::PROP_VALUE_MAX as usize];
   // SAFETY: `value` holds PROP_VALUE_MAX bytes, the most the call writes,
   // NUL included.
   let len = unsafe { libc::__system_property_get(name.as_ptr(), value.as_mut_ptr().cast()) };
   if len <= 0 {
      return None;
   }
   CStr::from_bytes_until_nul(&value)
      .ok()?
      .to_str()
      .ok()
      .map(str::to_owned)
}

/// Downloads split `name` in ranges, each within one response's limit, and
/// checks it against the build's hash.
fn download(build: &Build, name: &str, done: &mut usize, total: usize) -> Result<Vec<u8>, String> {
   let split = &build.splits[name];
   let url = format!("{}{name}", build.url);
   let mut bytes = Vec::with_capacity(split.size);
   let mut hash = Sha256::new();
   let step = format!("Downloading {name}...");
   while bytes.len() < split.size {
      let end = (bytes.len() + CHUNK).min(split.size) - 1;
      let range = format!("bytes={}-{end}", bytes.len());
      let chunk = fetch::get_with(&url, vec![("Range".to_owned(), range)])
         .map_err(|err| format!("{name}: {err}"))?;
      if chunk.is_empty() || bytes.len() + chunk.len() > split.size {
         return Err(format!("{name}: the server sent {} bytes", chunk.len()));
      }
      hash.update(&chunk);
      bytes.extend_from_slice(&chunk);
      *done += chunk.len();
      set(running(build, &step, *done, total));
   }
   let found = hash
      .finalize()
      .iter()
      .fold(String::with_capacity(64), |mut hex, byte| {
         let _ = write!(hex, "{byte:02x}");
         hex
      });
   if found != split.sha256 {
      return Err(format!("{name} differs from the checked build"));
   }
   Ok(bytes)
}
