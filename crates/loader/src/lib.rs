//! `libphoenix.so` / `phoenix.dll`, injected through `DT_NEEDED` on Android,
//! as a dylib on iOS, or loaded into Desktop Phoenix on Windows.
//!
//! On Unix the constructor runs once the host library is mapped and relocated.
//! On Windows `DllMain` spawns a thread so startup never runs under the OS
//! loader lock. Plugins come from the bundle stored beside this library (when
//! present) plus `$PHOENIX_HOME/plugins`.

use std::{
   env,
   panic::{
      self,
      AssertUnwindSafe,
   },
   path::PathBuf,
};

use phoenix_runtime::{
   Plugin,
   console::Tee,
};

#[cfg(unix)]
#[used]
#[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".init_array"))]
#[cfg_attr(
   target_vendor = "apple",
   unsafe(link_section = "__DATA,__mod_init_func")
)]
static CONSTRUCTOR: extern "C" fn() = init;

#[cfg(windows)]
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllMain(
   module: *mut std::ffi::c_void,
   reason: u32,
   _reserved: *mut std::ffi::c_void,
) -> i32 {
   const DLL_PROCESS_ATTACH: u32 = 1;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn DisableThreadLibraryCalls(module: *mut std::ffi::c_void) -> i32;
   }

   if reason == DLL_PROCESS_ATTACH {
      // SAFETY: `module` is the HINSTANCE handed to DllMain.
      unsafe { DisableThreadLibraryCalls(module) };
      logging();
      let started = std::time::Instant::now();
      if let Err(err) = phoenix_runtime::prepare() {
         log::error!("phoenix could not hook Discord: {err}");
      }
      log::info!("hooked Discord in {:?}", started.elapsed());
      let _ = std::thread::Builder::new()
         .name("sunbird-init".to_owned())
         .spawn(|| init());
   }
   1
}

extern "C" fn init() {
   logging();
   // Unwinding out of a constructor aborts the host, so a bug here costs
   // plugins, not the app.
   match panic::catch_unwind(AssertUnwindSafe(|| {
      phoenix_runtime::start(home(), bundled())
   })) {
      Ok(Ok(started)) => {
         log::info!(
            "phoenix started on {} ({}), plugins: {:?}",
            started.target,
            started
               .build
               .map_or_else(|| "no build ID".to_owned(), |id| id.to_string()),
            started.plugins,
         );
      },
      Ok(Err(err)) => log::error!("phoenix did not start: {err}"),
      Err(_) => log::error!("phoenix panicked while starting"),
   }
}

fn home() -> PathBuf {
   if let Some(home) = env::var_os("PHOENIX_HOME") {
      return PathBuf::from(home);
   }
   #[cfg(unix)]
   {
      // /proc/self/cmdline starts with the package name, plus `:process` for
      // secondary processes.
      let package = std::fs::read("/proc/self/cmdline")
         .ok()
         .and_then(|cmdline| {
            let name = cmdline.split(|byte| *byte == 0).next()?.to_vec();
            String::from_utf8(name).ok()
         })
         .map_or_default(|name| name.split(':').next().unwrap_or_default().to_owned());
      PathBuf::from(format!("/data/data/{package}/files/phoenix"))
   }
   #[cfg(windows)]
   {
      // Sits directly under `%LOCALAPPDATA%\DiscordPhoenix\sunbird` so its
      // parent matches Phoenix's `store/<account>/cache.sqlite` root.
      let base = env::var_os("LOCALAPPDATA")
         .map(PathBuf::from)
         .unwrap_or_else(env::temp_dir);
      base.join("DiscordPhoenix").join("sunbird")
   }
}

fn bundled() -> Vec<Plugin> {
   #[cfg(unix)]
   {
      match unix::read_bundle() {
         Ok(plugins) => plugins,
         Err(err) => {
            log::error!("plugin bundle: {err}");
            Vec::new()
         },
      }
   }
   #[cfg(not(unix))]
   {
      Vec::new()
   }
}

#[cfg(unix)]
mod unix {
   use std::{
      collections::BTreeMap,
      ffi::{
         CStr,
         c_void,
      },
      fs::{
         self,
         File,
      },
      io::{
         ErrorKind,
         Read,
      },
      path::Path,
   };

   use misstep::ResultExt;
   use phoenix_runtime::Plugin;
   use phoenix_sigs::PLUGIN_BUNDLE;
   use zip::{
      ZipArchive,
      result::ZipError,
   };

   /// Reads [`PLUGIN_BUNDLE`] from beside this library. Android maps native
   /// libraries straight out of the APK, so `dladdr` names `<apk>!/lib/<abi>/...`
   /// and the bundle is a stored entry in the same split.
   pub fn read_bundle() -> misstep::Result<Vec<Plugin>> {
      let mut info = libc::Dl_info {
         dli_fname: std::ptr::null(),
         dli_fbase: std::ptr::null_mut(),
         dli_sname: std::ptr::null(),
         dli_saddr: std::ptr::null_mut(),
      };
      // SAFETY: dladdr fills `info` for an address inside a loaded module.
      if unsafe { libc::dladdr(super::init as *const c_void, &raw mut info) } == 0
         || info.dli_fname.is_null()
      {
         return Ok(Vec::new());
      }
      // SAFETY: dli_fname is NUL-terminated and lives as long as the module.
      let path = unsafe { CStr::from_ptr(info.dli_fname) }.to_str()?;

      let bytes = if let Some((apk, inner)) = path.split_once("!/") {
         let dir = inner.rsplit_once('/').map_or("", |(dir, _)| dir);
         let mut archive =
            ZipArchive::new(File::open(apk).with_context(|| format!("opening {apk}"))?)?;
         let mut entry = match archive.by_name(&format!("{dir}/{PLUGIN_BUNDLE}")) {
            Ok(entry) => entry,
            Err(ZipError::FileNotFound) => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
         };
         let mut bytes = Vec::new();
         entry.read_to_end(&mut bytes)?;
         bytes
      } else {
         match fs::read(Path::new(path).with_file_name(PLUGIN_BUNDLE)) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
         }
      };
      let plugins: BTreeMap<String, String> = serde_json::from_slice(&bytes)?;
      Ok(plugins
         .into_iter()
         .map(|(name, source)| Plugin { name, source })
         .collect())
   }
}

#[cfg(target_os = "android")]
fn logging() {
   let config = android_logger::Config::default()
      .with_tag("phoenix")
      .with_max_level(log::LevelFilter::Info);
   let logger = Tee(android_logger::AndroidLogger::new(config));
   if log::set_logger(Box::leak(Box::new(logger))).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}

#[cfg(not(target_os = "android"))]
fn logging() {
   struct Stderr;

   impl log::Log for Stderr {
      fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
         metadata.level() <= log::Level::Info
      }

      fn log(&self, record: &log::Record<'_>) {
         if self.enabled(record.metadata()) {
            eprintln!("[phoenix {}] {}", record.level(), record.args());
         }
      }

      fn flush(&self) {}
   }

   if log::set_logger(&Tee(Stderr)).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}
