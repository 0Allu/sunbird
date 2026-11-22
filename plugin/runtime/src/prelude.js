'use strict';
// JS side of the plugin API (global `phoenix`); natives live on __phx (see lib.rs).
(() => {
const P = __phx, names = P.events;
// Values as a console shows them: strings raw, errors with their stack, objects as JSON.
const show = (v) => {
  if (typeof v === 'string') return v;
  if (v instanceof Error) return v.stack ? `${v}\n${v.stack}` : String(v);
  if (typeof v === 'bigint') return v + 'n';
  if (typeof v === 'function') return `[function ${v.name || 'anonymous'}]`;
  if (v instanceof Uint8Array) return `Uint8Array(${v.length}) [${Array.from(v.subarray(0, 32), (b) => b.toString(16).padStart(2, '0')).join(' ')}${v.length > 32 ? ' …' : ''}]`;
  if (v && typeof v === 'object') { try { return JSON.stringify(v, (k, x) => (typeof x === 'bigint' ? x + 'n' : x), 1) ?? String(v); } catch { return String(v); } }
  return String(v);
};
const fmt = (a) => a.map(show).join(' ');
// Last 500 lines for Settings > Phoenix > Console: {t, level (0 log 1 info 2 warn 3 error 4 debug), text}
const LOG = { lines: [], listeners: new Set() };
const logLine = (level, text) => {
  P.log(level, text);
  const l = { t: Date.now(), level, text };
  LOG.lines.push(l);
  if (LOG.lines.length > 500) LOG.lines.shift();
  for (const f of LOG.listeners) try { f(l); } catch {}
};
globalThis.console = Object.fromEntries(['log', 'info', 'warn', 'error', 'debug'].map((k, i) => [k, (...a) => logLine(i, fmt(a))]));
globalThis.__phx_log = { lines: () => LOG.lines.slice(), listen: (f) => { LOG.listeners.add(f); return () => LOG.listeners.delete(f); } };
// The console prompt: global eval; promises are awaited.
globalThis.__phx_eval = (code) => {
  logLine(0, '> ' + code);
  try {
    const r = (0, eval)(code);
    if (r && typeof r.then === 'function') r.then((v) => logLine(0, '< ' + show(v)), (e) => logLine(3, '< ' + show(e)));
    else logLine(0, '< ' + show(r));
  } catch (e) { logLine(3, show(e)); }
};

const sync = new Map(), async = new Map();
const add = (m, kind, name, fn) => { if (!m.has(name)) m.set(name, []); m.get(name).push(fn); P.listen(name, kind, 1);
  return () => { const a = m.get(name) || []; const i = a.indexOf(fn); if (i >= 0) { a.splice(i, 1); P.listen(name, kind, -1); } }; };
const mkev = (name, raw) => { let d; return { name, raw, get data() { return d === undefined ? (d = JSON.parse(raw)) : d; } }; };
globalThis.__phx_emit = (name, raw) => {
  const ev = mkev(name, raw); let out;
  for (const fn of sync.get(name) || []) {
    try { const r = fn(ev); if (r === false) return false; if (r !== undefined) { out = typeof r === 'string' ? r : JSON.stringify(r); ev.raw = out; } }
    catch (e) { console.error(`[${fn.__plugin || '?'}] ${name}:`, e, e && e.stack); }
  }
  return out;
};
globalThis.__phx_observe = (idx, raw) => { const name = names[idx], ev = mkev(name, raw);
  for (const fn of async.get(name) || []) { try { fn(ev); } catch (e) { console.error(`[${fn.__plugin || '?'}] ${name}:`, e, e && e.stack); } } };

const timers = new Map(); let tid = 0;
globalThis.setTimeout = (fn, ms = 0, ...a) => { const id = ++tid; timers.set(id, { at: P.now() + ms, fn, a }); P.wake(); return id; };
globalThis.setInterval = (fn, ms = 0, ...a) => { const id = ++tid; timers.set(id, { at: P.now() + ms, fn, a, every: Math.max(ms, 10) }); P.wake(); return id; };
globalThis.clearTimeout = globalThis.clearInterval = (id) => timers.delete(id);
globalThis.__phx_tick = () => { let next = -1; const t = P.now();
  for (const [id, x] of [...timers]) { if (x.at <= t) { if (x.every) x.at = t + x.every; else timers.delete(id);
      try { x.fn(...x.a); } catch (e) { console.error('timer:', e, e && e.stack); } } }
  for (const x of timers.values()) { const d = Math.max(0, x.at - P.now()); next = next < 0 ? d : Math.min(next, d); }
  return next; };

const pending = new Map();
globalThis.__phx_fetched = (id, text, status) => { const p = pending.get(id); if (!p) return; pending.delete(id);
  status < 0 ? p.reject(new Error('fetch failed')) : p.resolve({ status, ok: status >= 200 && status < 300, text: () => Promise.resolve(text ?? ''), json: () => Promise.resolve(JSON.parse(text ?? 'null')) }); };
const fetchImpl = (url, o = {}) => new Promise((resolve, reject) => {
  const h = Object.entries(o.headers || {}).map(([k, v]) => `${k}: ${v}`).join('\n');
  const id = P.fetch(String(url), o.method || 'GET', h, o.body == null ? null : String(o.body));
  if (id < 0) reject(new Error('fetch could not start')); else pending.set(id, { resolve, reject }); });

let store = {}; try { store = JSON.parse(P.readFile('storage.json') || '{}'); } catch {}
const storageFor = (id) => ({ get: (k, d) => (store[id] || {})[k] ?? d,
  set: (k, v) => { (store[id] ||= {})[k] = v; P.writeFile('storage.json', JSON.stringify(store)); },
  delete: (k) => { if (store[id]) { delete store[id][k]; P.writeFile('storage.json', JSON.stringify(store)); } } });

const BR = [[], [], [], []];
globalThis.__phx_bridge = (k, a, b) => { let cur = k === 2 ? b : a, changed = false;
  for (const fn of BR[k]) { try { const r = k === 2 ? fn(a, cur) : fn(cur); if (r === false && k !== 2) return false;
      if (r instanceof Uint8Array) { cur = r; changed = true; } } catch (e) { console.error(`[${fn.__plugin || '?'}] bridge:`, e, e && e.stack); } }
  return changed ? cur : undefined; };
const brOn = (k, id, fn) => { fn.__plugin = id; BR[k].push(fn); P.bridgeListen(k, 1);
  return () => { const i = BR[k].indexOf(fn); if (i >= 0) { BR[k].splice(i, 1); P.bridgeListen(k, -1); } }; };

// Inline hooks: ShadowHook calls __phx_intercept(id, x0..x30, sp, pc) on the hooking thread;
// the callback may change ctx.x[i] (arguments at entry, x0 = result at a ret).
const N = P.native, ICB = new Map(); let icbId = 0;
globalThis.__phx_intercept = (id, regs, sp, pc) => {
  const h = ICB.get(id);
  if (!h) return;
  const ctx = { x: regs.slice(), sp, pc };
  try { h.fn(ctx); } catch (e) { console.error(`[${h.plugin}] intercept:`, e, e && e.stack); return; }
  return ctx.x.some((v, i) => v !== regs[i]) ? ctx.x : undefined;
};
const nativeFor = (id) => ({
  base: N.base, arch: N.arch, inlineHooks: N.inlineHooks,
  /** host-lib offset (e.g. from scan) -> address */
  at: (off) => N.base + BigInt(off),
  scan: (pattern) => P.scan(pattern), sym: N.sym, moduleBase: N.moduleBase,
  read: N.read, write: N.write, call: N.call,
  readU64: (a) => new DataView(N.read(a, 8).buffer).getBigUint64(0, true),
  readCString: (a, max = 4096) => { const b = N.read(a, max); const n = b.indexOf(0); return String.fromCharCode(...b.subarray(0, n < 0 ? b.length : n)); },
  /** fn(ctx) before the function at `addr` runs (or before one instruction with {instruction: true}) */
  intercept: (addr, fn, opts = {}) => {
    const k = ++icbId;
    ICB.set(k, { fn, plugin: id });
    let stub;
    try { stub = N.intercept(addr, k, !opts.instruction); } catch (e) { ICB.delete(k); throw e; }
    return track(id, () => { if (ICB.delete(k)) N.unintercept(stub); });
  },
});

// Plugins: id -> { id, name, description, source, enabled, error, unsubs, unload }
const plugins = new Map();
let cfg = { disabled: [], removed: [] }; try { cfg = { disabled: [], removed: [], ...JSON.parse(P.readFile('plugins.json') || '{}') }; } catch {}
const saveCfg = () => P.writeFile('plugins.json', JSON.stringify(cfg));
let bundled = []; // the APK's plugin bundle ([{id, source?, url?}])
const track = (id, unsub) => { const p = plugins.get(id); if (p) p.unsubs.push(unsub); return unsub; };
const api = (id) => ({ id,
  bridge: { onOps: (fn) => track(id, brOn(0, id, fn)), onInvoke: (fn) => track(id, brOn(1, id, fn)),
    onInvokeResult: (fn) => track(id, brOn(2, id, fn)), onDispatch: (fn) => track(id, brOn(3, id, fn)) },
  on: (n, fn) => { fn.__plugin = id; return track(id, add(sync, 0, n, fn)); },
  observe: (n, fn) => { fn.__plugin = id; return track(id, add(async, 1, n, fn)); },
  /** Runs when the plugin is turned off in Settings (undo timers, Java hooks, ...). */
  onUnload: (fn) => { const p = plugins.get(id); if (p) p.unload.push(fn); },
  dispatch: (n, data) => P.dispatch(n, typeof data === 'string' ? data : JSON.stringify(data)),
  fetch: fetchImpl, storage: storageFor(id), events: [...names],
  // Live Phoenix stores via Phoenix's own serializers; store-thread only (inside phoenix.on).
  stores: { me: () => { const s = P.me(); return s == null ? null : JSON.parse(s); },
    raw: (ser, off) => { const s = P.storeJson(ser, off); return s == null ? null : JSON.parse(s); } },
  native: nativeFor(id) });
globalThis.phoenix = api('global');

// "// Name: what it does" on the first line names the plugin in Settings.
const meta = (id, src) => { const m = /^\s*\/\/\s*([^:\n]{1,40}):\s*([^\n]*)/.exec(src); return m ? { name: m[1].trim(), description: m[2].trim() } : { name: id, description: '' }; };
const run = (p) => {
  const fn = new Function('phoenix', 'console', p.source + `\n//# sourceURL=${p.id}.js`);
  const tag = (k) => (...a) => console[k](`[${p.id}]`, ...a);
  try { fn(api(p.id), { log: tag('log'), info: tag('info'), warn: tag('warn'), error: tag('error'), debug: tag('debug') }); p.error = null; console.info(`plugin ${p.id} loaded`); }
  catch (e) { p.error = String(e); console.error(`plugin ${p.id} failed:`, e, e && e.stack); stop(p); }
};
const stop = (p) => {
  for (const u of p.unsubs.splice(0)) try { u(); } catch {}
  for (const u of p.unload.splice(0)) try { u(); } catch (e) { console.error(`[${p.id}] onUnload:`, e); }
};
const load = (id, source) => {
  const p = { id, ...meta(id, source), source, enabled: !cfg.disabled.includes(id), error: null, unsubs: [], unload: [] };
  plugins.set(id, p);
  if (p.enabled) run(p); else console.info(`plugin ${id} is turned off`);
};
globalThis.__phx_plugins = {
  list: () => [...plugins.values()].map(({ id, name, description, enabled, error }) => ({ id, name, description, enabled, error })),
  setEnabled: (id, on) => {
    const p = plugins.get(id);
    if (!p || p.enabled === on) return;
    p.enabled = on;
    cfg.disabled = cfg.disabled.filter((x) => x !== id).concat(on ? [] : [id]);
    saveCfg();
    if (on) run(p); else { stop(p); console.info(`plugin ${id} turned off`); }
  },
  /** Re-reads every plugin from files/phoenix-js and restarts the enabled ones. */
  reload: () => {
    for (const p of plugins.values()) {
      const src = P.readFile(p.id + '.js');
      if (src != null && src !== p.source) Object.assign(p, meta(p.id, src), { source: src });
      if (p.enabled) { stop(p); run(p); }
    }
    console.info(`plugins reloaded: ${plugins.size}`);
  },
  /** Saves `source` as <id>.js and (re)starts it. */
  install: (id, source) => {
    if (!/^[\w-]{1,40}$/.test(id)) throw new Error(`bad plugin id "${id}"`);
    if (!P.writeFile(id + '.js', source)) throw new Error(`could not save ${id}.js`);
    const old = plugins.get(id);
    if (old) stop(old);
    cfg.removed = cfg.removed.filter((x) => x !== id);
    cfg.disabled = cfg.disabled.filter((x) => x !== id);
    saveCfg();
    load(id, source);
    console.info(`plugin ${id} installed`);
    return plugins.get(id).error;
  },
  /** Stops it and deletes its file; a plugin shipped in the APK stays removed. */
  remove: (id) => {
    const p = plugins.get(id);
    if (!p) return;
    stop(p);
    plugins.delete(id);
    P.removeFile(id + '.js');
    if (!cfg.removed.includes(id)) cfg.removed.push(id);
    saveCfg();
    console.info(`plugin ${id} removed`);
  },
  /** Plugins shipped in the APK that were removed: [{id, name}] */
  removedBundled: () => bundled.filter((b) => b.source && cfg.removed.includes(b.id)).map((b) => ({ id: b.id, name: meta(b.id, b.source).name })),
  /** Brings back a removed plugin that came with the app. */
  restore: (id) => { const b = bundled.find((x) => x.id === id && x.source); if (b) __phx_plugins.install(id, b.source); },
  /** Fetches a plugin from a URL and installs it (id: file name). Resolves to the id. */
  installUrl: async (url) => {
    const m = /([\w-]+)(\.m?js)?(?:[?#].*)?$/i.exec(url.trim());
    if (!/^https?:\/\//i.test(url.trim()) || !m) throw new Error('enter an http(s) URL of a .js file');
    const r = await fetchImpl(url.trim());
    if (!r.ok) throw new Error('HTTP ' + r.status);
    const src = await r.text();
    try { new Function(src); } catch (e) { throw new Error('not valid JavaScript: ' + e.message); }
    const err = __phx_plugins.install(m[1], src);
    if (err) throw new Error(err);
    return m[1];
  },
};

// Bundle entries with a source (shipped in the APK, so a reinstall updates them) are written to
// files/phoenix-js and win over older copies; then every other *.js there (sorted). url
// entries load their cached copy and are refreshed in the background.
globalThis.__phx_loadAll = (bundle) => { const seen = new Set();
  let b = []; try { b = JSON.parse(bundle || '[]'); } catch (e) { console.error('bad plugin bundle', e); }
  bundled = b;
  b = b.filter((p) => !cfg.removed.includes(p.id));
  for (const p of b) if (p.source) { if (P.readFile(p.id + '.js') !== p.source) P.writeFile(p.id + '.js', p.source); seen.add(p.id); load(p.id, p.source); }
  for (const f of P.listDir()) if (f.endsWith('.js')) { const id = f.slice(0, -3); if (!seen.has(id)) { seen.add(id); load(id, P.readFile(f)); } }
  for (const p of b) {
    if (p.url) fetchImpl(p.url).then(async (r) => { if (!r.ok) throw new Error('HTTP ' + r.status); const src = await r.text();
        if (src !== P.readFile(p.id + '.js')) { P.writeFile(p.id + '.js', src); console.info(`plugin ${p.id} updated from ${p.url}` + (seen.has(p.id) ? ' (applies on next start)' : ''));
          if (!seen.has(p.id)) { seen.add(p.id); load(p.id, src); } } })
      .catch((e) => console.warn(`plugin ${p.id}: update check failed:`, e.message)); }
};
})();
