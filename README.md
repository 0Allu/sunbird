# phoenix-unlock

Makes Discord's Phoenix client usable on an account without Phoenix access, and turns on
its staff-only settings (DevTools, Experiment Overrides). Phoenix is the native Rust client
in Discord Android alpha builds.

There are two ways to apply it:

1. **APK patcher (`patcher/`)**: no root and no Xposed. It patches the Discord split APKs
   directly and re-signs them. Targets are found by anchors (strings and code shape), not
   fixed offsets, so after a Discord update you just run it again. Supports arm64 and
   x86_64.
2. **Xposed module (`native/`, `java/`)**: patches the library in memory at runtime, with
   offsets hardcoded for 356207 x86_64. Used for WSA with LSPosed.

## Web patcher

https://mantikafasi.github.io/phoenix-unlock/

Open it on the phone. The page downloads the Discord splits, patches them, merges them into
one APK and signs it, all in the browser, so Android can install it with a single tap.
`web/lib/` is a JS port of the APK patcher, for arm64 and x86_64 (WSA, emulators, x86
Chromebooks; the CPU select defaults to x86_64 when the browser runs on x86 Android/ChromeOS):

- `arm64.js`, `x86.js`: the same anchors as `patcher/sopatch.py`, with a hand-written decoder and a
  hand-encoded staff hook per ABI. Their output is byte-identical to the Python/clang build.
  The ABI is taken from the split (`config.arm64_v8a` or `config.x86_64`); libplg and
  frida-gadget are picked to match.
- `dex.js`: patches `PhoenixLaunch.i`.
- `axml.js`: removes `requiredSplitTypes`/`splitTypes` from the manifest and sets
  `com.android.vending.splits.required` to false. Optionally renames the package; the site
  does this by default ("Next to Discord", with an editable package name). It changes `package`, the provider authorities,
  `com.discord.*` permissions and task affinities to `com.discord.phoenix` (a name Phoenix's
  native lib already knows) and sets the label to "Discord Phoenix". Class names stay
  unchanged. `arsc.js` renames the resource table package to match, so `getIdentifier()`
  still works. Tested next to the stock `com.discord` on Android 16: it installs and boots
  Phoenix.
- `arsc.js`: merges the density and language splits' `resources.arsc` into base's.
- `zip.js`: copies unchanged entries without recompressing them and aligns stored entries
  (`.so` to 16 KiB).
- `sign.js`: APK Signature Scheme v2 with WebCrypto, RSA-2048.

**Signing:** every build is signed with the one key in `web/signing-key.json`, whether it
comes from the site, `node-build` or `patch.py`. So any patched build installs over the
previous one as an update and you stay logged in. The key is public (the page signs in the
browser), which means anyone can produce an APK that installs over this one.

The APKs come from `tracker.vendetta.rocks`, which redirects to Google's CDN. The CDN sends
no CORS headers, so the page downloads through `web/worker/proxy.js`, a Cloudflare Worker
deployed at `https://phoenix-apk-proxy.mantikafasi.workers.dev`. It only forwards
`/tracker/index` and `/tracker/download/<vc>/<split>`, and caches them at the edge: splits for a year (they never change per versionCode, and browsers get `immutable` too), the index and 404s for 5 minutes. Responses carry `X-Cache: HIT|MISS`. Redeploy it with
`npx wrangler deploy web/worker/proxy.js --name phoenix-apk-proxy --compatibility-date 2026-11-01`.
The URL is set in `web/config.js`, or can be overridden with `?proxy=`. The tracker only
mirrors some density splits (for 356207, only hdpi and xxhdpi), so the page falls back to
the nearest one it has. If downloads are blocked, the page switches to a manual mode: it
links each split and the user picks
the downloaded files.

Local test, using the same code as the page:

```sh
node web/tools/node-build.mjs out.apk base.apk config.arm64_v8a.apk config.xxhdpi.apk config.en.apk
node web/tools/dev-server.mjs <dir with the split apks>   # then open http://localhost:8787/?proxy=http://localhost:8787
```

Verified on 356207 arm64 (Android 16) and x86_64 (WSA, Android 13, MessageLogger + Frida).
The APK built in the browser is byte-identical to the Node build. It installs and runs:
Phoenix reaches READY in 3.1 s, chat loads, and drawables render. The whole page flow also runs in the phone's own Chrome in about 30 s.

