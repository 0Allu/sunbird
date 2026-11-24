# Builds what the site serves but does not commit, inside `nix develop`: the
# loader for arm64 and armv7 Android and the libshadowhook_nothing.so it needs, into
# `web/plugins` under the names sunbird-tools/site fetches, and the patcher as
# wasm into `web/pkg`.

const API = 28

def main [web: path] {
   let root = $env.FILE_PWD | path dirname
   let sysroot = $"($env.ANDROID_NDK_ROOT)/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
   cd $root

   let plugins = $web | path join plugins
   mkdir $plugins
   for abi in [
      {android: arm64-v8a, rust: aarch64-linux-android, name: arm64}
      {android: armeabi-v7a, rust: armv7-linux-androideabi, name: armv7}
   ] {
      with-env {$"BINDGEN_EXTRA_CLANG_ARGS_($abi.rust)": $"--target=($abi.rust)($API) --sysroot=($sysroot)"} {
         ^cargo ndk -t $abi.android -P $API --link-builtins build --release -p phoenix-loader
      }
      let release = $root | path join target $abi.rust release
      let probe = glob $"($release)/build/phoenix-hook/*/out/libshadowhook_nothing.so"
         | sort-by { ls $in | get 0.modified }
         | last
      cp ($release | path join libphoenix.so) ($plugins | path join $"libphoenix-($abi.name).so")
      cp $probe ($plugins | path join $"libshadowhook_nothing-($abi.name).so")
   }

   ^cargo build --profile wasm -p patcher-web --target wasm32-unknown-unknown
   let wasm = $root | path join target wasm32-unknown-unknown wasm patcher_web.wasm
   ^wasm-bindgen --target web --out-dir ($web | path join pkg) $wasm
}
