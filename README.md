# phoenix-unlock

Makes Discord's Phoenix client (the native Rust client in Discord Android **356.7 Alpha,
versionCode 356207, x86_64**) usable on an account without Phoenix access, and turns on
its staff-only settings (DevTools, Experiment Overrides).

It is an add-on to the Valenced Xposed module: a small Java hook plus a native payload
that patches `liblibdiscord_wasm_host.so` in memory. Offsets apply to this exact build
only. The payload checks the original bytes first and skips any patch that doesn't match.

## Patches

| name   | offset      | function                                                                                  | patch                  |
|--------|-------------|-------------------------------------------------------------------------------------------|------------------------|
| access | `0x2eac1c4` | `ops_surfaces::client_access` evaluator for experiments `2026-11-phoenix-app-access` / `2026-11-phoenix-rollout` (with overrides). Returns `Option<bool>`, where 2 = unknown. | `mov al,1; ret` |
| staff  | `0x1f76640` | `is_staff(handle, force)`, 17 call sites through GOT slot `0x6bc5f30`. | jump to a C hook (see below) |

### Staff

Staff is checked as `(user.flags | flag_override.unwrap_or(0)) & 1`. `user.flags` is at
`state+0x84f0`. `flag_override` is an `Option<u32>`, with its tag at `+0x8580` and its
value at `+0x8588`. The compiler inlined this check in about 30 places and also left 6
standalone copies. Stubbing only one copy turns on the Staff Settings UI, but other copies
still refuse. For example, the setter for "Show Dev Tools Widget" goes through
`0x1f71c0d`, so the toggle kept flipping back.

The fix targets the shared data instead. The hook on `0x1f76640` takes the same store
read-guard the original function takes. It sets `flag_override = Some(STAFF)` in the
state and returns true. Every copy, standalone or inlined, reads that field, so they all
see the account as staff. The hook runs on every call, which happens often, so the flag
is written again if the state is rebuilt.

The access patch removes the gate where the decision is made, so the `ClientAccessGate`
screen ("No Access") is never built and the UI tree stays in sync. Two earlier approaches
broke input: dropping the gate's UI op batches, and stubbing the gate view.

The Java side only hooks `PhoenixLaunch.i(ContextWrapper)` to force Phoenix to launch.
`PhoenixLaunch.d` must not be hooked, because it causes a launch loop.

## Layout

- `native/accessfix.c`: the payload (`gate_payload_init`). It polls every 1 ms for the
  host lib to load, then applies the patches.
- `native/loader.c`: built as `libgateprobe.so` inside the module. It copies
  `/data/local/tmp/gatepayload.so` into the app's files dir and dlopens it. Payload
  changes only need `adb push` and an app restart.
- `java/gate/probe/*`: the Xposed entry `gate.probe.Probe`. `java/de/robv/...` holds
  compile-time stubs that are not shipped.
- `inject.py`: adds `classes2.dex`, the loader, and the `xposed_init` entry to a clean
  Valenced APK.
- `bench.sh`: cold-starts Phoenix N times and prints `ld-perf` milestones.

## Build and install

Requirements: Android SDK (build-tools 36, platform 33), NDK 26, a JDK, and Python 3.

```sh
./build.sh <clean-valenced.apk> <keystore.jks> <storepass>
adb push out/gatepayload.so /data/local/tmp/gatepayload.so
adb shell chmod 644 /data/local/tmp/gatepayload.so
adb install -r out/valenced-phoenix-unlock.apk
adb reboot   # LSPosed caches the old module path; reboot after every module reinstall
```

Enable the module for `com.discord` in LSPosed. To check that the patches applied:
`adb logcat | grep GateProbe` should print `patch access: applied` and
`patch staff: applied`.

DevTools: open `discord://app/settings/devtools`
(`adb shell am start -a android.intent.action.VIEW -d discord://app/settings/devtools com.discord`).
Experiment Overrides is under **Experiments** in that list. Settings also shows a
**Staff Settings** section. In that section, **Show Dev Tools Widget** adds a floating
DevTools bubble.

## Startup (WSA, x86_64, cached session, 180 guilds)

`bench.sh 5`, runs 2–5 (run 1 was a cold-disk outlier at 4.0 s fully loaded):

| milestone          | ms        |
|--------------------|-----------|
| first frame        | 165–175   |
| store hydrated     | 256–275   |
| first contentful   | 272–292   |
| gateway READY      | 1527–1683 |
| fully loaded       | 1625–1776 |