### Plugins

Optional, ticked on the page. The patcher renames the host lib's `DT_NEEDED` `liblog.so` to
`libplg.so`, the plugin host (prebuilt in `web/plugins/`, rebuild with `plugin/build.sh`):

| crate (`plugin/`) | what |
|---|---|
| `loader` | the cdylib. Its `.init_array` constructor checks the config block, hooks the registry, reads the plugin bundle out of the APK (`lib/<abi>/libplg.js.so`) and starts the runtime. |
| `sigs` | the typed config block the patcher fills: host lib build ID (a different build runs without plugins), gateway registry anchor, store serializer offsets. |
| `hook` | gateway registry hooks (handler swap, no inline hooks), the JNI bridge (ART table filters + `nativeDispatch`), module/dynsym helpers and an IDA-pattern scanner. |
| `runtime` | rquickjs (QuickJS-ng). The `phoenix` API is typed in `runtime/phoenix.d.ts`; `plugins/example` is a TypeScript plugin bundled with esbuild. |

JS plugins live in `files/phoenix-js/`; ones shipped in the APK are copied there and replace older copies.

- **MessageLogger** (`web/plugins/message-logger.js`): a JS plugin. Deleted messages stay, marked
  "(deleted)"; edits keep the earlier text under the new one.
- **Frida (JS plugins)**: chains frida-gadget 17.27.4 (`libfrg.so`, wxWindows Library Licence)
  in as libplg's dependency, in script mode, so the app starts without a PC. The bootstrap
  (`frida/bootstrap.js`, built into `web/plugins/frida-bootstrap.js` by `frida/build.sh`) runs
  the plugins from `files/phoenix-plugins/`; ones picked on the page are copied there on first
  start. A draggable **JS** button on every screen opens the panel: plugin list, add from URL,
  reload, a JS prompt and the console. The panel also has an optional debug shell on
  127.0.0.1:27043 (token shown in the panel; `adb forward tcp:27043 tcp:27043`), which takes
  one JSON per line: `{"id":1,"eval":"..."}`.

## APK patcher (Python, arm64 + x86_64)

Requirements: Python 3 with `capstone` and `numpy`, Android build-tools (zipalign and
apksigner), and the NDK (clang and lld, used to assemble the staff hook).

```sh
# splits from https://tracker.vendetta.rocks/tracker/download/<versionCode>/<split>
python patcher/patch.py --out out base.apk config.arm64_v8a.apk config.xxhdpi.apk config.en.apk
adb uninstall com.discord            # the signature differs from the Play build
adb install-multiple out/*.apk
```

What it patches:

| target | how it is found | patch |
|---|---|---|
| `PhoenixLaunch.i` (dex) | the only method containing the string "Phoenix didn't stay up for 3 launches"; must return `Z` | `const/4 v0,1; return v0`, then the dex checksum and SHA-1 are fixed |
| access evaluator (`.so`) | the only function that references the experiment table whose first entry is `{"2026-11-phoenix-app-access", 26}` | `return Some(true)` |
| `is_staff` (`.so`) | the call made right after `dev_analytics_env()` in the Dev Tools settings builder (the function that references "Show Dev Tools Widget") | the body is replaced with a staff-override hook (see Staff below); the key, `read_state`, `drop` and field offsets are read from the original function |

Verified on 356207: arm64 (Android 16 phone, Phoenix reaches READY with no gate, Staff
Settings and DevTools work) and x86_64 (the finders return the same addresses that were
found by hand).

## Xposed module (runtime, x86_64 356207)

### Patches

| name   | offset      | function                                                                                  | patch                  |
|--------|-------------|-------------------------------------------------------------------------------------------|------------------------|
| access | `0x2eac1c4` | `ops_surfaces::client_access` evaluator for experiments `2026-11-phoenix-app-access` / `2026-11-phoenix-rollout` (with overrides). Returns `Option<bool>`, where 2 = unknown. | `mov al,1; ret` |
| staff  | `0x1f76640` | `is_staff(handle, force)`, 17 call sites through GOT slot `0x6bc5f30`. | jump to a C hook (see Staff below) |

## Staff

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

### Layout

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

### Build and install

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
