#!/bin/bash
# Build the native plugin host for the web patcher: web/plugins/libplg-<abi>.so
set -euo pipefail
cd "$(dirname "$0")"
LAD=${LOCALAPPDATA:-}; command -v cygpath >/dev/null && [ -n "$LAD" ] && LAD=$(cygpath -u "$LAD")
SDK=${ANDROID_SDK:-$LAD/Android/Sdk}
NDK=${ANDROID_NDK:-$(ls -d "$SDK"/ndk/*/toolchains/llvm/prebuilt/*/bin | sort -V | tail -1 | sed "s|/toolchains/.*||")}
BIN=$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)
mkdir -p ../web/plugins
for t in aarch64:arm64 x86_64:x86_64; do
  triple=${t%%:*}; abi=${t##*:}
  cc=$(ls "$BIN/$triple-linux-android28-clang"* | head -1)
  "$cc" -O2 -shared -fPIC -fvisibility=hidden -Wl,-soname,libplg.so -Wall -Wno-unused-function \
    -o "../web/plugins/libplg-$abi.so" plugin.c -llog
  "$BIN/llvm-strip" --strip-unneeded "../web/plugins/libplg-$abi.so"
done
ls -l ../web/plugins
