#!/bin/bash
# Build libplg.so (Rust workspace: loader + runtime + hook + sigs) for the web patcher:
# web/plugins/libplg-<abi>.so. Needs rustup targets aarch64-linux-android and x86_64-linux-android
# and an NDK. On Windows without Visual Studio, build scripts need a GNU host toolchain:
#   TOOLCHAIN=+stable-x86_64-pc-windows-gnu ./build.sh
set -euo pipefail
cd "$(dirname "$0")"
LAD=${LOCALAPPDATA:-}; command -v cygpath >/dev/null && [ -n "$LAD" ] && LAD=$(cygpath -u "$LAD")
SDK=${ANDROID_SDK:-$LAD/Android/Sdk}
NDK=${ANDROID_NDK:-$(ls -d "$SDK"/ndk/*/toolchains/llvm/prebuilt/*/bin | sort -V | tail -1 | sed "s|/toolchains/.*||")}
BIN=$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)
win() { command -v cygpath >/dev/null && cygpath -w "$1" || echo "$1"; }
ext=; [ -e "$BIN/clang.exe" ] && ext=.exe
mkdir -p ../web/plugins
# Java helper (file picker) -> hook/java/picker.dex, embedded in libplg. Needs a JDK; the committed
# dex is used as is without one.
if command -v javac >/dev/null; then
  JT=$(mktemp -d)
  javac --release 8 -nowarn -cp "$(win "$SDK/platforms/android-36/android.jar")" -d "$(win "$JT")" hook/java/phx/Picker.java
  D8=$(ls "$SDK"/build-tools/*/d8* | sort -V | tail -1)
  ( cd "$JT" && "$D8" --release --min-api 26 --lib "$(win "$SDK/platforms/android-36/android.jar")" --output . phx/*.class )
  cp "$JT/classes.dex" hook/java/picker.dex
  rm -rf "$JT"
fi
for t in aarch64:arm64 x86_64:x86_64; do
  triple=${t%%:*}-linux-android; abi=${t##*:}; T=$(echo "$triple" | tr 'a-z-' 'A-Z_'); t_=${triple//-/_}
  cc=$(ls "$BIN/${triple}28-clang.cmd" 2>/dev/null || ls "$BIN/${triple}28-clang")
  # clang itself as the linker: the NDK's .cmd wrappers mangle rustc's quoted arguments
  env "CC_$t_=$(win "$cc")" "AR_$t_=$(win "$BIN/llvm-ar$ext")" \
      "CARGO_TARGET_${T}_LINKER=$(win "$BIN/clang$ext")" "CARGO_TARGET_${T}_RUSTFLAGS=-Clink-arg=--target=${triple}28" \
      cargo ${TOOLCHAIN:-} build --release --target "$triple" -p loader
  cp "target/$triple/release/libplg.so" "../web/plugins/libplg-$abi.so"
  # ShadowHook dlopens this empty library at init (arm64 only, like ShadowHook itself)
  [ "$abi" = arm64 ] && "$cc" -shared -Oz -nostdlib -Wl,-soname,libshadowhook_nothing.so -Wl,--build-id=none     -Wl,-z,max-page-size=16384 -o ../web/plugins/libshadowhook_nothing-arm64.so hook/shadowhook/nothing/sh_nothing.c
done
ls -l ../web/plugins/libplg-*.so ../web/plugins/libshadowhook_nothing-*.so
