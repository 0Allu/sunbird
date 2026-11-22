# Phoenix UI-op wire format (Android versionCode 356207)

Recovered from `classes.dex` bytecode (R8 names) and checked against three captured
batches. `uiops.js` implements everything below. `node dump.mjs <file.hex>` decodes all three
captures, uses all of every buffer, and re-encodes each one to identical bytes.

Labels: **[V]** = checked against captured bytes. **[B]** = read from bytecode only, with no capture that exercises it. **[I]** = inferred.

## Path

Rust calls `GuestHost.applyOps([BI)V`. That looks up `"UIManager"` and calls `modules.gq.d([BI)V`, which reads a
little-endian `ByteBuffer`:
- `yq` reads values and props.
- `xq`/`rq` hold the op list.
- `q3` caches each parent's child-id list for `childEdits`.
- `gq.c` applies each op to real Views.
- `gq.c` (field) is a `HashMap<Integer, View>` keyed by node id.

All integers are little-endian. Every int below is a signed i32 unless marked otherwise.

## Batch

```
u8 version            must be 1, else IllegalArgumentException("view wire version N")   [V]
op*                   ops run until the end of the buffer; there is no count             [V]
```

## Ops (byte = index into `rq.f` name table)

| byte | name | payload | notes |
|---|---|---|---|
| 1 | create | `i32 id, u16 len, utf8 type[len], props` | **[V]** The type is usually one of the 29 names in `yq.a` (view text stack image list scroll hscroll flow navigator input sheet modal banner popover systemOverlay player lottie rive video webframe zoom ring spinner datePicker markup slider vector_anim swipeFill canvas). Java checks those names first and falls back to a plain string otherwise. |
| 2 | update | `i32 id, props` | **[V]** Merges props into an existing node. |
| 3 | children | `i32 parent, i32 n, i32 ids[n]` | **[V]** Replaces the whole child list and updates the `q3` cache. |
| 4 | delete | `i32 n, i32 ids[n]` | **[V]** Removes each id from the map. It does not recurse: Rust lists every id it deletes **[B]**. |
| 5 | root | `i32 id` | **[B]** |
| 6 | focus | `u8 has, [i32 id if has≠0]` | **[B]** `has=0` clears focus. |
| 7 | scrollTo | `i32 id, u8 tag, value` | **[B]** The value is normally an `obj`. |
| 8 | requestFrame | (none) | **[B]** Sets a flag that posts a frame callback. |
| 9 | motion | `i32 n, i32 words[n]` | **[V]** The words are opaque (animation/worklet program). |
| 10 | childEdits | `i32 parent, i32 n, n × {i32 keep, i32 del, i32 m, i32 ins[m]}` | **[V]** Edits the cached child list with a cursor (see below). Java turns it into a `children` op. |

How `childEdits` runs (from `gq.d` 0x52): start with `old = q3[parent]` (empty if none) and `cur = 0`. For each edit:
1. Copy `old[cur .. cur+keep]` to the new list.
2. Skip `del` entries: `cur += keep + del`.
3. Append `ins`.

After the last edit, copy the rest of `old` (from `cur` to the end). If `keep`/`del` run past `old`, Java throws "childEdits past view …".

## Props block (`yq.b`)

```
u16 count
count × { u16 key, u8 tag, value }
```
`key` is an index into `lh/c2.a`, a 855-entry name table (`PhxUI.PROPS`). The same table names events. Java drops keys ≥ 855.

## Value tags (`yq.f`; writer `debugmeta.c.Q`)

