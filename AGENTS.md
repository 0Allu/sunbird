# AGENTS.md

Notes for coding agents working on Phoenix Unlock (this repo): a patcher and plugin runtime for
Phoenix, Discord's native Rust Android client (356.7 Alpha, versionCode 356207). "Phoenix" is
Discord's client; our tool is "Phoenix Unlock" (the code calls its runtime `phoenix`).

## Rules from the owner

- **Attribution.** Agents may credit themselves in commit messages, e.g. with an
  `Assisted-By: <agent>` trailer.
- **API in Rust.** Plugin API features and their machinery (UI ops, patch targets, settings pages,
  stores) live in the Rust crates and reach plugins through the `phoenix` object. Plugins are
  TypeScript; keep JS out of the runtime itself.
- **One signing identity.** Every APK, from the patcher, the site or `node-build`, is signed with
  `crates/patcher/keys/` (`web/signing-key.json` holds the same key for the site): v3 only, with
  the proof of rotation from the old Valenced test key, so builds install over each other and over
  older installs. The old private key must not be added to the repo. Read `keys/README.md`.
- **Push only what works.** Test on a device before pushing.
- **Privacy.** Screenshots and logs from the owner's phone show private chats, emails and
  accounts. Don't quote them, don't commit captures, delete screenshots after use. Some plugins
  (`example`) log message text to the console.
- **Phone.** The owner's arm64 phone has no root. Don't enter passwords. Install over the existing
  `com.discord` (same key keeps the login). Don't tap around while the owner is using it; ask.
  Remove test plugins and files you add.

## Layout

| Path | What |
|---|---|
| `crates/loader` | `libphoenix.so`, injected as `libplm.so`: a constructor that starts the runtime inside the host. |
| `crates/runtime` | rquickjs plugin runtime and the `phoenix` API: gateway hooks, UI bridge, Settings → Phoenix pages (`settings.rs`), UI patch targets (`patches/`), console, file picker (`picker.rs` + `java/phx/Picker.java` → `java/picker.dex`, rebuild with `java/build.sh`). `phoenix.d.ts` is the plugin API reference. |
| `crates/hook` | ShadowHook (arm) / Dobby (elsewhere) inline hooks, bytehook PLT hooks, image scanner. |
| `crates/sigs`, `crates/sigscan` | Hook-point rules and the per-build table; `sigscan` evaluates them against a library file. |
| `crates/ui`, `crates/wire` | Codecs for Phoenix's UI-op batches and bridge values/frames. `ui/src/props.in` is Phoenix's prop table, `templates.json` captured layouts. |
| `crates/patcher` | CLI that patches the split APKs (dex launcher, host access check, `liblog.so` → `libplm.so`, adds the loader, shadowhook probe and plugin bundle) and signs them. |
| `crates/stand-in*` | A fake Phoenix gateway library and an end-to-end demo, to test hooks without Discord. |
| `plugins/*/src/index.ts` | Bundled plugins (TypeScript; `// @` header lines are kept as the bundle banner). |
| `web/` | The browser patcher (GitHub Pages, `.github/workflows/pages.yml`), a JS port of `crates/patcher` that must stay byte-identical to it: `lib/apk.js`, `host.js`, `dex.js`, `sign.js`, `zip.js`, `axml.js`, `arsc.js`, `frida.js`. `tools/node-build.mjs` is the same pipeline as a CLI, `tools/bundle-plugins.mjs` builds `web/plugins/*.js` + `index.json` (build output, not committed; CI runs it). `web/plugins/libphoenix-arm64.so` is copied from a loader build. |
| `frida/` | The optional Frida gadget bootstrap (`build.sh` → `web/plugins/frida-bootstrap.js`). |
| `flake.nix`, `nix/`, `scripts/*.nu` | The Linux devshell and build/install scripts (`scripts/phoenix.nu` patches and installs on a phone). |

## Building

- **Linux:** `nix develop`, then `nu scripts/phoenix.nu base.apk config.arm64_v8a.apk config.<dpi>.apk config.<lang>.apk`.
  The devshell sets `SHADOWHOOK_SRC` / `BYTEHOOK_SRC` to the commits pinned in `flake.lock`.
