# Phoenix plugin API v1

Plugins are plain JavaScript run by libplg's runtime (rquickjs / QuickJS-ng) inside Discord's
Phoenix client. Each one gets `phoenix` and a tagged `console` in scope. Types:
[`runtime/phoenix.d.ts`](runtime/phoenix.d.ts). A TypeScript starter (esbuild):
[`../plugins/example`](../plugins/example).

Install a plugin from the patcher page (JS plugins → your files), or in the app: Settings →
Phoenix → Install plugin (a URL or a .js file). Each plugin gets a page there with its switch,
settings, Edit code, Reload and Remove. Its console output goes to Settings → Phoenix → Console
and to logcat (`adb logcat -s PhoenixJS`).

## Header

Leading comment lines, read before the plugin runs:

```js
// @name Example
// @description What it does (shown in Settings)
// @version 1.0.0
// @author you
// @api 1
```

`@api` is the plugin API version it needs; an app with an older API shows the plugin as failed
instead of running it. The older one-line form `// Name: what it does` still works.

## Threads

Phoenix handles gateway events on its **store thread**. `phoenix.on` listeners run there, before
Phoenix's own handler, and can change or drop the event. Everything else (`observe`, timers,
fetch callbacks, promise jobs) runs on the plugin JS thread. `dispatch` and `stores.*` need the
store thread: inside an `on` listener they run immediately, anywhere else they wait for the next
gateway event (usually well under a second). Both always return Promises.

## Gateway

| | |
|---|---|
| `on(event, ev => …)` | Sync listener. Return `false` to drop the event, an object or string to replace its JSON, nothing to pass it on. `ev.data` is parsed lazily from `ev.raw`. Returns an unsubscribe function. |
| `observe(event, ev => …)` | Async copy on the JS thread; never delays Phoenix. |
| `dispatch(event, data)` | Runs an event through Phoenix's own handler (store thread). Resolves to whether Phoenix accepted it. |
| `events` | Every event name this build handles. |
| `ready` | Resolves once Phoenix has handled READY. |

## App

| | |
|---|---|
| `messages.addLocal(channelId, content, {author?, guildId?, embeds?})` | Shows a message only on this device (from "Phoenix" or `author`). Resolves to its id. |
| `messages.updateLocal(message)` / `deleteLocal(channelId, id)` | Local-only edit / removal. Nothing is sent to Discord. |
| `stores.me()` | The signed-in user. |
| `stores.read(s => …)` | Several store reads at once, synchronously on the store thread. |
| `stores.raw(ser, off)` | Any of Phoenix's serde serializers on a store field (per-build offsets, advanced). |
| `ui.toast(text, {long?})` | Android toast. |
| `ui.confirm(title, message, {ok?, cancel?})` | Dialog; resolves to `true` for OK. |
| `ui.pickFile(mime?)` | Android's document picker; resolves to `{name, text}` or `null` (4 MB max). |
| `fetch(url, {method, headers, body})` | HTTP through Android's HttpURLConnection; `{status, ok, text(), json()}`. |

`ui.*` calls made before Phoenix's first screen is up wait for it.

## Plugin

| | |
|---|---|
| `settings.define({key: {type, title, description?, default?}})` | Settings on the plugin's page. Types: `switch`, `text`, `number`, `button` (with `onPress`). Call it each time the plugin starts. |
| `settings.get(key)` / `set(key, value)` / `onChange((key, value) => …)` | Values persist per plugin. |
| `storage.get(key, fallback)` / `set` / `delete` | Free-form per-plugin JSON storage. |
| `onUnload(fn)` | Runs when the plugin is turned off, reloaded or removed. Listeners, bridge hooks and intercepts registered through `phoenix` are removed automatically; timers and anything else are yours to undo. |
| `exports` / `plugins.get(id)` / `plugins.list()` | Share functions between plugins. |
| `version` | `{api, runtime, arch, inlineHooks}`. |

## Low level

| | |
|---|---|
| `native.sym(name, lib?)`, `moduleBase(lib)`, `base`, `at(offset)`, `scan(pattern)` | Symbols, module bases, IDA-style byte patterns over Phoenix's code. |
| `native.read(addr, len)` / `write(addr, bytes)` / `readU64` / `readCString` | Memory through the kernel: a bad address throws instead of crashing. |
| `native.call(addr, ...args)` | C call, up to 8 integer/pointer args, integer result. |
| `native.intercept(addr, ctx => …, {instruction?})` | ShadowHook inline hook (arm64): `ctx.x[0..30]` (BigInts), change them to change arguments; with `{instruction: true}` hook one instruction, e.g. a `ret` to see `x0`. |
| `bridge.onOps / onInvoke / onInvokeResult / onDispatch` | Phoenix's Rust ↔ Java bridge: UI-op batches (format: [`ui/FORMAT.md`](ui/FORMAT.md)), guest → host calls, host → guest events. |

Addresses and registers are BigInts: arm64 heap pointers carry tag bits above 2^53.

## Errors

Uncaught errors and unhandled promise rejections are logged with their stack to the Console.
A plugin that throws while starting is shown as failed on its page; the others keep running.