| tag | name in uiops.js | encoding |
|---|---|---|
| 0 | null | none (in a props block it probably resets the prop) **[B]/[I]** |
| 1 | false | none **[V]** |
| 2 | true | none **[V]** |
| 3 | i32 | i32. Colours with no alpha bit set come out as i32 (e.g. `background=0x1497979f`) **[V]** |
| 4 | i64 | i64. Colours with alpha 0xFF need i64 (e.g. `tint=0xfffbfbfb`) **[V]** |
| 5 | f64 | f64. Used for all dimensions and numbers (width=24, fontSize=16) **[V]** |
| 6 | str | `i32 len, utf8[len]` **[V]** |
| 7 | arr | `i32 n, n × {u8 tag, value}` **[V]** |
| 8 | obj | `i32 n, n × {u16 key, u8 tag, value}` (keys from the prop table) **[V]** (1 in batch 2) |
| 9 | rect | 4 × f32. Used for padding/margin `[l,t,r,b]`; order inferred from `[56,0,0,0]` = left inset **[V]/[I]** |
| 10 | map | `i32 n, n × {i32 len, utf8 key, u8 tag, value}` **[B]** |
| 11 | words | `i32 n, i32[n]`. Also appended to the batch's list table **[V]** |
| 12 | wpatch | `i32 listIdx, u16 n, n × {u16 idx, i32 word}`: copies list `listIdx` from this batch's table, patches it, and appends the result **[V]** (6 in batch 2) |

Tags 11 and 12 point into a per-batch list table, numbered in decode order. Never move a `wpatch`
value into another batch.

## Event subscription and press frames

A node subscribes to events with the prop `on` (key 320 = 0x140): an `arr` of `str` event names, e.g.
`on=["press"]` **[V]**. When the user taps it, Java follows this path **[B]**:
1. `lh.c0` calls `qo.h(...,"press", payload=new j3())`.
2. That calls `qo.f`, which calls `yq.d(nodeId, "press", {})`.
3. Then `GuestHost.d(bytes)` runs, then `WasmRuntime.nativeDispatch(handle, frame)`.

A single dispatch frame:
```
00 00 00 00          4-byte prefix; GuestHost.d allocates len+4 and copies at offset 4, so these stay zero [B]
01 01                yq.d writes two literal 1 bytes (version, kind = view event) [B]
i32  nodeId          id of the node that carries on=[...] (e.g. the row view, not its text child) [B]
u16  eventKey        index of the event name in the prop table: press=614 (0x266), longPress=642, rowPress=620, navigate=604, changeText=576 [B]
u8 tag + value       payload; for press it is an empty obj: 08 00 00 00 00 [B]
```
Example: a press on node 0x50000000 is `00000000 0101 00000050 6602 08 00000000` (17 bytes).

Frames can also arrive batched. `GuestHost` calls `f7.t.d` when it flushes several at once **[B]**:
```
04 00  u16 0  u16 0  u16 0  u32 0  u32 hdrLen(=0)  hdr[hdrLen]
then { u32 len, frame[len] }*        each frame keeps its own 4-byte prefix
```
Module frames from `GuestHost.c` use a 16-byte header: `03 00 u16 0 u16 eventId u16 0 u32 0 u32 hdrLen`.
`PhxUI.decodeDispatch(u8)` handles both forms. It returns one entry per frame, with `off`/`len`
so you can splice frames out. To swallow a press, drop the whole call if it is a single frame. For a kind-4 batch,
rebuild it without that `{len, frame}` entry. None of this has been checked against a captured
outgoing frame yet. Capture one with the dispatch hook to confirm.

## Node ids

- Rust gives out ids from a single counter that only goes up, starting low **[V]/[I]**. Batch 1 uses 0xd8–0xda and deletes 0x329. Batch 2 creates 0x32a–0x4cd. Batch 3 creates 0x4ce–0x4d5, the very next numbers. Id 7 is an existing root/overlay host.
- Java keys nodes only by `HashMap<Integer,View>`. It does not index arrays by id, and no `View.setId(nodeId)` call was found **[B]**.
- **Safe range for injected nodes: 0x50000000–0x5FFFFFFF.** These are positive i32s, far above Rust's counter, and stay clear of 0x7Fxxxxxx (Android resource-id space) and of negative values. Words such as `516..523` inside `motion`/`worklet` look like a separate value-id space **[I]**.
- Java does not recurse on `delete`, so Rust will never delete your nodes. When Rust deletes their parent or ancestor (e.g. the settings modal closes), send your own `delete` with all injected ids. Otherwise the Views leak in `gq.c`.

