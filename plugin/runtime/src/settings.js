'use strict';
// Phoenix in Discord's Settings: a "Phoenix" row at the top of the list opens our pages, pushed on
// Settings' navigator (tablet) or the app's root one (phone) like Discord's own sub-pages:
//   Phoenix: plugins (tap: its page, switch: on/off), Install plugin (URL or file), Restore, Console, Reload, About
//   <plugin>: Enabled, the plugin's own settings (phoenix.settings.define), Reload, Remove, Info
//   Console: a JS prompt and the live log
// Everything is built from Phoenix's own layouts (settings_tpl.js) and spliced into the UI-op
// stream (plugin/ui/FORMAT.md). Our nodes use ids from 0x50000000, far above Rust's counter.
(() => {
  const UI = PhxUI, T = __phx_tpl, PL = __phx_plugins, LOG = __phx_log;
  const ICON = {
    plugin: 'res://ic_mana_puzzle_piece_plus', reload: 'res://ic_mana_puzzle_piece_retry', remove: 'res://ic_mana_puzzle_piece_minus',
    url: 'res://ic_mana_browser_link', file: 'res://ic_mana_image_file_up', console: 'res://ic_mana_angle_brackets',
  };
  const COLOR = { text: 4294704123, muted: 4288059294, warn: 4293964338, danger: 4294475386 }; // ARGB
  const LEVEL_COLOR = [COLOR.text, COLOR.text, COLOR.warn, COLOR.danger, COLOR.muted];
  let nextId = 0x50000000;
  // the open Settings: list (main list node), rust (Rust's children of it), search, nav,
  // navRust (Rust's pages on nav), hdr (live header props), entry (our row), stack (our pages)
  let inst = null;
  const navs = new Set(); // every navigator Rust created (the phone layout pushes Settings onto the app's own)

  const str = (v) => ({ t: 'str', v });
  const bool = (b) => ({ t: b ? 'true' : 'false' });
  const num = (v) => ({ t: 'f64', v });
  const col = (v) => ({ t: 'i64', v: String(v) });
  const clone = (x) => JSON.parse(JSON.stringify(x));
  const setProp = (props, k, v) => {
    const i = props.findIndex((p) => p.k === k);
    if (v === undefined) { if (i >= 0) props.splice(i, 1); } else if (i >= 0) props[i] = { k, v }; else props.push({ k, v });
  };
  const noRadius = (props) => ['radius', 'radiusTopLeft', 'radiusTopRight', 'radiusBottomLeft', 'radiusBottomRight'].forEach((k) => setProp(props, k));
  const all = (n) => [n.id, ...n.kids.flatMap(all)];
  const findSrc = (n, src) => (n.src === src ? n : n.kids.map((k) => findSrc(k, src)).find(Boolean));

  // Creates a tree (children first). Nodes may carry `h` (event handlers), inherited by their subtree.
  const build = (root, handlers) => {
    const ops = [];
    const walk = (node) => {
      const kids = node.kids.map(walk);
      node.id = nextId++;
      ops.push({ op: 'create', id: node.id, type: node.type, props: node.props });
      if (kids.length) ops.push({ op: 'children', id: node.id, ids: kids });
      return node.id;
    };
    walk(root);
    const reg = (n, h) => { h = n.h || h; if (h) handlers.set(n.id, h); n.kids.forEach((k) => reg(k, h)); };
    reg(root, null);
    return ops;
  };

  // ---- rows (layouts: see settings_tpl.js) ------------------------------------------------
  const section = (title, rows, grouped = true) => {
    const s = clone(T.section), hd = clone(T.header);
    setProp(hd.props, 'text', str(title));
    let body;
    if (grouped) { body = clone(T.group); body.kids = rows.flatMap((n, i) => (i ? [clone(T.divider), n] : [n])); }
    else { body = { type: 'view', props: clone(T.section.props), kids: rows }; setProp(body.props, 'gap', num(2)); }
    s.kids = title ? [hd, body] : [body];
    s.body = body;
    return s;
  };
  // root > row > [icon, text, chevron]
  const buttonRow = (title, icon, press, { chevron = false, danger = false, key = title } = {}) => {
    const r = clone(T.buttonRow), row = r.kids[0];
    noRadius(r.props);
    setProp(r.props, 'accessibilityLabel', str(title));
    setProp(r.props, 'accessibilityId', str(`phoenix-${key}`));
    setProp(row.kids[0].props, 'uri', str(icon));
    setProp(row.kids[1].props, 'text', str(title));
    if (danger) { setProp(row.kids[1].props, 'color', col(COLOR.danger)); setProp(row.kids[0].props, 'tint', col(COLOR.danger)); }
    if (!chevron) row.kids.length = 2;
    if (press) r.h = { press };
    else { setProp(r.props, 'on'); setProp(r.props, 'role'); }
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
  const subtitleOf = (p) => (p.error ? `Failed: ${p.error}` : p.description) || '';
  // root > row > [icon, column > [title, subtitle], wrap > track > knob]
  // o: {key, title, subtitle, on, icon?}; state() -> {on, subtitle} for refreshes. Tapping the row
  // runs onRow (default: toggle), tapping the switch runs onToggle.
  const switchRow = (o, state, onToggle, onRow) => {
    const r = clone(T.switchRow);
    noRadius(r.props);
    setProp(r.props, 'accessibilityLabel', str(o.subtitle ? `${o.title}, ${o.subtitle}` : o.title));
    setProp(r.props, 'accessibilityChecked', bool(o.on));
    setProp(r.props, 'accessibilityId', str(`phoenix-${o.key}`));
    const [icon, column, wrap] = r.kids[0].kids;
    if (o.icon === null) r.kids[0].kids.shift(); else setProp(icon.props, 'uri', str(o.icon || ICON.plugin));
    setProp(column.kids[0].props, 'text', str(o.title));
    if (o.subtitle) setProp(column.kids[1].props, 'text', str(o.subtitle)); else column.kids.pop();
    const track = wrap.kids[0], knob = track.kids[0];
    if (!o.on) { track.props = clone(T.trackOff); knob.props = clone(T.knobOff); }
    const sw = { root: r, track, knob, sub: column.kids[1], state };
    r.h = { press: () => (onRow || onToggle)(sw) };
    track.h = { press: () => onToggle(sw) };
    return r;
  };
  const pluginState = (id) => () => { const p = PL.list().find((x) => x.id === id); return p && { on: p.enabled, subtitle: subtitleOf(p) }; };
  // Settings' search field: row > box > [icon wrap > icon, input]
  const inputRow = (placeholder, icon, key, onSubmit) => {
    const r = clone(T.input), box = r.kids[0], input = box.kids[0].kids[1];
    setProp(box.kids[0].kids[0].kids[0].props, 'uri', str(icon));
    setProp(input.props, 'placeholder', str(placeholder));
    setProp(input.props, 'accessibilityId', str(`phoenix-${key}`));
    setProp(input.props, 'role');
    setProp(input.props, 'returnKey', str('go'));
    const h = { text: '', changeText: (t) => { h.text = t; }, submit: () => onSubmit(h.text, h) };
    input.h = h;
    h.node = input;
    return r;
  };
  // small text: console lines (monospace) and notes
  const textLine = (text, color = COLOR.text, mono = true) => {
    const t = clone(T.header);
    setProp(t.props, 'role');
    setProp(t.props, 'margin');
    for (const [k, v] of [['text', str(text)], ['color', col(color)], ['fontSize', num(12)], ['lineHeight', num(16)],
      ...(mono ? [['fontFamily', str('monospace')]] : []), ['selectable', bool(true)]]) setProp(t.props, k, v);
    return t;
  };

  const send = (ops) => { if (ops.length && !__phx.uiApply(UI.encode(ops))) console.warn('settings: UI update failed'); };
  const pageIndex = () => inst.navRust.length + inst.stack.length - 1;
  const navOps = () => [{ op: 'children', id: inst.nav, ids: [...inst.navRust, ...inst.stack.map((p) => p.root)] },
    { op: 'update', id: inst.nav, props: [{ k: 'index', v: { t: 'i32', v: pageIndex() } }] }];

  // ---- pages: { title, render(page) -> sections, onOpen?(page), onClose?(page) } -----------------
  const openPage = (def) => {
    if (!inst || !inst.navRust) return console.warn(`settings: no navigator pages known (nav ${inst && inst.nav})`);
    const pg = clone(T.page), page = { def, handlers: new Map() };
    setProp(findSrc(pg, 0x56a).props, 'text', str(def.title));
    setProp(pg.props, 'paneTitle', str(def.title));
    if (inst.hdr) { // status-bar inset: Rust bakes it into each page header, so copy the live one
      const h = findSrc(pg, 0x562), ht = inst.hdr.find((p) => p.k === 'height');
      for (const k of ['padding', 'height']) { const v = inst.hdr.find((p) => p.k === k); if (v) setProp(h.props, k, v.v); }
      if (ht) setProp(findSrc(pg, 0x4e1).props, 'height', ht.v);
    }
    const back = findSrc(pg, 0x565);
    setProp(back.props, 'accessibilityLabel', str('Back'));
    back.h = { press: closeTop };
    const content = findSrc(pg, 0x4e4);
    page.sections = def.render(page);
    content.kids = page.sections;
    const ops = build(pg, page.handlers);
    const sectionIds = new Set(page.sections.flatMap(all));
    // the page's own handlers (Back), kept when the sections are re-rendered
    page.base = new Map([...page.handlers].filter(([id]) => !sectionIds.has(id)));
    Object.assign(page, { root: pg.id, content: content.id, ids: all(pg) });
    inst.stack.push(page);
    send([...ops, ...navOps()]);
    def.onOpen?.(page);
  };
  function closeTop() {
    const page = inst && inst.stack.pop();
    if (!page) return;
    page.def.onClose?.(page);
    send([...navOps(), { op: 'delete', ids: page.ids }]);
  }
  const rerender = (page) => {
    if (!inst || !inst.stack.includes(page)) return;
    const old = page.sections.flatMap(all), handlers = new Map(page.base), sections = page.def.render(page), ops = [];
    for (const s of sections) ops.push(...build(s, handlers));
    page.sections = sections;
    page.handlers = handlers;
    page.ids = page.ids.filter((id) => !old.includes(id)).concat(sections.flatMap(all));
    send([...ops, { op: 'children', id: page.content, ids: sections.map((s) => s.id) }, { op: 'delete', ids: old }]);
  };

  const refreshSwitch = (sw) => {
    const st = sw.state();
    if (!st) return;
    const on = st.on, track = clone(T.switchRow).kids[0].kids[2].kids[0];
    const ops = [
      { op: 'update', id: sw.track.id, props: on ? track.props : clone(T.trackOff) },
      { op: 'update', id: sw.knob.id, props: on ? track.kids[0].props : clone(T.knobOff) },
      { op: 'update', id: sw.root.id, props: [{ k: 'accessibilityChecked', v: bool(on) }] },
    ];
    if (sw.sub && st.subtitle !== undefined) ops.push({ op: 'update', id: sw.sub.id, props: [{ k: 'text', v: str(st.subtitle) }] });
    send(ops);
  };
  const togglePlugin = (id) => (sw) => { const p = PL.list().find((x) => x.id === id); if (p) PL.setEnabled(id, !p.enabled); refreshSwitch(sw); };
  const rerenderMain = () => { const main = inst && inst.stack.find((p) => p.def === PHOENIX); if (main) rerender(main); };
  const pluginRow = (p) => switchRow({ key: `plugin-${p.id}`, title: p.name, subtitle: subtitleOf(p), on: p.enabled }, pluginState(p.id), togglePlugin(p.id), () => openPage(pluginPage(p.id)));
  const setText = (node, text, color) => send([{ op: 'update', id: node.id, props: [{ k: 'text', v: str(text) }, ...(color ? [{ k: 'color', v: col(color) }] : [])] }]);
  const clearInput = (h) => { h.text = ''; send([{ op: 'update', id: h.node.id, props: [{ k: 'value', v: str('') }] }]); };

  const PHOENIX = {
    title: 'Phoenix',
    render: () => {
      const plugins = PL.list(), removed = PL.removedBundled();
      const rows = plugins.map(pluginRow);
      if (!rows.length) rows.push(buttonRow('No JS plugins installed', ICON.plugin));
      return [
        section('Plugins', rows),
        section('', [buttonRow('Install plugin', ICON.url, () => openPage(INSTALL), { chevron: true, key: 'install' })]),
        ...(removed.length ? [section('Removed (came with the app)', removed.map((p) => buttonRow(`Restore ${p.name}`, ICON.plugin, () => { PL.restore(p.id); rerenderMain(); }, { key: `restore-${p.id}` })))] : []),
        section('Debug', [
          buttonRow('Console', ICON.console, () => openPage(CONSOLE), { chevron: true, key: 'console' }),
          buttonRow('Reload plugins', ICON.reload, () => { PL.reload(); rerenderMain(); }, { key: 'reload' }),
        ]),
        section('About', [valueRow('Runtime', 'rquickjs (QuickJS-ng)'), valueRow('Native hooks', phoenix.native.inlineHooks ? 'ShadowHook' : 'not on this ABI'),
          valueRow('Plugins', String(plugins.length)), valueRow('Folder', 'files/phoenix-js')]),
      ];
    },
  };

  // One plugin: Enabled, its settings, Reload, Remove, Info.
  const pluginPage = (id) => ({
    title: (PL.list().find((x) => x.id === id) || { name: id }).name,
    render: (page) => {
      const p = PL.list().find((x) => x.id === id);
      if (!p) return [section('', [textLine('This plugin was removed.', COLOR.muted, false)], false)];
      const settingRows = PL.settings(id).map((st) => settingRow(id, st));
      const origin = p.origin === 'app' ? 'Came with the app' : p.origin === 'file' ? 'Added to files/phoenix-js' : p.origin;
      const enabledState = () => { const q = PL.list().find((x) => x.id === id); return q && { on: q.enabled, subtitle: q.error ? `Failed: ${q.error}` : '' }; };
      return [
        ...(p.description ? [section('', [textLine(p.description, COLOR.muted, false)], false)] : []),
        section('', [switchRow({ key: `enabled-${id}`, title: 'Enabled', subtitle: p.error ? `Failed: ${p.error}` : '', on: p.enabled, icon: null },
          enabledState, (sw) => { togglePlugin(id)(sw); rerender(page); rerenderMain(); })]),
        ...(settingRows.length ? [section('Settings', settingRows)] : p.enabled ? [] : [section('', [textLine('Turn it on to see its settings.', COLOR.muted, false)], false)]),
        section('', [
          buttonRow('Reload', ICON.reload, () => { PL.reloadOne(id); rerender(page); rerenderMain(); }, { key: `reload-${id}` }),
          buttonRow('Remove', ICON.remove, () => { PL.remove(id); closeTop(); rerenderMain(); }, { danger: true, key: `remove-${id}` }),
        ]),
        section('Info', [valueRow('ID', id), valueRow('Source', origin)]),
      ];
    },
  });
  // A plugin setting row by type: switch, text / number (Enter or leaving the field saves), button.
  const settingRow = (id, st) => {
    if (st.type === 'button') return buttonRow(st.title, ICON.plugin, () => PL.pressSetting(id, st.key), { key: `setting-${id}-${st.key}` });
    if (st.type === 'text' || st.type === 'number') {
      const save = (text, h) => {
        const v = st.type === 'number' ? Number(text) : text;
        if (st.type === 'number' && (text.trim() === '' || Number.isNaN(v))) return;
        PL.setSetting(id, st.key, v);
        h.saved = text;
      };
      const r = inputRow(st.description || st.title, ICON.plugin, `setting-${id}-${st.key}`, (text, h) => save(text, h));
      const input = r.kids[0].kids[0].kids[1], h = input.h;
      h.text = h.saved = st.value == null ? '' : String(st.value);
      setProp(input.props, 'value', str(h.text));
      if (st.type === 'number') setProp(input.props, 'keyboard', str('numeric'));
      h.blur = () => { if (h.text !== h.saved) save(h.text, h); };
      const wrap = { type: 'view', props: clone(T.section.props), kids: [textLine(st.title, COLOR.text, false), r] };
      setProp(wrap.props, 'padding', { t: 'rect', v: [12, 12, 12, 12] });
      setProp(wrap.props, 'gap', num(6));
      return wrap;
    }
    return switchRow({ key: `setting-${id}-${st.key}`, title: st.title, subtitle: st.description, on: !!st.value, icon: null },
      () => { const cur = PL.settings(id).find((x) => x.key === st.key); return cur && { on: !!cur.value }; },
      (sw) => { const cur = PL.settings(id).find((x) => x.key === st.key); PL.setSetting(id, st.key, !(cur && cur.value)); refreshSwitch(sw); });
  };

  const INSTALL = {
    title: 'Install plugin',
    render: (page) => {
      const status = textLine('Install a plugin from a link or from a .js file on this device.', COLOR.muted, false);
      const done = (id) => { if (id) { setText(status, `Installed ${id}. It is on the Phoenix page now.`, COLOR.text); rerenderMain(); } else setText(status, 'No file chosen.', COLOR.muted); };
      const fail = (e) => setText(status, `Failed: ${e.message}`, COLOR.danger);
      return [
        section('From a URL', [inputRow('https://…/plugin.js', ICON.url, 'install-input', (url, h) => {
          setText(status, 'Installing…', COLOR.muted);
          PL.installUrl(url).then((id) => { done(id); clearInput(h); }, fail);
        })]),
        section('From a file', [buttonRow('Choose a file…', ICON.file, () => {
          setText(status, 'Choose a .js file…', COLOR.muted);
          PL.installFile().then(done, fail);
        }, { key: 'install-file' })]),
        section('', [status], false),
      ];
    },
  };

  const MAX_LINES = 150;
  const CONSOLE = {
    title: 'Console',
    render: (page) => {
      const lines = LOG.lines().slice(-MAX_LINES).reverse().map((l) => textLine(l.text, LEVEL_COLOR[l.level]));
      const box = section('Log (newest first)', lines, false);
      page.box = box.body;
      return [section('Run JavaScript', [inputRow('phoenix.native.arch', ICON.console, 'console-input', (code, h) => { if (code.trim()) { __phx_eval(code); clearInput(h); } })]), box];
    },
    onOpen: (page) => {
      const queue = [];
      let timer = 0;
      const flush = () => {
        timer = 0;
        if (!inst || !inst.stack.includes(page)) return;
        const ops = [], fresh = [];
        for (const l of queue.splice(0)) { const n = textLine(l.text, LEVEL_COLOR[l.level]); ops.push(...build(n, page.handlers)); fresh.unshift(n); }
        const kids = fresh.concat(page.box.kids), drop = kids.splice(MAX_LINES);
        page.box.kids = kids;
        const dropIds = drop.flatMap(all);
        page.ids = page.ids.concat(fresh.flatMap(all)).filter((id) => !dropIds.includes(id));
        send([...ops, { op: 'children', id: page.box.id, ids: kids.map((k) => k.id) }, ...(dropIds.length ? [{ op: 'delete', ids: dropIds }] : [])]);
      };
      page.unlisten = LOG.listen((l) => { queue.push(l); if (!timer) timer = setTimeout(flush, 150); });
    },
    onClose: (page) => page.unlisten && page.unlisten(),
  };

  // ---- main list entry ------------------------------------------------------------------------
  const buildEntry = () => {
    const handlers = new Map();
    const s = section('', [buttonRow('Phoenix', ICON.plugin, () => openPage(PHOENIX), { chevron: true, key: 'category' })]);
    const ops = build(s, handlers);
    return { ops, ids: all(s), root: s.id, handlers };
  };
  const handlerFor = (node) => {
    if (!inst) return null;
    for (const p of inst.stack) { const h = p.handlers.get(node); if (h) return h; }
    return (inst.entry && inst.entry.handlers.get(node)) || null;
  };

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
      let nav, x = list;
      while (x !== undefined) {
        if (navs.has(x)) { nav = x; break; }
        if (creates.get(x)?.props.some((p) => p.k === 'presentation' || p.k === 'uikitPush')) { nav = parent.get(x); break; }
        x = parent.get(x);
      }
      const hdr = [...creates.values()].find((c) => c.props.some((p) => p.k === 'stackHeader'));
      inst = { list, rust: null, search: null, nav, navRust: null, hdr: hdr && hdr.props, entry: null, stack: [] };
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
        if (inst.stack.length) { out.push({ op: 'children', id: inst.nav, ids: [...inst.navRust, ...inst.stack.map((p) => p.root)] }); changed = true; }
        else out.push(o);
      } else if (o.op === 'delete' && (o.ids.includes(inst.list) || o.ids.includes(inst.nav))) {
        // Settings closed; Java does not delete recursively
        for (const p of inst.stack) p.def.onClose?.(p);
        out.push({ op: 'delete', ids: o.ids.concat(inst.entry?.ids || [], inst.stack.flatMap((p) => p.ids)) });
        inst = null;
        changed = true;
        out.push(...ops.slice(i + 1));
        break;
      } else out.push(o);
    }
    return changed ? UI.encode(out) : undefined;
  });

  // Events on our nodes, and back navigation away from our pages, go to us instead of Rust.
  const payloadText = (p) => p && p.t === 'obj' && (p.v.find((x) => x.k === 'text') || {}).v?.v;
  phoenix.bridge.onDispatch((u8) => {
    if (!inst) return;
    let items;
    try { items = UI.decodeDispatch(u8); } catch { return; }
    const mine = items.filter((it) => it.kind === 'view' && (handlerFor(it.node) || (inst.stack.length && it.node === inst.nav && it.event === 'navigate')));
    if (!mine.length) return;
    // this runs on the main thread; UI ops must be sent from the JS thread
    for (const it of mine) {
      if (it.event === 'navigate') {
        const idx = (it.payload?.v?.find((x) => x.k === 'index') || {}).v?.v ?? pageIndex() - 1;
        setTimeout(() => { while (inst && inst.stack.length && pageIndex() > idx) closeTop(); }, 0);
        continue;
      }
      const h = handlerFor(it.node);
      if (it.event === 'changeText') { if (h.changeText) h.changeText(payloadText(it.payload) ?? ''); }
      else if (h[it.event]) setTimeout(() => h[it.event](), 0);
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
