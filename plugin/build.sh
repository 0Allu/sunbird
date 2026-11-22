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
for t in aarch64:arm64 x86_64:x86_64; do
  triple=${t%%:*}-linux-android; abi=${t##*:}; T=$(echo "$triple" | tr 'a-z-' 'A-Z_'); t_=${triple//-/_}
  cc=$(ls "$BIN/${triple}28-clang.cmd" 2>/dev/null || ls "$BIN/${triple}28-clang")
  # clang itself as the linker: the NDK's .cmd wrappers mangle rustc's quoted arguments
  env "CC_$t_=$(win "$cc")" "AR_$t_=$(win "$BIN/llvm-ar$ext")" \
      "CARGO_TARGET_${T}_LINKER=$(win "$BIN/clang$ext")" "CARGO_TARGET_${T}_RUSTFLAGS=-Clink-arg=--target=${triple}28" \
      cargo ${TOOLCHAIN:-} build --release --target "$triple" -p loader
  cp "target/$triple/release/libplg.so" "../web/plugins/libplg-$abi.so"
done
ls -l ../web/plugins/libplg-*.so
