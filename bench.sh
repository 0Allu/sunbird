#!/bin/bash
# Cold-start Phoenix N times and print ld-perf milestones (ms since process start).
export MSYS_NO_PATHCONV=1
N=${1:-5}
echo "run first_frame hydrated ready fully_loaded fcp patched"
for r in $(seq 1 "$N"); do
  adb shell am force-stop com.discord; sleep 2; adb logcat -c
  adb shell monkey -p com.discord -c android.intent.category.LAUNCHER 1 >/dev/null 2>&1
  sleep 15
  L=$(adb logcat -d)
  g() { echo "$L" | grep -m1 "ld-perf: core:$1 " | sed -E 's/.*\+([0-9]+)ms.*/\1/'; }
  fcp=$(echo "$L" | grep -m1 app_ui_viewed2 | sed -E 's/.*"time_first_contentful_paint":([0-9]+).*/\1/')
  p=$(echo "$L" | grep -c "accessfix: patched")
  echo "$r $(g first_frame) $(g store_hydrated) $(g ready) $(g fully_loaded) $fcp $p"
done
