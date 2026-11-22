# Phoenix plugin API v2

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
// @api 2
```

`@api` is the plugin API version it needs; an app with an older API shows the plugin as failed
instead of running it. v2 added the UI patch targets, `commands` and `messages.onSend`. The older one-line form `// Name: what it does` still works.

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
| `stores.guilds()` / `guild(id)` / `members(guildId)` / `member(g, u)` / `guildSettings(g)` | Guilds (`properties`, `roles`, `emojis`, …), members Phoenix has seen, your notification settings. |
| `stores.channels(guildId)` / `privateChannels()` / `channel(id)` | Channels (guild 0 = DMs). |
| `stores.messages(channelId, {limit, before})` / `message(ch, id)` | Cached messages, newest first. |
| `stores.user(id)` / `users()` / `relationships()` / `readState(channelId)` | |
| `stores.meta(key)` / `domain(name)` | Phoenix's meta table (`guild_order`, `experiments`, `my_members`, …) and persisted domains (`profiles`, `threads`, `scheduled_events`, `collectibles`, `connections`, `user_affinities`, …). |
| `stores.tables()` / `get(t, key)` / `scan(t, {prefix, after, before, reverse, limit})` / `count(t)` / `sql(query, args)` | Every table, generically, and read-only SQL. |
| `stores.read(s => …)` | Several live reads at once, synchronously on the store thread. |
| `stores.raw(ser, off)` | Any of Phoenix's serde serializers on a live store field (per-build offsets, advanced). |

The stores are Phoenix's own database (`files/store/<account>/cache.sqlite`), opened read-only:
key/value tables with JSON values, written by Phoenix as it handles events. Ids in keys are
zero-padded to 20 digits (`"<guild>:<channel>"`, `"<channel>:<message>"`, `"<guild>:<user>"`).
Change notifications: listen to the matching gateway events (`on`/`observe`).
| `ui.toast(text, {long?})` | Android toast. |
| `ui.confirm(title, message, {ok?, cancel?})` | Dialog; resolves to `true` for OK. |
| `ui.pickFile(mime?)` | Android's document picker; resolves to `{name, text}` or `null` (4 MB max). |
| `fetch(url, {method, headers, body})` | HTTP through Android's HttpURLConnection; `{status, ok, text(), json()}`. |

`ui.*` calls made before Phoenix's first screen is up wait for it.

## UI patch targets

Plugin UI goes next to Phoenix's own, styled like its neighbours. Each `add*` returns a remover
with `.update()` (re-render, e.g. after your data changed); everything is removed when the plugin
is turned off. Icons are Phoenix's drawables, e.g. `res://ic_mana_puzzle_piece_plus`,
`res://ic_mana_id`, `res://ic_mana_angle_brackets`. Handlers get a `ctx` with `channelId` and
`reply(text)` (a local "only you can see this" message from Phoenix).

| | |
|---|---|
| `ui.addHeaderButton({icon, label, onPress(ctx)})` | Icon button in the chat header, left of search. |
| `ui.addComposerButton({icon, label, onPress(ctx)})` | Icon button in the message box, left of the emoji button. |
| `ui.addMessageAccessory({render(ctx), placement?})` | A line of text under each message's content (`placement: 'header'`: after its timestamp). `ctx = {messageId, channelId}`; return a string, `{text, color}`, `null` for nothing, or a Promise of those. Runs for every message row, so keep it fast. |
| `ui.addMessageAction({label, icon?, danger?, visible?(ctx), onPress(ctx)})` | A row in the message long-press menu, in a group above Delete. `ctx = {messageId, channelId, message}`; the menu closes on press. |
| `ui.addProfileSection({title, render(ctx)})` | A section at the end of a profile's Main tab. `ctx = {userId}` (from the avatar; null for default avatars); `render` returns text or a Promise of it. |
| `commands.register({name, description?, icon?, execute(args, ctx)})` | Runs when you send `/name args` from the message box. Return text to send that instead; return nothing (or a Promise) and nothing is sent. Matching commands are listed first in Discord's `/` suggestions while it shows them; picking one fills in `/name `. `commands.list()` shows them all. |
| `messages.onSend((text, ctx) => …)` | Before a message from the message box is sent: return a string to change it, `false` to cancel. Synchronous. |

Not there yet: profile tabs, bottom tabs, custom pages outside Settings, a `/` list of our own when
none of Discord's commands match what you typed.

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
