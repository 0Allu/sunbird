//! Compiles `java/phx/*.java` into `picker.dex` in `OUT_DIR` with the SDK's
//! `android.jar` and `d8`.

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
   if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("android") {
      dex();
   }
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
   let d8_name = if cfg!(windows) { "d8.bat" } else { "d8" };
   let d8 = newest(&sdk.join("build-tools"), "").join(d8_name);
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
