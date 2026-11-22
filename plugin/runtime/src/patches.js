'use strict';
// UI patch targets for plugins: header and composer buttons, message accessories, message actions
// (long-press menu), profile sections, slash commands and send interceptors.
// Phoenix's screens are UI-op trees built by Rust (plugin/ui/FORMAT.md). We mirror their structure
// (children, accessibilityIds) and splice our nodes next to stable accessibilityIds, cloning the
// look of Phoenix's own neighbours, the way settings.js adds its row. Our ids start at 0x58000000
// (settings.js counts up from 0x50000000).
(() => {
  const UI = PhxUI, FIRST_ID = 0x58000000;
  let nextId = FIRST_ID;
  const str = (v) => ({ t: 'str', v: String(v) });
  const col = (v) => ({ t: 'i64', v: String(v) });
  // props hold BigInts (i64), so no JSON round trip
  const clone = (x) => (x && typeof x === 'object' ? (Array.isArray(x) ? x.map(clone) : Object.fromEntries(Object.entries(x).map(([k, v]) => [k, clone(v)]))) : x);
  const prop = (props, k) => props.find((p) => p.k === k)?.v;
  const setProp = (props, k, v) => {
    const i = props.findIndex((p) => p.k === k);
    if (v === undefined) { if (i >= 0) props.splice(i, 1); } else if (i >= 0) props[i] = { k, v }; else props.push({ k, v });
  };
  const pad = (id) => String(id).padStart(20, '0');
  const all = (n) => [n.id, ...n.kids.flatMap(all)];
  const warned = new Set();
  const warnOnce = (k, ...a) => { if (!warned.has(k)) { warned.add(k); console.warn(...a); } };

  // ---- mirror of Rust's tree ----------------------------------------------------------------
  const kids = new Map(), parent = new Map(), aidOf = new Map(), sheets = new Set();
  const applyEdits = (old, edits) => {
    const out = [];
    let i = 0;
    for (const e of edits) { out.push(...old.slice(i, i + e.keep)); i += e.keep + e.del; out.push(...e.ins); }
    return out.concat(old.slice(i));
  };
  // first descendant (depth-first) whose accessibilityId matches
  const findDown = (id, re, depth = 12) => {
    const a = aidOf.get(id);
    if (a && re.test(a)) return id;
    if (!depth) return undefined;
    for (const c of kids.get(id) || []) { const r = findDown(c, re, depth - 1); if (r !== undefined) return r; }
  };

  // ---- targets: where contributions go, found from an anchor node -----------------------------
  // place(anchorId, aid) -> {parent, before, ctx} | null; our nodes go into `parent`, before `before` (or last)
  const MSG = /^message\.(?:content|avatar|author|timestamp):(\d+)$/;
  let lastLongPress = null, profileUser = null;
  const TARGETS = {
    header: { match: (a) => a === 'search.entry', place: (id) => ({ parent: parent.get(id), before: id, ctx: {} }) },
    composer: { match: (a) => a === 'composer.expressions', place: (id) => ({ parent: parent.get(id), before: id, ctx: {} }) },
    content: { match: (a) => a.startsWith('message.content:'), place: (id, a) => ({ parent: id, before: null, ctx: { messageId: a.slice(16) } }) },
    mheader: { match: (a) => a.startsWith('message.timestamp:'), place: (id, a) => ({ parent: parent.get(id), before: null, ctx: { messageId: a.slice(18) } }) },
    action: {
      match: (a) => a === 'message.actions.root',
      place: (id) => {
        const groups = (kids.get(id) || []).at(-1);
        if (groups === undefined) return null;
        const del = (kids.get(groups) || []).find((g) => findDown(g, /^message\.actions\.delete$/, 2) !== undefined);
        return { parent: groups, before: del ?? null, ctx: { messageId: lastLongPress } };
      },
    },
    profile: {
      match: (a) => a === 'profile-tab-pages',
      // pages row > Main page > scroll > content > padding > sections
      place: (id) => { let x = id; for (let i = 0; i < 6 && x !== undefined; i++) x = (kids.get(x) || [])[0]; return x === undefined ? null : { parent: x, before: null, ctx: { userId: profileUser } }; },
    },
  };
  const anchors = Object.fromEntries(Object.keys(TARGETS).map((t) => [t, new Set()]));
  const contrib = Object.fromEntries(Object.keys(TARGETS).map((t) => [t, []]));

  // ---- templates: Phoenix's own nodes, snapshotted when they appear ---------------------------
  const tpl = {};
  const snap = (id, creates) => {
    const o = creates.get(id);
    return o && { type: o.type, props: clone(o.props), kids: (kids.get(id) || []).map((c) => snap(c, creates)).filter(Boolean) };
  };
  const ROW_TPL = /^message\.actions\.(reply|copy_text|copy_id|mark_unread|copy_link|mention)$/;
  const snapshot = (id, a, creates) => {
    if (a === 'search.entry') tpl.header = snap(id, creates);
    else if (a === 'composer.expressions') tpl.composer = snap(id, creates);
    else if (a.startsWith('message.timestamp:') && !tpl.ts) tpl.ts = snap(id, creates);
    else if (ROW_TPL.test(a)) {
      const row = snap(id, creates), g = parent.get(id), sib = kids.get(g) || [], next = sib[sib.indexOf(id) + 1];
      const div = next !== undefined && !aidOf.has(next) ? snap(next, creates) : null;
      if (row && row.kids[0]?.kids.length >= 2) Object.assign(tpl, { row, group: creates.get(g) ? { type: creates.get(g).type, props: clone(creates.get(g).props) } : tpl.group, divider: div || tpl.divider });
    } else if (a === 'profile-tab-pages') {
      const sec = TARGETS.profile.place(id)?.parent, first = sec !== undefined && (kids.get((kids.get(sec) || [])[0]) || [])[0];
      const s = first !== undefined && snap(first, creates);
      if (s && s.kids[0]?.type === 'text' && s.kids[1]?.kids[0]?.type === 'text') tpl.profile = s;
    }
  };

  // ---- building our nodes ----------------------------------------------------------------------
  const handlers = new Map(); // our node id -> {press}
  // Creates a tree (children first); `h` handlers are inherited by the subtree.
  const build = (root, out) => {
    const walk = (n, h) => {
      h = n.h || h;
      const ids = n.kids.map((k) => walk(k, h));
      n.id = nextId++;
      if (h) handlers.set(n.id, h);
      out.push({ op: 'create', id: n.id, type: n.type, props: n.props });
      if (ids.length) out.push({ op: 'children', id: n.id, ids });
      return n.id;
    };
    walk(root, null);
  };
  const call = (c, what, fn) => { try { return fn(); } catch (e) { console.error(`[${c.plugin}] ${what}:`, e, e && e.stack); } };
  const text = (props, value, color) => {
    const t = { type: 'text', props: clone(props), kids: [] };
    setProp(t.props, 'spans');
    setProp(t.props, 'text', str(value));
    if (color != null) setProp(t.props, 'color', col(color));
    return t;
  };
  const textOf = (r) => (r == null || r === false ? null : typeof r === 'object' ? r : { text: String(r) });
  // async results (Promises) fill a text node later
  const later = [];
  const resolveText = (c, node, r, what) => {
    if (r && typeof r.then === 'function') {
      later.push([node, r.then((v) => textOf(v), (e) => { console.error(`[${c.plugin}] ${what}:`, e); return null; })]);
      return true;
    }
    return false;
  };
  const iconButton = (t, c, ctx) => {
    const b = clone(t);
    setProp(b.props, 'accessibilityId', str(`phoenix.${c.plugin}.${c.id}`));
    setProp(b.props, 'accessibilityLabel', str(c.label || c.id));
    if (b.kids[0] && c.icon) setProp(b.kids[0].props, 'uri', str(c.icon));
    b.h = { press: () => withChannel(ctx).then((x) => call(c, 'onPress', () => c.onPress(x))) };
    return b;
  };
  const RENDER = {
    header: (cs, ctx) => (tpl.header ? cs.map((c) => iconButton(tpl.header, c, ctx)) : []),
    composer: (cs, ctx) => (tpl.composer ? cs.map((c) => iconButton(tpl.composer, c, ctx)) : []),
    content: (cs, ctx) => accessories(cs, ctx, 'content'),
    mheader: (cs, ctx) => accessories(cs, ctx, 'header'),
    action: (cs, ctx) => {
      if (!tpl.row || !tpl.group) { warnOnce('row', 'ui: no message action layout seen yet'); return []; }
      const shown = cs.filter((c) => !c.visible || call(c, 'visible', () => c.visible(ctxFor(ctx))));
      if (!shown.length) return [];
      const rows = shown.map((c, i) => {
        const r = clone(tpl.row), inner = r.kids[0], icon = inner.kids[0], label = inner.kids[1];
        for (const k of ['radius', 'radiusTopLeft', 'radiusTopRight', 'radiusBottomLeft', 'radiusBottomRight']) setProp(r.props, k);
        if (shown.length === 1) setProp(r.props, 'radius', { t: 'f64', v: 12 });
        else if (i === 0) { setProp(r.props, 'radiusTopLeft', { t: 'f64', v: 12 }); setProp(r.props, 'radiusTopRight', { t: 'f64', v: 12 }); }
        else if (i === shown.length - 1) { setProp(r.props, 'radiusBottomLeft', { t: 'f64', v: 12 }); setProp(r.props, 'radiusBottomRight', { t: 'f64', v: 12 }); }
        setProp(r.props, 'accessibilityId', str(`phoenix.${c.plugin}.${c.id}`));
        setProp(r.props, 'accessibilityLabel', str(c.label));
        setProp(label.props, 'text', str(c.label));
        setProp(icon.props, 'uri', str(c.icon || 'res://ic_mana_puzzle_piece_plus'));
        if (c.danger) { setProp(label.props, 'color', col(4294475386)); setProp(icon.props, 'tint', col(4294475386)); }
        r.h = { press: () => withMessage(ctx).then((x) => call(c, 'onPress', () => c.onPress(x))), sheet: true };
        return r;
      });
      return [{ type: tpl.group.type, props: clone(tpl.group.props), kids: rows.flatMap((r, i) => (i && tpl.divider ? [clone(tpl.divider), r] : [r])) }];
    },
    profile: (cs, ctx) => {
      if (!tpl.profile) { warnOnce('profile', 'ui: no profile section layout seen yet'); return []; }
      const out = [];
      for (const c of cs) {
        const r = call(c, 'render', () => c.render(ctxFor(ctx)));
        if (r == null || r === false) continue;
        const s = clone(tpl.profile), head = text(s.kids[0].props, c.title), node = text(s.kids[1].kids[0].props, '');
        setProp(head.props, 'accessibilityLabel', str(c.title));
        s.kids = [head, { ...s.kids[1], kids: [node] }];
        if (!resolveText(c, node, r, 'render')) { const t = textOf(r); setProp(node.props, 'text', str(t.text)); if (t.color != null) setProp(node.props, 'color', col(t.color)); }
        out.push(s);
      }
      return out;
    },
  };
  const accessories = (cs, ctx, where) => {
    const base = tpl.ts ? tpl.ts.props.filter((p) => !['accessibilityId', 'accessibilityLabel', 'accessibilityHidden', 'maxLines', 'margin', 'gravity', 'on'].includes(p.k))
      : [{ k: 'color', v: col(4288059294) }, { k: 'fontSize', v: { t: 'f64', v: 12 } }, { k: 'fontWeight', v: { t: 'i32', v: 500 } }];
    const out = [];
    for (const c of cs) {
      const r = call(c, 'render', () => c.render(ctxFor(ctx)));
      if (r == null || r === false) continue;
      const n = text(base, '');
      if (where === 'content') setProp(n.props, 'margin', { t: 'rect', v: [0, 2, 0, 0] });
      if (!resolveText(c, n, r, 'render')) { const t = textOf(r); setProp(n.props, 'text', str(t.text)); if (t.color != null) setProp(n.props, 'color', col(t.color)); }
      out.push(n);
    }
    return out;
  };

  // ---- slots: one per (target, parent) where our nodes live -------------------------------------
  const slots = new Map(); // parent -> [slot]
  const slotsAt = (p) => slots.get(p) || [];
  const fill = (s, out) => {
    const cs = contrib[s.target];
    const nodes = cs.length ? RENDER[s.target](cs, s.ctx) : [];
    for (const n of nodes) build(n, out);
    s.roots = nodes.map((n) => n.id);
    s.all = nodes.flatMap(all);
  };
  const merged = (p) => {
    const list = (kids.get(p) || []).slice();
    for (const s of slotsAt(p)) {
      if (!s.roots.length) continue;
      const i = s.before == null ? -1 : list.indexOf(s.before);
      list.splice(i < 0 ? list.length : i, 0, ...s.roots);
    }
    return list;
  };
  const place = (t, id, out) => {
    const pl = TARGETS[t].place(id, aidOf.get(id));
    if (!pl || pl.parent === undefined) return null;
    let s = slotsAt(pl.parent).find((x) => x.target === t);
    if (s) { s.before = pl.before; return null; }
    s = { target: t, parent: pl.parent, before: pl.before, ctx: pl.ctx, roots: [], all: [] };
    if (!slots.has(pl.parent)) slots.set(pl.parent, []);
    slots.get(pl.parent).push(s);
    fill(s, out);
    return s;
  };
  const dropSlot = (s) => {
    const l = slotsAt(s.parent).filter((x) => x !== s);
    l.length ? slots.set(s.parent, l) : slots.delete(s.parent);
    for (const id of s.all) handlers.delete(id);
  };

  const send = (ops) => { if (ops.length && !__phx.uiApply(UI.encode(ops))) console.warn('ui: update failed'); };
  // texts that plugins return as Promises
  const flushLater = () => {
    for (const [node, p] of later.splice(0)) p.then((t) => {
      if (!t || !live.has(node.id)) return;
      send([{ op: 'update', id: node.id, props: [{ k: 'text', v: str(t.text) }, ...(t.color != null ? [{ k: 'color', v: col(t.color) }] : [])] }]);
    });
  };
  const live = new Set(); // ids of our nodes that are on screen

  // ---- Rust's batches ----------------------------------------------------------------------------
  phoenix.bridge.onOps((u8) => {
    let ops;
    try { ops = UI.decode(u8); } catch { return; }
    const creates = new Map(), extraDelete = [];
    for (const o of ops) {
      if (o.op === 'create') {
        creates.set(o.id, o);
        const a = prop(o.props, 'accessibilityId');
        if (a && a.t === 'str') aidOf.set(o.id, a.v);
        if (o.type === 'sheet') sheets.add(o.id);
      } else if (o.op === 'children') {
        kids.set(o.id, o.ids.slice());
        for (const c of o.ids) parent.set(c, o.id);
      } else if (o.op === 'childEdits') {
        kids.set(o.id, applyEdits(kids.get(o.id) || [], o.edits));
        for (const e of o.edits) for (const c of e.ins) parent.set(c, o.id);
      } else if (o.op === 'delete') {
        for (const id of o.ids) {
          for (const s of slotsAt(id)) { extraDelete.push(...s.all); s.all.forEach((x) => live.delete(x)); dropSlot(s); }
          kids.delete(id); parent.delete(id); aidOf.delete(id); sheets.delete(id); composerText.delete(id);
          for (const t in anchors) anchors[t].delete(id);
        }
      } else if (o.op === 'update' && composerText.has(o.id)) {
        const v = prop(o.props, 'value');
        if (v && v.t === 'str') composerText.set(o.id, v.v);
      }
    }
    // new anchors: snapshot Phoenix's layouts, open slots for targets that have contributions
    const mine = [], touched = new Set(), named = [...creates.keys()].filter((id) => aidOf.has(id));
    for (const id of named) snapshot(id, aidOf.get(id), creates);
    for (const id of named) {
      const a = aidOf.get(id);
      if (a === 'composer') composerText.set(id, composerText.get(id) ?? '');
      if (a === 'profile-tab-pages') {
        profileUser = null;
        for (const [cid, c] of creates) { if (cid > id) break; const u = prop(c.props, 'uri'); const m = u && /\/avatars\/(\d+)\//.exec(u.v); if (m) { profileUser = m[1]; break; } }
      }
      for (const t in TARGETS) {
        if (!TARGETS[t].match(a)) continue;
        anchors[t].add(id);
        if (!contrib[t].length) continue;
        const s = place(t, id, mine);
        if (s) { touched.add(s.parent); s.all.forEach((x) => live.add(x)); }
      }
    }
    if (!touched.size && !extraDelete.length && ![...ops].some((o) => (o.op === 'children' || o.op === 'childEdits') && slots.has(o.id))) return;
    const out = mine;
    for (const o of ops) {
      if ((o.op === 'children' || o.op === 'childEdits') && slots.has(o.id)) { out.push({ op: 'children', id: o.id, ids: merged(o.id) }); touched.delete(o.id); }
      else if (o.op === 'delete' && extraDelete.length) out.push({ op: 'delete', ids: o.ids.concat(extraDelete.splice(0)) });
      else out.push(o);
    }
    for (const p of touched) out.push({ op: 'children', id: p, ids: merged(p) });
    // ponytail: Java applies this batch after we return, outside our lock; a short delay keeps the
    // async texts behind it. Queue them per batch if 100 ms ever proves short.
    if (later.length) setTimeout(flushLater, 100);
    return UI.encode(out);
  });

  // Re-renders every slot of a target (after a plugin adds or removes something). JS thread only.
  const refresh = (t) => {
    const out = [], del = [], touched = new Set();
    for (const list of slots.values()) for (const s of list.filter((x) => x.target === t)) {
      del.push(...s.all);
      s.all.forEach((x) => { handlers.delete(x); live.delete(x); });
      fill(s, out);
      s.all.forEach((x) => live.add(x));
      touched.add(s.parent);
    }
    if (contrib[t].length) for (const id of anchors[t]) { const s = place(t, id, out); if (s) { touched.add(s.parent); s.all.forEach((x) => live.add(x)); } }
    for (const p of touched) out.push({ op: 'children', id: p, ids: merged(p) });
    if (del.length) out.push({ op: 'delete', ids: del });
    send(out);
    flushLater();
  };
  let pending = new Set(), timer = 0;
  const schedule = (t) => { pending.add(t); if (!timer) timer = setTimeout(() => { timer = 0; const ts = [...pending]; pending = new Set(); ts.forEach(refresh); }, 0); };

  // ---- context for handlers ------------------------------------------------------------------------
  const messageById = async (id) => {
    const r = await phoenix.stores.sql('SELECT k, v FROM messages WHERE k LIKE ? LIMIT 1', ['%:' + pad(id)], 1);
    if (!r.rows.length) return null;
    let v = r.rows[0][1];
    try { v = JSON.parse(v); } catch {}
    return { channelId: String(BigInt(r.rows[0][0].split(':')[0])), message: v };
  };
  let channelId = null;
  // the open channel: Phoenix's persisted selection, else the newest message on screen
  const currentChannel = async () => {
    const sel = await phoenix.stores.meta('selected').catch(() => null);
    if (sel && sel.channel_id) return (channelId = String(sel.channel_id));
    let best = null;
    for (const id of anchors.content) { const m = MSG.exec(aidOf.get(id) || ''); if (m && (best === null || BigInt(m[1]) > BigInt(best))) best = m[1]; }
    if (best) { const m = await messageById(best).catch(() => null); if (m) channelId = m.channelId; }
    return channelId;
  };
  const reply = (cid) => (content) => (cid ? Promise.resolve(cid) : currentChannel()).then((c) => {
    if (!c) throw new Error('no open channel');
    return phoenix.messages.addLocal(c, String(content));
  });
  const ctxFor = (ctx) => ({ ...ctx, channelId: ctx.channelId ?? channelId });
  const withChannel = async (ctx) => { const c = await currentChannel(); return { ...ctx, channelId: c, reply: reply(c) }; };
  const withMessage = async (ctx) => {
    const m = ctx.messageId ? await messageById(ctx.messageId).catch(() => null) : null;
    if (m) channelId = m.channelId;
    return { ...ctx, channelId: m ? m.channelId : await currentChannel(), message: m && m.message, reply: reply(m && m.channelId) };
  };

  // ---- events: our nodes, long presses, the composer -----------------------------------------------
  const composerText = new Map(); // composer input node -> its text
  const commands = new Map(), interceptors = [];
  const payloadField = (p, k) => p && p.t === 'obj' && (p.v.find((x) => x.k === k) || {}).v?.v;
  // frames -> one kind-4 batch frame (plugin/ui/FORMAT.md)
  const batchFrame = (frames) => {
    const size = 16 + frames.reduce((a, f) => a + 4 + f.length, 0), out = new Uint8Array(size), dv = new DataView(out.buffer);
    out[0] = 4;
    let p = 16;
    for (const f of frames) { dv.setUint32(p, f.length, true); out.set(f, p + 4); p += 4 + f.length; }
    return out;
  };
  const changeText = (node, value) => UI.encodeViewEvent(node, 'changeText', { t: 'obj', v: [{ k: 'text', v: str(value) }] });
  const clearComposer = (node) => { composerText.set(node, ''); setTimeout(() => send([{ op: 'update', id: node, props: [{ k: 'value', v: str('') }] }]), 0); };
  // What to do with a message about to be sent: {cancel} or {text}.
  const onSubmit = (node) => {
    let value = composerText.get(node) ?? '';
    const ctx = { channelId, reply: reply(null) };
    const m = /^\/(\S+)(?:\s+([\s\S]*))?$/.exec(value);
    const cmd = m && commands.get(m[1].toLowerCase());
    if (cmd) {
      const r = call(cmd, `/${cmd.name}`, () => cmd.execute((m[2] || '').trim(), ctx));
      if (r && typeof r.then === 'function') { r.catch((e) => console.error(`[${cmd.plugin}] /${cmd.name}:`, e)); return { cancel: true }; }
      const t = r && typeof r === 'object' ? r.content : r;
      if (t == null || t === false || t === '') return { cancel: true };
      value = String(t);
    }
    for (const it of interceptors) {
      const r = call(it, 'onSend', () => it.fn(value, ctx));
      if (r === false) return { cancel: true };
      if (typeof r === 'string') value = r;
    }
    return value === (composerText.get(node) ?? '') ? null : { text: value };
  };
  const indexOfRow = (list, index) => {
    const row = (kids.get(list) || [])[index];
    const hit = row !== undefined && findDown(row, MSG, 8);
    return hit !== undefined && hit !== false ? MSG.exec(aidOf.get(hit))[1] : null;
  };
  const slotOf = (id) => { for (const l of slots.values()) for (const s of l) if (s.all.includes(id)) return s; };
  const sheetOf = (id) => { for (let x = slotOf(id)?.parent, i = 0; x !== undefined && i < 40; x = parent.get(x), i++) if (sheets.has(x)) return x; };

  phoenix.bridge.onDispatch((u8) => {
    let items;
    try { items = UI.decodeDispatch(u8); } catch { return; }
    let changed = false;
    const frames = [];
    for (const it of items) {
      const raw = u8.subarray(it.off, it.off + it.len);
      if (it.kind !== 'view') { frames.push(raw); continue; }
      if (it.event === 'rowLongPress') { const i = payloadField(it.payload, 'index'); if (i != null) lastLongPress = indexOfRow(it.node, i); }
      if (it.event === 'changeText' && composerText.has(it.node)) composerText.set(it.node, payloadField(it.payload, 'text') ?? '');
      const h = it.node >= FIRST_ID && handlers.get(it.node);
      if (h) {
        changed = true;
        if (it.event === 'press' && h.press) {
          setTimeout(h.press, 0);
          const sheet = h.sheet ? sheetOf(it.node) : undefined;
          if (sheet !== undefined) frames.push(UI.encodeViewEvent(sheet, 'dismiss'));
        }
        continue;
      }
      // sending: the keyboard's submit on the input, or the send button next to it
      const input = it.event === 'submit' && composerText.has(it.node) ? it.node
        : it.event === 'press' && aidOf.get(it.node) === 'send' ? [...composerText.keys()].at(-1) : undefined;
      if (input !== undefined && (commands.size || interceptors.length)) {
        const r = onSubmit(input);
        if (r && r.cancel) { frames.push(changeText(input, '')); clearComposer(input); changed = true; continue; }
        if (r) { frames.push(changeText(input, r.text), raw); changed = true; continue; }
      }
      frames.push(raw);
    }
    if (!changed) return;
    if (!frames.length) return false;
    return frames.length === 1 ? frames[0] : batchFrame(frames);
  });

  // ---- registration (phoenix.ui.add*, phoenix.commands, phoenix.messages.onSend) -----------------
  let seq = 0;
  const add = (plugin, t, o) => {
    if (!o || typeof o !== 'object') throw new TypeError('options object expected');
    const c = { ...o, plugin, id: String(o.id || `${t}${++seq}`) };
    contrib[t].push(c);
    contrib[t].sort((a, b) => (b.priority || 0) - (a.priority || 0));
    schedule(t);
    const off = () => { const i = contrib[t].indexOf(c); if (i >= 0) { contrib[t].splice(i, 1); schedule(t); } };
    off.update = () => schedule(t);
    return off;
  };
  globalThis.__phx_patches = {
    add,
    command: (plugin, o) => {
      if (!o || !o.name || typeof o.execute !== 'function') throw new TypeError('commands.register({name, execute}) expected');
      const c = { ...o, plugin, name: String(o.name).replace(/^\//, '').toLowerCase() };
      commands.set(c.name, c);
      return () => { if (commands.get(c.name) === c) commands.delete(c.name); };
    },
    commands: () => [...commands.values()].map((c) => ({ name: c.name, description: c.description || '', plugin: c.plugin })),
    onSend: (plugin, fn) => { const it = { plugin, fn }; interceptors.push(it); return () => { const i = interceptors.indexOf(it); if (i >= 0) interceptors.splice(i, 1); }; },
  };
})();
