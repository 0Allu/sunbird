# Builds the loader for arm64 Android and copies libphoenix.so and the
# libshadowhook_nothing.so it needs into `out`, named the way web/lib/apk.js
# expects. Run inside `nix develop`.

const API = 28

def main [out: path] {
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
   mkdir $out
   cp ($release | path join libphoenix.so) ($out | path join libphoenix-arm64.so)
   cp $probe ($out | path join libshadowhook_nothing-arm64.so)
}
