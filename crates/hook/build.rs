use std::{
   env,
   path::{
      Path,
      PathBuf,
   },
};

fn main() {
   let os = env::var("CARGO_CFG_TARGET_OS").unwrap();
   let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
   if os != "android" {
      return;
   }

   let out = PathBuf::from(env::var("OUT_DIR").unwrap());
   let bytehook = source_dir("BYTEHOOK_SRC");
   let arm = matches!(arch.as_str(), "aarch64" | "arm");

   // bytehook's PLT path calls into shadowhook's linker monitor on ARM, so both
   // get compiled into one archive. Both trees carry a copy of bytesig.c, and
   // only shadowhook's is kept.
   let mut build = cc::Build::new();

   // Their C constructors (errno TLS key, signal guards, linker offsets) must
   // run before the loader's, which sits in plain .init_array ahead of these
   // archive objects. A priority moves them into .init_array.00101, which the
   // linker orders first.
   build
      .std("c11")
      .flag("-Os")
      .warnings(false)
      .define("NDEBUG", None)
      .define("constructor", "constructor(101)");
   for dir in ["", "third_party/bsd", "third_party/lss", "include"] {
      build.include(bytehook.join(dir));
   }

   let mut sources = c_files(&bytehook, &["bh_jni.c"]);
   if arm {
      let shadowhook = source_dir("SHADOWHOOK_SRC");
      let isa = if arch == "aarch64" { "arm64" } else { "arm" };
      for dir in [
         "",
         "include",
         "common",
         "third_party/xdl",
         "third_party/bsd",
         "third_party/lss",
      ] {
         build.include(shadowhook.join(dir));
      }
      build.include(shadowhook.join("arch").join(isa));
      sources.retain(|path| path.file_name().is_some_and(|name| name != "bytesig.c"));
      sources.extend(c_files(&shadowhook, &["sh_jni.c"]));
      for dir in ["common", "third_party/xdl"] {
         sources.extend(c_files(&shadowhook.join(dir), &[]));
      }
      sources.extend(c_files(&shadowhook.join("arch").join(isa), &[]));
      probe_library(&shadowhook, &out);
      bind(
         &shadowhook.join("include/shadowhook.h"),
         "shadowhook_.*",
         &out.join("shadowhook.rs"),
      );
   }
   build.files(&sources).compile("plumage_native_hooks");
   println!("cargo:rustc-link-lib=log");
   println!("cargo:rustc-link-lib=dl");
   bind(
      &bytehook.join("include/bytehook.h"),
      "bytehook_.*",
      &out.join("bytehook.rs"),
   );
}

/// `shadowhook_init` dlopens `libshadowhook_nothing.so` by name to learn the
/// linker's soinfo layout, and fails without it. It has to ship next to
/// `libplumage.so`, in the APK's native library directory, where the linker
/// searches.
fn probe_library(shadowhook: &Path, out: &Path) {
   let library = out.join("libshadowhook_nothing.so");
   let status = cc::Build::new()
      .get_compiler()
      .to_command()
      .args([
         "-shared",
         "-Oz",
         "-nostdlib",
         "-Wl,-soname,libshadowhook_nothing.so",
         "-o",
      ])
      .arg(&library)
      .arg(shadowhook.join("nothing/sh_nothing.c"))
      .status()
      .expect("running the C compiler");
   assert!(
      status.success(),
      "building libshadowhook_nothing.so failed with {status}"
   );
}

fn source_dir(var: &str) -> PathBuf {
   println!("cargo:rerun-if-env-changed={var}");
   env::var_os(var).map_or_else(
      || panic!("{var} is unset, enter the devshell with `nix develop`"),
      PathBuf::from,
   )
}

fn c_files(dir: &Path, skip: &[&str]) -> Vec<PathBuf> {
   let mut files: Vec<_> = std::fs::read_dir(dir)
      .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
      .map(|entry| entry.unwrap().path())
      .filter(|path| {
         path.extension().is_some_and(|ext| ext == "c" || ext == "S")
            && path
               .file_name()
               .is_some_and(|name| !skip.iter().any(|skip| name == *skip))
      })
      .collect();
   files.sort();
   files
}

fn bind(header: &Path, allow: &str, out: &Path) {
   bindgen::Builder::default()
      .header(header.to_str().unwrap())
      .allowlist_function(allow)
      .allowlist_type(allow)
      .allowlist_var("(SHADOWHOOK|BYTEHOOK)_.*")
      .generate()
      .unwrap_or_else(|err| panic!("bindgen {}: {err}", header.display()))
      .write_to_file(out)
      .unwrap();
}
