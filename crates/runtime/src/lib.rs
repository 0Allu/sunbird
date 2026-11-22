//! The plugin runtime the loader starts.
//!
//! [`start`] picks the target library among the loaded images, boots `QuickJS`
//! with the `plumage` API, and evaluates every bundle in `<home>/plugins`.
//! Plugins register gateway hooks, which resolve a [`plumage_sigs::Sig`]
//! against the target and attach an inline hook.

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
   LIBRARIES,
};

mod gateway;
mod js;

#[derive(Debug, thiserror::Error)]
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
   #[error("all {0} gateway detour slots are in use")]
   OutOfSlots(usize),
}

struct State {
   image:   Image<'static>,
   build:   Option<&'static Build>,
   home:    PathBuf,
   js:      Mutex<js::Engine>,
   gateway: Mutex<gateway::Installed>,
}

static STATE: OnceLock<State> = OnceLock::new();

/// What [`start`] found and loaded.
pub struct Started {
   pub target:  String,
   pub build:   Option<&'static str>,
   pub plugins: Vec<String>,
}

/// Starts plumage against the first loaded library that matches a known build
/// ID, falling back to the first one named in [`LIBRARIES`].
pub fn start(home: PathBuf) -> Result<Started, Error> {
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

   let engine = js::Engine::new()?;
   let state = State {
      image,
      build,
      home,
      js: Mutex::new(engine),
      gateway: Mutex::new(gateway::Installed::default()),
   };
   STATE.set(state).map_err(|_| Error::AlreadyStarted)?;
   let state = STATE.get().expect("STATE was set above");

   let plugins = js::load_plugins(state)?;
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
