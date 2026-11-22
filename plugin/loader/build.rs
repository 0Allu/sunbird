fn main() {
    // the patched host lib asks for "libplg.so" by DT_NEEDED
    println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libplg.so");
}
