// ShadowHook (inline hooks) is arm64-only; other targets build without it (see src/inline.rs).
fn main() {
    println!("cargo:rerun-if-changed=shadowhook");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("aarch64") { return; }
    let dir = std::path::Path::new("shadowhook");
    let mut b = cc::Build::new();
    for sub in ["", "arch/arm64", "common", "third_party/xdl"] {
        for e in std::fs::read_dir(dir.join(sub)).unwrap().flatten() {
            let p = e.path();
            if matches!(p.extension().and_then(|x| x.to_str()), Some("c" | "S")) { b.file(p); }
        }
    }
    for inc in ["", "include", "arch/arm64", "common", "third_party/xdl", "third_party/bsd", "third_party/lss"] { b.include(dir.join(inc)); }
    b.flag("-std=c11").flag("-Os").flag("-ffunction-sections").flag("-fdata-sections").flag("-fvisibility=hidden")
        .flag("-Wno-everything").define("_GNU_SOURCE", None);
    // ShadowHook needs compiler-rt builtins (__clear_cache), which rustc's -nodefaultlibs leaves out
    let rt = b.get_compiler().to_command().arg("--print-libgcc-file-name").output().expect("clang --print-libgcc-file-name");
    let rt = std::path::PathBuf::from(String::from_utf8_lossy(&rt.stdout).trim());
    b.compile("shadowhook");
    println!("cargo:rustc-link-search=native={}", rt.parent().unwrap().display());
    println!("cargo:rustc-link-lib=static={}", rt.file_stem().unwrap().to_str().unwrap().trim_start_matches("lib"));
    println!("cargo:rustc-link-lib=log");
}
