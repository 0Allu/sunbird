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
   pub name:     String,
   pub enabled:  bool,
   pub error:    Option<String>,
   pub switches: Vec<Switch>,
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
