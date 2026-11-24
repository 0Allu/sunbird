# Builds what the site serves but does not commit, inside `nix develop`: the
# loader for arm64 Android and the libshadowhook_nothing.so it needs, into
# `web/plugins` under the names web/app.js fetches, and the patcher as wasm
# into `web/pkg` for web/patcher.js.

const API = 28

def main [web: path] {
   let root = $env.FILE_PWD | path dirname
   let sysroot = $"($env.ANDROID_NDK_ROOT)/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
   cd $root

   with-env {"BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android": $"--target=aarch64-linux-android($API) --sysroot=($sysroot)"} {
      ^cargo ndk -t arm64-v8a -P $API --link-builtins build --release -p phoenix-loader
   }

   let release = $root | path join target aarch64-linux-android release
   let probe = glob $"($release)/build/phoenix-hook/*/out/libshadowhook_nothing.so"
      | sort-by { ls $in | get 0.modified }
      | last
   let plugins = $web | path join plugins
   mkdir $plugins
   cp ($release | path join libphoenix.so) ($plugins | path join libphoenix-arm64.so)
   cp $probe ($plugins | path join libshadowhook_nothing-arm64.so)

   ^cargo build --profile wasm -p patcher-web --target wasm32-unknown-unknown
   let wasm = $root | path join target wasm32-unknown-unknown wasm patcher_web.wasm
   ^wasm-bindgen --target web --no-typescript --out-dir ($web | path join pkg) $wasm
}
