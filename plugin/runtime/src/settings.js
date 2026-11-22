'use strict';
// Phoenix in Discord's Settings: one "Phoenix" category row at the top of the list, opening a
// page of its own (pushed on Settings' navigator like Account etc.) with a switch per JS plugin,
// Reload and About. Built from Phoenix's own layouts (settings_tpl.js) and spliced into the
// UI-op stream (plugin/ui/FORMAT.md). Our nodes use ids from 0x50000000, far above Rust's counter.
(() => {
  const UI = PhxUI, T = __phx_tpl, PL = __phx_plugins;
  const ICON = 'res://ic_mana_puzzle_piece_plus', ICON_RELOAD = 'res://ic_mana_puzzle_piece_retry';
  let nextId = 0x50000000;
  // the open Settings: list (main list node), rust (Rust's children of it), search, nav,
  // navRust (Rust's pages), entry (our category section), page (our pushed page, or null)
  let inst = null;
  const navs = new Set(); // every navigator Rust created (the phone layout pushes Settings onto the app's own)

  const str = (v) => ({ t: 'str', v });
  const bool = (b) => ({ t: b ? 'true' : 'false' });
  const clone = (x) => JSON.parse(JSON.stringify(x));
  const setProp = (props, k, v) => {
    const i = props.findIndex((p) => p.k === k);
    if (v === undefined) { if (i >= 0) props.splice(i, 1); } else if (i >= 0) props[i] = { k, v }; else props.push({ k, v });
  };
  const noRadius = (props) => ['radius', 'radiusTopLeft', 'radiusTopRight', 'radiusBottomLeft', 'radiusBottomRight'].forEach((k) => setProp(props, k));

  // Creates a template tree (children first); returns ops, ids, and sets node.id everywhere.
  const build = (root) => {
    const ops = [], ids = [];
    const walk = (node) => {
      const kids = node.kids.map(walk);
      node.id = nextId++;
      ids.push(node.id);
      ops.push({ op: 'create', id: node.id, type: node.type, props: node.props });
      if (kids.length) ops.push({ op: 'children', id: node.id, ids: kids });
      return node.id;
    };
    walk(root);
    return { ops, ids };
  };
  const all = (n) => [n.id, ...n.kids.flatMap(all)];
  const findSrc = (n, src) => (n.src === src ? n : n.kids.map((k) => findSrc(k, src)).find(Boolean));

  // ---- rows (layouts: see settings_tpl.js) ------------------------------------------------
  const section = (title, rows) => {
    const s = clone(T.section), h = clone(T.header), g = clone(T.group);
    setProp(h.props, 'text', str(title));
    g.kids = rows.flatMap((n, i) => (i ? [clone(T.divider), n] : [n]));
    s.kids = title ? [h, g] : [g];
    return s;
  };
  // root > row > [icon, text, chevron]
  const buttonRow = (title, icon, key, chevron) => {
    const r = clone(T.buttonRow), row = r.kids[0];
    noRadius(r.props);
    setProp(r.props, 'accessibilityLabel', str(title));
    setProp(r.props, 'accessibilityId', str(`phoenix-${key}`));
    setProp(row.kids[0].props, 'uri', str(icon));
    setProp(row.kids[1].props, 'text', str(title));
    if (!chevron) row.kids.length = 2;
    return r;
  };
  // root > row > [label, value wrap > value, chevron]
  const valueRow = (label, value) => {
    const r = clone(T.valueRow), row = r.kids[0];
    noRadius(r.props);
    setProp(r.props, 'on');
    setProp(r.props, 'role');
    setProp(r.props, 'accessibilityLabel', str(`${label}, ${value}`));
    setProp(row.kids[0].props, 'text', str(label));
    setProp(row.kids[1].kids[0].kids[0].props, 'text', str(value));
    row.kids.length = 2;
    return r;
  };
  // root > row > [icon, column > [title, subtitle], wrap > track > knob]
  const switchRow = (title, subtitle, on, key) => {
    const r = clone(T.switchRow);
    noRadius(r.props);
    setProp(r.props, 'accessibilityLabel', str(subtitle ? `${title}, ${subtitle}` : title));
    setProp(r.props, 'accessibilityChecked', bool(on));
    setProp(r.props, 'accessibilityId', str(`phoenix-${key}`));
    const [icon, col, wrap] = r.kids[0].kids;
    setProp(icon.props, 'uri', str(ICON));
    setProp(col.kids[0].props, 'text', str(title));
    if (subtitle) setProp(col.kids[1].props, 'text', str(subtitle)); else col.kids.pop();
    const track = wrap.kids[0], knob = track.kids[0];
    if (!on) { track.props = clone(T.trackOff); knob.props = clone(T.knobOff); }
    return { node: r, track, knob, sub: col.kids[1] };
  };
  const subtitleOf = (p) => (p.error ? `Failed: ${p.error}` : p.description) || '';

  // ---- main list entry --------------------------------------------------------------------
  const buildEntry = () => {
    const row = buttonRow('Phoenix', ICON, 'category', true);
    const s = section('', [row]);
    const b = build(s);
    return { ...b, root: s.id, press: new Map(all(row).map((id) => [id, { kind: 'open' }])) };
  };

  // ---- the page ---------------------------------------------------------------------------
  const buildPage = () => {
    const pg = clone(T.page), press = new Map(), switches = [];
    setProp(findSrc(pg, 0x56a).props, 'text', str('Phoenix'));
    setProp(pg.props, 'paneTitle', str('Phoenix'));
    // status-bar inset: Rust bakes it into each page header, so copy the live Settings header's
    if (inst.hdr) {
      const h = findSrc(pg, 0x562);
      for (const k of ['padding', 'height']) { const v = inst.hdr.find((p) => p.k === k); if (v) setProp(h.props, k, v.v); }
      const ht = inst.hdr.find((p) => p.k === 'height');
      if (ht) setProp(findSrc(pg, 0x4e1).props, 'height', ht.v);
    }
    const plugins = PL.list();
    const rows = plugins.map((p) => {
      const s = switchRow(p.name, subtitleOf(p), p.enabled, `plugin-${p.id}`);
      switches.push({ id: p.id, s });
      return s.node;
    });
    if (!rows.length) rows.push(buttonRow('No JS plugins installed', ICON, 'none', false));
    const reload = buttonRow('Reload plugins', ICON_RELOAD, 'reload', false);
    findSrc(pg, 0x4e4).kids = [
      section('Plugins', rows),
      section('', [reload]),
      section('About', [valueRow('Runtime', 'rquickjs (QuickJS-ng)'), valueRow('Plugins', String(plugins.length)), valueRow('Folder', 'files/phoenix-js')]),
    ];
    const back = findSrc(pg, 0x565);
    setProp(back.props, 'accessibilityLabel', str('Back'));
    const b = build(pg);
    for (const sw of switches) for (const id of all(sw.s.node)) press.set(id, { kind: 'plugin', id: sw.id, s: sw.s });
    for (const id of all(reload)) press.set(id, { kind: 'reload', switches });
    for (const id of all(back)) press.set(id, { kind: 'back' });
    return { ...b, root: pg.id, press };
  };

  const send = (ops) => { if (!__phx.uiApply(UI.encode(ops))) console.warn('settings: UI update failed'); };
  const openPage = () => {
    if (!inst || inst.page) return;
    if (!inst.navRust) return console.warn(`settings: no navigator pages known (nav ${inst.nav})`);
    const p = buildPage();
    inst.page = p;
    send([...p.ops, { op: 'children', id: inst.nav, ids: [...inst.navRust, p.root] }, { op: 'update', id: inst.nav, props: [{ k: 'index', v: { t: 'i32', v: inst.navRust.length } }] }]);
  };
  const closePage = () => {
    if (!inst || !inst.page) return;
    const p = inst.page;
    inst.page = null;
    send([{ op: 'update', id: inst.nav, props: [{ k: 'index', v: { t: 'i32', v: inst.navRust.length - 1 } }] },
      { op: 'children', id: inst.nav, ids: inst.navRust }, { op: 'delete', ids: p.ids }]);
  };
  const refresh = (row) => {
    const p = PL.list().find((x) => x.id === row.id);
    if (!p) return;
    const on = p.enabled, s = row.s, track = clone(T.switchRow).kids[0].kids[2].kids[0];
    const ops = [
      { op: 'update', id: s.track.id, props: on ? track.props : clone(T.trackOff) },
      { op: 'update', id: s.knob.id, props: on ? track.kids[0].props : clone(T.knobOff) },
      { op: 'update', id: s.node.id, props: [{ k: 'accessibilityChecked', v: bool(on) }] },
    ];
    if (s.sub) ops.push({ op: 'update', id: s.sub.id, props: [{ k: 'text', v: str(subtitleOf(p)) }] });
    send(ops);
  };
  const press = (node) => {
    const r = inst && ((inst.entry && inst.entry.press.get(node)) || (inst.page && inst.page.press.get(node)));
    if (!r) return;
    if (r.kind === 'open') openPage();
    else if (r.kind === 'back') closePage();
    else if (r.kind === 'plugin') { const p = PL.list().find((x) => x.id === r.id); if (p) PL.setEnabled(p.id, !p.enabled); refresh(r); }
    else if (r.kind === 'reload') { PL.reload(); for (const sw of r.switches) refresh({ id: sw.id, s: sw.s }); }
  };
  const ours = (node) => !!inst && (inst.entry?.press.has(node) || inst.page?.press.has(node) || false);

  // ---- splice into Rust's batches -------------------------------------------------------------
  const applyEdits = (old, edits) => {
    const out = [];
    let i = 0;
    for (const e of edits) { out.push(...old.slice(i, i + e.keep)); i += e.keep + e.del; out.push(...e.ins); }
    return out.concat(old.slice(i));
  };
  const withEntry = (list) => { const at = list[0] === inst.search ? 1 : 0; return [...list.slice(0, at), inst.entry.root, ...list.slice(at)]; };

  phoenix.bridge.onOps((u8) => {
    let ops;
    try { ops = UI.decode(u8); } catch (e) { console.warn('settings: undecodable batch', e.message); return; }
    const creates = new Map(), parent = new Map();
    for (const o of ops) {
      if (o.op === 'create') { creates.set(o.id, o); if (o.type === 'navigator') navs.add(o.id); }
      else if (o.op === 'children') for (const c of o.ids) parent.set(c, o.id);
    }
    // the Settings list: Log Out row > group > section > list (accessibilityId is language-neutral)
    for (const o of creates.values()) {
      if (!o.props.some((p) => p.k === 'accessibilityId' && p.v.v === 'settings.row.logout')) continue;
      const list = parent.get(parent.get(parent.get(o.id)));
      if (list === undefined || (inst && inst.list === list)) continue;
      // nearest navigator: a known one, or the container of the page that holds the list
      // (tablet: Settings has its own navigator; phone: Settings is a page on the app's root one)
      let nav, x = list;
      while (x !== undefined) {
        if (navs.has(x)) { nav = x; break; }
        if (creates.get(x)?.props.some((p) => p.k === 'presentation' || p.k === 'uikitPush')) { nav = parent.get(x); break; }
        x = parent.get(x);
      }
      const hdr = [...creates.values()].find((c) => c.props.some((p) => p.k === 'stackHeader'));
      inst = { list, rust: null, search: null, nav, navRust: null, entry: null, page: null, hdr: hdr && hdr.props };
    }
    if (!inst) return;
    let changed = false;
    const out = [];
    for (let i = 0; i < ops.length; i++) {
      const o = ops[i];
      if (o.id === inst.list && (o.op === 'children' || (o.op === 'childEdits' && inst.rust))) {
        if (o.op === 'children') {
          inst.rust = o.ids.slice();
          if (inst.search === null) inst.search = creates.get(o.ids[0])?.type === 'stack' ? o.ids[0] : -1;
        } else inst.rust = applyEdits(inst.rust, o.edits);
        if (!inst.entry) { inst.entry = buildEntry(); out.push(...inst.entry.ops); }
        out.push({ op: 'children', id: inst.list, ids: withEntry(inst.rust) });
        changed = true;
      } else if (o.id === inst.nav && (o.op === 'children' || (o.op === 'childEdits' && inst.navRust))) {
        inst.navRust = o.op === 'children' ? o.ids.slice() : applyEdits(inst.navRust, o.edits);
        if (inst.page) { out.push({ op: 'children', id: inst.nav, ids: [...inst.navRust, inst.page.root] }); changed = true; }
        else out.push(o);
      } else if (o.op === 'delete' && (o.ids.includes(inst.list) || o.ids.includes(inst.nav))) {
        // Settings closed; Java does not delete recursively
        out.push({ op: 'delete', ids: o.ids.concat(inst.entry?.ids || [], inst.page?.ids || []) });
        inst = null;
        changed = true;
        out.push(...ops.slice(i + 1));
        break;
      } else out.push(o);
    }
    return changed ? UI.encode(out) : undefined;
  });

  // Taps on our rows, and back navigation away from our page, go to us instead of Rust.
  phoenix.bridge.onDispatch((u8) => {
    if (!inst) return;
    let items;
    try { items = UI.decodeDispatch(u8); } catch { return; }
    const mine = items.filter((it) => it.kind === 'view' && (ours(it.node) || (inst.page && it.node === inst.nav && it.event === 'navigate')));
    if (!mine.length) return;
    // this runs on the main thread; UI ops must be sent from the JS thread
    for (const it of mine) {
      if (it.event === 'press') { const n = it.node; setTimeout(() => press(n), 0); }
      else if (it.event === 'navigate') setTimeout(closePage, 0);
    }
    if (mine.length === items.length) return false;
    if (u8[0] !== 4) return false;
    const dv = new DataView(u8.buffer, u8.byteOffset, u8.byteLength), skip = new Set(mine.map((m) => m.off));
    let p = 16 + dv.getUint32(12, true);
    const parts = [u8.subarray(0, p)];
    while (p + 4 <= u8.length) { const n = dv.getUint32(p, true); if (!skip.has(p + 4)) parts.push(u8.subarray(p, p + 4 + n)); p += 4 + n; }
    const res = new Uint8Array(parts.reduce((a, x) => a + x.length, 0));
    let o = 0; for (const x of parts) { res.set(x, o); o += x.length; }
    return res;
  });
})();
