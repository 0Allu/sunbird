# AGENTS.md

Notes for coding agents working on this repo. Read README.md for what the project does; this file
is about how to work on it without repeating past mistakes.

## Rules from the owner

- **No self-attribution.** Never add `Co-Authored-By` (or any other agent attribution) to commits
  or PRs. Existing history was rewritten to remove them.
- **One signing key.** Every APK (site, `node-build`, `patch.py`) is signed with
  `web/signing-key.json`, so builds install over each other. Don't introduce other keys.
- **Push only what works.** Test on a device (WSA and/or the owner's phone) before pushing.
  Pushing `main` deploys GitHub Pages (`.github/workflows/pages.yml`, `web/` only). A force push
  may not trigger it: run `gh workflow run pages.yml --ref main`.
- **Privacy.** Screenshots of the owner's devices show private chats, emails, phone numbers.
  Don't quote that content, don't commit captures (`plugin/ui/mktpl.mjs` scrubs text from
  templates), delete screenshots after use.
- **Phone:** the owner's arm64 phone has no root. Don't enter passwords; install over the existing
  `com.discord.phoenix` (same key keeps the login). Clean up test plugins/files you add.

## Layout

| Path | What |
|---|---|
| `web/` | The browser patcher (GitHub Pages): `lib/arm64.js`, `lib/x86.js` (host lib patches), `dex.js`, `axml.js`, `arsc.js`, `zip.js`, `sign.js`, `apk.js` (pipeline), `plugins.js` (plugin options + config block), `frida.js`; `tools/node-build.mjs` is the same pipeline as a CLI; `worker/proxy.js` is the Cloudflare CORS proxy. |
| `web/plugins/` | Prebuilt artifacts served by the site: `libplg-{arm64,x86_64}.so`, `libshadowhook_nothing-arm64.so`, `message-logger.js`, frida gadget + bootstrap. `*.so` is gitignored except here. |
| `plugin/` | The plugin host, a Rust workspace building `libplg.so`: `loader` (cdylib, `.init_array` constructor), `sigs` (typed config block filled by the patcher), `hook` (gateway registry hooks, JNI bridge, scanner, ShadowHook, Java helper loader), `runtime` (rquickjs, JS API). |
| `plugin/runtime/src/prelude.js` | The JS side of the `phoenix` API. `settings.js` = the Settings → Phoenix pages. `patches.js` = UI patch targets (`ui.add*`, `commands`, `messages.onSend`). |
| `plugin/API.md`, `plugin/runtime/phoenix.d.ts` | Plugin API reference and types. Keep both in sync with prelude.js. |
| `plugin/ui/` | Phoenix UI-op format (`FORMAT.md`), codec (`uiops.js`), template generator (`mktpl.mjs`). |
| `plugin/hook/java/phx/` | Java helper classes (picker, toast/confirm, DB) compiled to `picker.dex` and embedded in libplg. |
| `plugin/hook/shadowhook/` | Vendored ShadowHook 2.0.1 (see its README.phoenix for local patches). |
| `plugin/rquickjs-sys/` | Trimmed rquickjs-sys with Android bindings (copied from linux-gnu). |
| `plugins/example/` | TypeScript plugin template (esbuild, keeps the header via `build.mjs`). |
| `patcher/`, `native/`, `java/` | Older Python patcher and Xposed-based tooling (x86 WSA test injection). |

## Building

- libplg: `TOOLCHAIN=+stable-x86_64-pc-windows-gnu plugin/build.sh` (Git Bash). Builds both ABIs
  into `web/plugins/` and recompiles `picker.dex` if `javac` exists.
  - No Visual Studio on the dev box: use the GNU host toolchain (`+stable-x86_64-pc-windows-gnu`).
  - The NDK `*.cmd` wrappers mangle rustc's quoted link args, so `clang.exe --target=...` is the linker.
- Patched APK locally: `node web/tools/node-build.mjs --package com.discord.phoenix --plugins jsPlugins,frida --bundled messageLogger [--js my.js] out.apk base.apk config.<abi>.apk config.<dpi>.apk config.<lang>.apk`
- Web dev server: `node web/tools/dev-server.mjs` (port 8787).
- Example plugin: `cd plugins/example && npm i && npm run build && npm run check`.

## Testing

- **WSA (x86_64)** at `adb -s 127.0.0.1:58526`. `com.discord` there is a test install with an
  Xposed module (`native/loader.c`, "GateProbe") that loads `/data/local/tmp/gatepayload.so`
  after start. For libplg tests push a shim as `gatepayload.so` that applies the gate/staff
  patches, copies `/data/local/tmp/libplg_test.so` + `libplg.js.so` (the bundle) into
  `files/` (SELinux won't map code from /data/local/tmp) and dlopens it. Fill the config block
  of the test libplg yourself (build ID, registry anchor) like `web/lib/plugins.js` does.
- **Phone (arm64)**: build with node-build and `adb install -r`; logs: `adb logcat -s PhoenixJS`.
- Open Settings: `adb shell am start -a android.intent.action.VIEW -d discord://app/settings <pkg>`
  (ignored while the app is still starting). Find rows by accessibility label with
  `uiautomator dump`, not by coordinates (layouts move).
- `adb shell input text` with parentheses needs `"input text '...'"` quoting; the phone keyboard
  inserts spaces after dots.

## Facts that cost time to learn

- **Threads:** Phoenix handles gateway events on its store thread. Store reads and dispatches
  must run there (`registry::request_store_thread` / `onStore` in prelude.js). UI ops must not be
  sent from the main thread (`hook::jni::apply_ops`, serialized with Rust's batches by `UI_LOCK`).
- **Bridge:** almost all UI-op batches arrive as `GuestHost.invoke` frames starting with 4 zero
  bytes, not through `applyOps`. ART's shared JNI table is patched once (`hook::jni::attach`).
- **Settings injection:** our UI node ids start at 0x50000000. Rust's `childEdits` count Java's
  child list, so the Phoenix row is spliced in by mirroring Rust's list and re-sending full
  `children` ops. The phone layout pushes Settings onto the app's root navigator; the tablet one
  has its own. Page headers copy the live status-bar inset from Rust's `stackHeader`.
- **UI patch targets (`patches.js`):** it mirrors Rust's tree (children + accessibilityIds) and
  anchors on language-neutral ids: `search.entry` (chat header), `composer.expressions` /
  `composer` / `send` (message box), `message.content:<id>` / `message.timestamp:<id>` (rows),
  `message.actions.root` (long-press sheet; its message comes from the list's `rowLongPress`
  index), `profile-tab-pages` (profile sheet; user id from the avatar URL). Layouts are cloned
  from the live neighbours, so themes carry over. Closing a sheet = a `dismiss` event to the
  `sheet` node. Sending = `submit` on `composer` or `press` on `send`; Rust keeps the text from
  `changeText`, so a rewrite is `changeText(new)` + the original event in one batch frame.
  Our ids start at 0x58000000. Text set from a Promise waits ~100 ms so Java has applied the batch.
- **Testing sends:** never post to real channels while testing; load a guard plugin (a
  `phoenix.bridge.onDispatch` listener returning `false` for `submit` and `send` presses) and check `/commands` with `ctx.reply`.
  `uiautomator dump` pops an accessibility toast over the bottom of the phone screen for a few
  seconds: wait before tapping there. Git Bash turns `/word` into a Windows path in
  `adb shell input text`: set `MSYS_NO_PATHCONV=1`.
- **ShadowHook:** its constructors are patched to priority 101 (they must run before libplg's),
  it needs `libshadowhook_nothing.so` next to libplg (shipped by the patcher), and compiler-rt
  builtins linked in (`__clear_cache`). arm64 only.
- **BigInt:** arm64 heap pointers carry tag bits above 2^53; native addresses/registers are BigInts.
- **Stores:** Phoenix persists its state in `files/store/<account>/cache.sqlite` (Java module
  `nativeapp.modules.w4`): key/value tables (`k`, `v` JSON) like guilds, members, channels,
  users, messages, read_states, user_guild_settings, relationships, and a `meta` table (`me`,
  `guild_order`, `domain.*`, experiments, ...). Keys are 20-digit zero-padded ids. The runtime
  reads it read-only through the Java helper (`phx.Db`). READY does not go through the hooked
  event table on the phone build, so don't rely on observing it. `files/kv-storage/` is the
  old React Native app's storage.
- **Credentials on the bridge:** HTTP requests travel over `GuestHost.invoke` with the user's
  `authorization` header. Never capture/log invoke frames wholesale; filter before saving and
  delete captures that contain tokens.
- **Plugins dir:** JS plugins live in `files/phoenix-js/`; Frida plugins in `files/phoenix-plugins/`.
- **esbuild drops line comments**: keep plugin headers with a banner (see plugins/example/build.mjs).
- **Shell gotchas (Windows Git Bash):** long heredocs sometimes fail to parse; write scripts to a
  file instead. Set `MSYS_NO_PATHCONV=1` for adb paths, pass Windows paths (`cygpath -w`) to
  native tools. Python on this box prints with cp1254: write UTF-8 to files instead.
