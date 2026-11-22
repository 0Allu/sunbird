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
mod gateway;
mod js;
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
   image:    Image<'static>,
   build:    Option<&'static Build>,
   home:     PathBuf,
   js:       Mutex<js::Engine>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// A plugin bundle, an esbuild IIFE that receives the `plumage` API.
pub struct Plugin {
   pub name:   String,
   pub source: String,
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
   settings::start(plugins.clone());
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
