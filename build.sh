#!/bin/bash
# Build the payload, the loader, and a patched Valenced module APK.
# usage: ./build.sh <clean-valenced.apk> <keystore.jks> <storepass>
set -euo pipefail
cd "$(dirname "$0")"

LAD=${LOCALAPPDATA:-}; command -v cygpath >/dev/null && [ -n "$LAD" ] && LAD=$(cygpath -u "$LAD")
SDK=${ANDROID_SDK:-$LAD/Android/Sdk}
NDK=${ANDROID_NDK:-$SDK/ndk/26.0.10792818}
BT=${BUILD_TOOLS:-$SDK/build-tools/36.0.0}
CC=$(ls "$NDK"/toolchains/llvm/prebuilt/*/bin/x86_64-linux-android33-clang* | head -1)
EXE=; [[ "$OSTYPE" == msys* || "$OSTYPE" == cygwin* ]] && EXE=.bat

BASE=${1:?clean Valenced APK}; KS=${2:?keystore}; PASS=${3:?store password}
rm -rf out && mkdir -p out/classes out/dex

"$CC" -O2 -shared -fPIC -o out/gatepayload.so native/accessfix.c -llog
"$CC" -O2 -shared -fPIC -o out/libgateprobe.so native/loader.c -llog

javac --release 8 -nowarn -cp "$SDK/platforms/android-33/android.jar" -d out/classes $(find java -name '*.java')
# only our classes go in the dex; the Xposed stubs are compile-time only
"$BT/d8$EXE" --min-api 26 --output out/dex $(find out/classes/gate -name '*.class')

python inject.py "$BASE" out/dex/classes.dex out/libgateprobe.so out/module-unsigned.apk
"$BT/zipalign" -p -f 4 out/module-unsigned.apk out/module-aligned.apk
"$BT/apksigner$EXE" sign --ks "$KS" --ks-pass "pass:$PASS" --key-pass "pass:$PASS" \
    --out out/valenced-phoenix-unlock.apk out/module-aligned.apk
echo "built out/valenced-phoenix-unlock.apk and out/gatepayload.so"
