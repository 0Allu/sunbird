//! Bakes the build's version into `PHOENIX_VERSION`: the commit's UTC date and
//! short hash, `2026.11.22.2049-146126e`, so versions sort by date. Without
//! git it is `0`, older than any site build.
//!
//! On Android it also compiles `java/phx/*.java` into `picker.dex` in
//! `OUT_DIR` with the SDK's `android.jar` and `d8`.

use std::{
   env,
   fs,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

fn main() {
   version();
   if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
      dex();
   }
}

fn version() {
   // Moves on every commit, checkout and reset.
   println!("cargo:rerun-if-changed=../../.git/logs/HEAD");
   let version = Command::new("git")
      .args([
         "log",
         "-1",
         "--format=%cd-%h",
         "--date=format-local:%Y.%m.%d.%H%M",
      ])
      .env("TZ", "UTC0")
      .output()
      .ok()
      .filter(|output| output.status.success())
      .and_then(|output| String::from_utf8(output.stdout).ok())
      .map(|version| version.trim().to_owned())
      .filter(|version| !version.is_empty())
      .unwrap_or_else(|| "0".to_owned());
   println!("cargo:rustc-env=PHOENIX_VERSION={version}");
}

/// The entry of `dir` that sorts last by its dotted version name.
fn newest(dir: &Path, prefix: &str) -> PathBuf {
   let version = |path: &PathBuf| {
      path
         .file_name()
         .and_then(|name| name.to_str())
         .and_then(|name| name.strip_prefix(prefix))
         .map(|rest| {
            rest
               .split(['.', '-'])
               .map(|part| part.parse::<u32>().unwrap_or_default())
               .collect::<Vec<_>>()
         })
   };
   fs::read_dir(dir)
      .unwrap_or_else(|error| panic!("{}: {error}", dir.display()))
      .filter_map(|entry| Some(entry.ok()?.path()))
      .filter(|path| version(path).is_some())
      .max_by_key(version)
      .unwrap_or_else(|| panic!("{}: no {prefix}* entry", dir.display()))
}

fn run(command: &mut Command) {
   let status = command
      .status()
      .unwrap_or_else(|error| panic!("{command:?}: {error}"));
   assert!(status.success(), "{command:?} failed with {status}");
}

fn dex() {
   println!("cargo:rerun-if-changed=java/phx");
   println!("cargo:rerun-if-env-changed=ANDROID_SDK_ROOT");
   let sdk = PathBuf::from(
      env::var_os("ANDROID_SDK_ROOT")
         .expect("ANDROID_SDK_ROOT is unset, build inside `nix develop`"),
   );
   let jar = newest(&sdk.join("platforms"), "android-").join("android.jar");
   let d8 = newest(&sdk.join("build-tools"), "").join("d8");
   let out = PathBuf::from(env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
   let classes = out.join("classes");
   let _ = fs::remove_dir_all(&classes);

   let sources = fs::read_dir("java/phx")
      .expect("java/phx")
      .filter_map(|entry| Some(entry.ok()?.path()))
      .filter(|path| {
         path
            .extension()
            .is_some_and(|extension| extension == "java")
      })
      .collect::<Vec<_>>();
   run(
      Command::new("javac")
         .args(["--release", "8", "-nowarn", "-cp"])
         .arg(&jar)
         .arg("-d")
         .arg(&classes)
         .args(&sources),
   );

   let compiled = fs::read_dir(classes.join("phx"))
      .expect("javac wrote no classes")
      .filter_map(|entry| Some(entry.ok()?.path()))
      .collect::<Vec<_>>();
   run(
      Command::new(d8)
         .args(["--min-api", "28", "--lib"])
         .arg(&jar)
         .arg("--output")
         .arg(&out)
         .args(&compiled),
   );
   fs::rename(out.join("classes.dex"), out.join("picker.dex")).expect("d8 wrote no classes.dex");
}
