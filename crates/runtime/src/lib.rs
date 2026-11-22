//! The plugin runtime the loader starts.
//!
//! [`start`] picks the target library among the loaded images, boots `QuickJS`
//! with the `plumage` API, and evaluates every bundle in `<home>/plugins`.
//! Every gateway registry entry points at plumage's detour from the start, and
//! a plugin's hook turns its event on.

use std::{
   path::{
      Path,
      PathBuf,
   },
   sync::{
      Mutex,
      OnceLock,
   },
};

use plumage_hook::Image;
use plumage_sigs::{
   Build,
   GatewayRegistry,
   LIBRARIES,
};

#[cfg(target_os = "android")] mod bridge;
pub mod console;
#[cfg(target_os = "android")] mod fetch;
mod gateway;
mod js;
#[cfg(target_os = "android")] mod jvm;
#[cfg(target_os = "android")] mod settings;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("no loaded library matches a known build or one of {LIBRARIES:?}")]
   NoTarget,
   #[error("the runtime is already running")]
   AlreadyStarted,
   #[error("the runtime has not started")]
   NotStarted,
   #[error("{path}: {source}")]
   Io {
      path:   PathBuf,
      #[source]
      source: std::io::Error,
   },
   #[error("javascript: {0}")]
   Js(String),
   #[error(transparent)]
   Sig(#[from] plumage_sigs::Error),
   #[error(transparent)]
   Hook(#[from] plumage_hook::Error),
   #[error("{url}: plugins install from https:// URLs ending in a name like plugin.js")]
   PluginUrl { url: String },
   #[error("{url}: the download is not UTF-8 text")]
   NotUtf8 { url: String },
   #[cfg(target_os = "android")]
   #[error(transparent)]
   Fetch(#[from] fetch::FetchError),
}

struct State {
   image: Image<'static>,
   build: Option<&'static Build>,
   home:  PathBuf,
   js:    Mutex<js::Engine>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// A plugin bundle, an esbuild IIFE that receives the `plumage` API.
pub struct Plugin {
   pub name:   String,
   pub source: String,
}

/// A plugin as the settings page shows it.
#[derive(Clone, Debug)]
pub struct PluginInfo {
   pub name:      String,
   pub enabled:   bool,
   pub error:     Option<String>,
   pub installed: bool,
   pub switches:  Vec<Switch>,
}

/// A plugin setting as the user currently has it.
#[derive(Clone, Debug)]
pub struct Switch {
   pub key:         String,
   pub title:       String,
   pub description: Option<String>,
   pub on:          bool,
}

fn engine() -> Option<std::sync::MutexGuard<'static, js::Engine>> {
   Some(
      STATE
         .get()?
         .js
         .lock()
         .unwrap_or_else(std::sync::PoisonError::into_inner),
   )
}

/// Every plugin found at start, enabled or not.
#[must_use]
pub fn plugins() -> Vec<PluginInfo> {
   engine().map_or_default(|engine| engine.plugins())
}

/// Turns a plugin on or off and remembers the choice across restarts.
pub fn set_enabled(name: &str, enabled: bool) {
   if let Some(mut engine) = engine() {
      engine.set_enabled(name, enabled);
   }
}

/// Sets one of a plugin's switches and remembers it across restarts.
pub fn set_switch(plugin: &str, key: &str, on: bool) {
   if let Some(engine) = engine() {
      engine.set_switch(plugin, key, on);
   }
}

/// Runs `code` in the plugin runtime's global scope and logs it with its
/// result.
pub fn evaluate(code: &str) {
   log::info!("> {code}");
   let result = engine().map(|engine| engine.evaluate(code));
   match result {
      Some(Ok(result)) => log::info!("{result}"),
      Some(Err(err)) => log::error!("{err}"),
      None => log::error!("the runtime is not running"),
   }
}

/// Downloads a plugin bundle into `home/plugins` and loads it, replacing a
/// plugin of the same name. Returns the plugin's name.
#[cfg(target_os = "android")]
pub fn install(url: &str) -> Result<String, Error> {
   let bad_url = || {
      Error::PluginUrl {
         url: url.to_owned(),
      }
   };
   let rest = url.strip_prefix("https://").ok_or_else(bad_url)?;
   let path = rest.split(['?', '#']).next().unwrap_or_default();
   let name = path
      .rsplit('/')
      .next()
      .and_then(|file| file.strip_suffix(".js"))
      .filter(|name| {
         !name.is_empty()
            && name
               .bytes()
               .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
      })
      .ok_or_else(bad_url)?;

   let body = fetch::get(url)?;
   let source = String::from_utf8(body).map_err(|_| {
      Error::NotUtf8 {
         url: url.to_owned(),
      }
   })?;
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   let write = || {
      std::fs::create_dir_all(state.plugins_dir())?;
      std::fs::write(&file, &source)
   };
   write().map_err(io_error(&file))?;
   if let Some(mut engine) = engine() {
      engine.install(name, source);
   }
   Ok(name.to_owned())
}

/// Deletes a plugin installed into `home/plugins` and unloads it.
#[cfg(target_os = "android")]
pub fn uninstall(name: &str) -> Result<(), Error> {
   let state = STATE.get().ok_or(Error::NotStarted)?;
   let file = state.plugins_dir().join(format!("{name}.js"));
   std::fs::remove_file(&file).map_err(io_error(&file))?;
   if let Some(mut engine) = engine() {
      engine.uninstall(name);
   }
   Ok(())
}

/// Evaluates an enabled plugin again with its hooks cleared.
pub fn reload(name: &str) {
   if let Some(mut engine) = engine() {
      engine.reload(name);
   }
}

/// What [`start`] found and loaded.
pub struct Started {
   pub target:  String,
   pub build:   Option<&'static str>,
   pub plugins: Vec<String>,
}

/// Starts plumage against the first loaded library that matches a known build
/// ID.
///
/// Falls back to the first library named in [`LIBRARIES`]. `bundled` plugins
/// load first, and a file in `home/plugins` with the same name replaces one.
pub fn start(home: PathBuf, bundled: Vec<Plugin>) -> Result<Started, Error> {
   // SAFETY: the target is the host app's own library, which stays mapped for
   // the life of the process, and scans run before the host's threads touch its
   // data.
   let images = unsafe { Image::loaded() };
   let known = |image: &Image<'_>| image.build_id.and_then(Build::find);
   let image = match images.iter().position(|image| known(image).is_some()) {
      Some(index) => images.into_iter().nth(index),
      None => {
         images
            .into_iter()
            .find(|image| LIBRARIES.contains(&image.file_name()))
      },
   }
   .ok_or(Error::NoTarget)?;
   let build = image.build_id.and_then(Build::find);
   let walked = GatewayRegistry::locate(&image)?;
   log::info!("gateway registry: {} events", walked.iter().count());

   let registry = gateway::swap(&image, walked)?;
   let engine = js::Engine::new(registry)?;
   let state = State {
      image,
      build,
      home,
      js: Mutex::new(engine),
   };
   STATE.set(state).map_err(|_| Error::AlreadyStarted)?;
   let state = STATE.get().expect("STATE was set above");
   std::thread::spawn(|| gateway::verify(state));

   let plugins = js::load_plugins(state, bundled)?;
   #[cfg(target_os = "android")]
   settings::start();
   Ok(Started {
      target: state.image.path.clone(),
      build: state.build.map(|build| build.label),
      plugins,
   })
}

impl State {
   fn storage_path(&self, plugin: &str) -> PathBuf {
      self.home.join("storage").join(format!("{plugin}.json"))
   }

   fn choices_path(&self) -> PathBuf {
      self.home.join("plugins.json")
   }

   fn plugins_dir(&self) -> PathBuf {
      self.home.join("plugins")
   }
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
   move |source| {
      Error::Io {
         path: path.to_owned(),
         source,
      }
   }
}