- **Windows (no nix):**
  - Rust `stable-x86_64-pc-windows-gnu` with the `aarch64-linux-android` target, `cargo install cargo-ndk`, an NDK.
  - Fetch bytedance `android-inline-hook` and `bhook` at the `flake.lock` revisions and point
    `SHADOWHOOK_SRC` (`…/shadowhook/src/main/cpp`) and `BYTEHOOK_SRC` (`…/bytehook/src/main/cpp`) at them.
  - `LIBCLANG_PATH` to an LLVM `bin` with `libclang.dll`, and set
    `BINDGEN_EXTRA_CLANG_ARGS_aarch64-linux-android="--target=aarch64-linux-android28 --sysroot=<ndk>/toolchains/llvm/prebuilt/windows-x86_64/sysroot -isystem <ndk>/…/lib/clang/<ver>/include"`
    (a hyphenated name: set it with `env`).
  - Git for Windows' MinGW (gcc 12) can't link the host build scripts. Take it off `PATH` so rustc
    links with its bundled MinGW, and put a dir with only `dlltool.exe` (from the toolchain's
    `self-contained`) and `as.exe` (plus its DLLs) on `PATH`.
  - `cargo +stable-x86_64-pc-windows-gnu ndk -t arm64-v8a -P 28 --link-builtins build --release -p phoenix-loader`,
    and `cargo build --release -p patcher` for the host patcher. The patcher calls `apksigner.bat` on Windows.
  - The C: drive is nearly full: use `CARGO_TARGET_DIR` on E:.
- **Site:** `node web/tools/bundle-plugins.mjs` (`ESBUILD=` path to esbuild), then
  `node web/tools/dev-server.mjs <dir with the splits> [port]`. The deploy workflow runs on pushes to `main`.

## Testing

- Phone (arm64): patch with the patcher or `node web/tools/node-build.mjs`, `adb install-multiple -r`
  (splits) or `adb install -r` (merged), then `adb logcat -s phoenix` (`-s FridaPlugins` for Frida).
- Open Settings: `adb shell am start -a android.intent.action.VIEW -d discord://app/settings com.discord`.
  Find rows with `uiautomator dump` by resource-id (= accessibility id, ours are `phoenix-<title>`);
  the dump shows an accessibility toast over the bottom of the screen for a few seconds.
- Compare the site against the Rust patcher by unzipping both outputs: every entry but the
  signature must be identical.
- Git Bash turns `/word` into a Windows path in adb arguments: set `MSYS_NO_PATHCONV=1`, and pass
  local paths with `cygpath -w`.

## Facts that cost time to learn

- **Hooks:** gateway events are hooked by swapping handler pointers in Phoenix's event registry
  (found from the `MESSAGE_CREATE` name, so it survives updates); the UI bridge swaps two entries
  of ART's JNI table. ShadowHook and bytehook are linked in but not used yet.
- **Update risks:** `crates/ui/src/props.in` (UI ops name props by index, so a new prop shifts
  them), `GuestHost.d(byte[], Function0)` (an obfuscated name), and `templates.json` (captured
  look). Everything else is found by names, strings and patterns, not addresses.
- **UI text:** text and inputs take `spans` / `valueSpans`: arrays of `{text, color, weight,
  italic, bg, …}` pieces that concatenate to the text. Set an input's text with
  `forceValue {seq, text, start, end}` (applied when `seq` changes), not `value`, which is
  ignored while the keyboard has edits in flight.
- **Threads:** UI ops must not be sent from the main thread (Rust may hold the UI lock while it
  waits on Java); the settings worker sends them.
- **Stores:** Phoenix keeps its state in `files/store/<account>/cache.sqlite`.
- **Credentials on the bridge:** HTTP requests travel over `GuestHost.invoke` with the user's
  `authorization` header. Never capture or log invoke frames wholesale.
- **Shell (Windows):** long heredocs in Git Bash sometimes fail to parse; write scripts to files.
  Python prints with cp1254 here: write UTF-8 to files. Bash's `printf` writes `é` as one
  Latin-1 byte, not UTF-8.
