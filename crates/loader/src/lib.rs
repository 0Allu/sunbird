//! `libplumage.so`, injected through a `DT_NEEDED` entry on Android and as a
//! dylib on iOS.
//!
//! The constructor runs once the host library is mapped and relocated, before
//! the host's own constructors and before the app touches the gateway, so hooks
//! are in place for the first dispatch. Plugins come from
//! `$PLUMAGE_HOME/plugins`, which defaults to the app's `files/plumage`
//! directory on Android.

use std::{
   env,
   fs,
   panic::{
      self,
      AssertUnwindSafe,
   },
   path::PathBuf,
};

#[used]
#[cfg_attr(not(target_vendor = "apple"), unsafe(link_section = ".init_array"))]
#[cfg_attr(
   target_vendor = "apple",
   unsafe(link_section = "__DATA,__mod_init_func")
)]
static CONSTRUCTOR: extern "C" fn() = init;

extern "C" fn init() {
   logging();
   // Unwinding out of a constructor aborts the host, so a bug here costs
   // plugins, not the app.
   match panic::catch_unwind(AssertUnwindSafe(|| plumage_runtime::start(home()))) {
      Ok(Ok(started)) => {
         log::info!(
            "plumage started on {} ({}), plugins: {:?}",
            started.target,
            started.build.unwrap_or("unknown build, portable rules"),
            started.plugins,
         );
      },
      Ok(Err(err)) => log::error!("plumage did not start: {err}"),
      Err(_) => log::error!("plumage panicked while starting"),
   }
}

fn home() -> PathBuf {
   if let Some(home) = env::var_os("PLUMAGE_HOME") {
      return PathBuf::from(home);
   }
   // /proc/self/cmdline starts with the package name, plus `:process` for
   // secondary processes.
   let package = fs::read("/proc/self/cmdline")
      .ok()
      .and_then(|cmdline| {
         let name = cmdline.split(|byte| *byte == 0).next()?.to_vec();
         String::from_utf8(name).ok()
      })
      .map_or_default(|name| name.split(':').next().unwrap_or_default().to_owned());
   PathBuf::from(format!("/data/data/{package}/files/plumage"))
}

#[cfg(target_os = "android")]
fn logging() {
   android_logger::init_once(
      android_logger::Config::default()
         .with_tag("plumage")
         .with_max_level(log::LevelFilter::Info),
   );
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
            eprintln!("[plumage {}] {}", record.level(), record.args());
         }
      }

      fn flush(&self) {}
   }

   if log::set_logger(&Stderr).is_ok() {
      log::set_max_level(log::LevelFilter::Info);
   }
}
