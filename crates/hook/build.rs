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

   // bytehook's PLT path calls into shadowhook's linker monitor on ARM, which
   // sunhook-sys links. Both trees carry the same bytesig.c, and only
   // sunhook's is kept.
   let mut build = cc::Build::new();

   // bytehook's C constructors (errno TLS key, signal guards) must run before
   // the loader's, which sits in plain .init_array ahead of these archive
   // objects. A priority moves them into .init_array.00101, which the linker
   // orders first.
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
      build.include(env::var_os("DEP_SHADOWHOOK_INCLUDE").expect("sunhook-sys sets its include"));
      sources.retain(|path| path.file_name().is_some_and(|name| name != "bytesig.c"));
   }
   build.files(&sources).compile("phoenix_native_hooks");
   println!("cargo:rustc-link-lib=log");
   println!("cargo:rustc-link-lib=dl");
   bind(
      &bytehook.join("include/bytehook.h"),
      "bytehook_.*",
      &out.join("bytehook.rs"),
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
   let mut files = std::fs::read_dir(dir)
      .unwrap_or_else(|err| panic!("{}: {err}", dir.display()))
      .map(|entry| entry.unwrap().path())
      .filter(|path| {
         path.extension().is_some_and(|ext| ext == "c" || ext == "S")
            && path
               .file_name()
               .is_some_and(|name| !skip.iter().any(|skip| name == *skip))
      })
      .collect::<Vec<PathBuf>>();
   files.sort();
   files
}

fn bind(header: &Path, allow: &str, out: &Path) {
   bindgen::Builder::default()
      .header(header.to_str().unwrap())
      .allowlist_function(allow)
      .allowlist_type(allow)
      .allowlist_var("BYTEHOOK_.*")
      .generate()
      .unwrap_or_else(|err| panic!("bindgen {}: {err}", header.display()))
      .write_to_file(out)
      .unwrap();
}
