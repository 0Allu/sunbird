#!/usr/bin/env bash
# Rebuilds picker.dex (embedded by src/picker.rs) from phx/*.java. Needs a JDK and
# ANDROID_SDK_ROOT with a platform and build-tools; the committed dex is used otherwise.
set -euo pipefail
cd "$(dirname "$0")"
sdk=${ANDROID_SDK_ROOT:?ANDROID_SDK_ROOT is unset}
jar=$(ls -d "$sdk"/platforms/android-*/android.jar | sort -V | tail -1)
d8=$(ls -d "$sdk"/build-tools/*/ | sort -V | tail -1)d8
[ -e "$d8" ] || d8=$d8.bat
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
javac --release 8 -nowarn -cp "$jar" -d "$tmp" phx/*.java
"$d8" --min-api 28 --lib "$jar" --output "$tmp" "$tmp"/phx/*.class
cp "$tmp/classes.dex" picker.dex
