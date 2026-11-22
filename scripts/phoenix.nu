# Builds the loader for arm64 Android, patches Discord's split APKs with it, installs them
# on the device and tails phoenix's log. Pass base.apk plus the arm64, density and language
# splits. Plugins ride along in the APK. Run inside `nix develop`. The bindgen override is explained in phone-demo.nu.

const API = 28
const PACKAGE = "com.discord"

def main [...apks: path] {
   let root = $env.FILE_PWD | path dirname
   let sysroot = $"($env.ANDROID_NDK_ROOT)/toolchains/llvm/prebuilt/linux-x86_64/sysroot"
   let apks = $apks | each { path expand }
   cd $root

   with-env {"BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android": $"--target=aarch64-linux-android($API) --sysroot=($sysroot)"} {
      ^cargo ndk -t arm64-v8a -P $API --link-builtins build --release -p phoenix-loader
   }
   ^cargo build --release -p patcher

   let release = $root | path join target aarch64-linux-android release
   let probe = glob $"($release)/build/phoenix-hook/*/out/libshadowhook_nothing.so"
      | sort-by { ls $in | get 0.modified }
      | last
   let out = $root | path join target phoenix
   rm -rf $out
   let plugins = $root | path join target phoenix-plugins
   rm -rf $plugins
   for plugin in (ls plugins | where type == dir | get name | path basename) {
      let header = open $"plugins/($plugin)/src/index.ts"
         | lines
         | take while {|line| $line starts-with "// @" }
         | str join (char nl)
      ^esbuild $"plugins/($plugin)/src/index.ts" --bundle --format=iife --target=es2023 --log-level=warning $"--banner:js=($header)" $"--outfile=($plugins)/($plugin).js"
   }
   let bundled = glob $"($plugins)/*.js" | each {|plugin| [--plugin $plugin] } | flatten
   ^($root | path join target release patcher) --out $out --loader ($release | path join libphoenix.so) --lib $probe ...$bundled ...$apks

   ^adb install-multiple -r ...(glob $"($out)/*.apk")
   ^adb shell am force-stop $PACKAGE
   ^adb logcat -c
   ^adb shell monkey -p $PACKAGE -c android.intent.category.LAUNCHER 1 | ignore
   ^adb logcat -s phoenix
}