## The "Cache Actions" row (batch 3, captured)

```
#4ce <view>  direction="column" width="fill" background=i32 0x1497979f align="stretch"
             role="button" accessibilityLabel="Cache Actions" accessibilityId="Cache Actions"
             activeBackground=i32 0x12ffffff on=["press"]               ← press target
  #4cf <view> direction="row" padding=rect[12,12,12,12] width="fill" minHeight=56 align="center"
    #4d0 <image> uri="res://ic_mana_file_warning" margin=rect[4,4,16,4] width=24 height=24 tint=i64 0xfffbfbfb scale="fit"
    #4d1 <text>  text="Cache Actions" color=i64 0xfffbfbfb fontSize=16 fontWeight=500 lineHeight=20
                 lineHeightExact=true includeFontPadding=true flex=1
    #4d2 <view>  direction="row" margin=rect[8,0,8,0] width=12 height=24 align="center" justify="center" clip=true
      #4d3 <image> uri="res://ic_mana_chevron_small_right" width=12 height=24 tint=i64 0xffabacb2
#4d4 <view> padding=rect[56,0,0,0] margin=rect[0,-1,0,0] width="fill" height=1 align="stretch"   ← divider
  #4d5 <view> width="fill" height=1 background=i32 0x1f97979f align="stretch"
childEdits #439 keep 8 del 0 ins [#4ce, #4d4]
```
The batch sends the creates first, then the `children` ops bottom-up (#4d2→[#4d3], #4cf→[#4d0,#4d1,#4d2], #4d4→[#4d5],
#4ce→[#4cf]), then the `childEdits`. `#439` is the "Developer Settings" group (`radius=12 clip=true`). Its 9 children
alternate row/divider. The new row goes after the 8th child, so it lands just before the last row, "Upload i18n data".
Other settings rows look the same. The first row of a group adds `radiusTopLeft/TopRight=12` and the last adds
`radiusBottomLeft/Right=12`. The group's own clip already rounds the corners, so these only shape the press highlight.

## Injecting "Phoenix Plugins"

1. Clone the 6 creates and 4 `children` ops of the row (#4ce–#4d3). Remap every id into 0x50000000+. Change the
   `text`, `accessibilityLabel` and `accessibilityId` strings, and optionally the icon `uri`. Keep `on=["press"]`.
2. Attach the row **at the end** of an existing parent's child list. Then Rust's own `childEdits`, which use indexes from
   Rust's shorter list, never reach it, and Java's tail copy keeps it last. When Rust sends a full `children` op for that
   parent, append your id again. Either send `childEdits {parent, [{keep: <current Java length>, del: 0, ins: [row]}]}`
   in a separate `applyOps` batch, or rewrite Rust's `children` op for the parent in flight. `keep` must equal the
   length of Java's cached list for that parent, or Java throws.
3. In the `nativeDispatch` hook, `decodeDispatch(frame)`, then swallow any entry with `kind==='view' && event==='press' && node` in your range.
4. When the parent goes away, send `delete` for your ids.

A test of the clone-and-encode path (in Node) re-encoded identically: 10 ops, 655 bytes. It has not been tested on a device.

## Uncertainties
- Not verified against captured outgoing frames: the press frame layout, the meaning of its zero prefix, and the empty-obj payload. All come from bytecode.
- Ops 5, 6, 7 and 8, and value tags 0 and 10, do not appear in the captures. Their decoding follows the bytecode exactly. The meanings of ops 5–9 come from the `rq.f` names only.
- The idea that ids come from a counter that only goes up is based on three captures. The 0x50000000 base is a choice with a wide margin, not a documented limit.
