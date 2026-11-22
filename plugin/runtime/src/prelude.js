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
globalThis.__phx_unhandled = (r) => logLine(3, 'Unhandled promise rejection: ' + show(r));
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
// Android's document picker (Java helper in libplg): resolves to {name, text}, or null if cancelled.
const picks = new Map(); let pickId = 0;
globalThis.__phx_picked = (id, name, text, error) => {
  const p = picks.get(id); if (!p) return; picks.delete(id);
  if (error) p.reject(new Error(error)); else p.resolve(text == null ? null : { name, text });
};
const pickFile = (mime = '*/*') => new Promise((resolve, reject) => {
  const id = ++pickId; picks.set(id, { resolve, reject });
  try { P.pickFile(id, mime); } catch (e) { picks.delete(id); reject(e); }
});

// ---- plugin API v1 ----------------------------------------------------------------------------
const API = 1;
// Work that must run on Phoenix's store thread (dispatch, store reads): now if we are already there
// (inside a phoenix.on listener), else just before Phoenix's next gateway event. Always a Promise.
const storeQ = [];
globalThis.__phx_storeWork = () => { for (const j of storeQ.splice(0)) { try { j.res(j.fn()); } catch (e) { j.rej(e); } } };
const onStore = (fn) => {
  if (P.inHandler()) { try { return Promise.resolve(fn()); } catch (e) { return Promise.reject(e); } }
  return new Promise((res, rej) => { storeQ.push({ fn, res, rej }); P.wantStore(); });
};
const parseOrNull = (s) => (s == null ? null : JSON.parse(s));
const dispatchNow = (n, data) => P.dispatch(n, typeof data === 'string' ? data : JSON.stringify(data));
const confirms = new Map(); let confirmId = 0;
globalThis.__phx_confirmed = (id, ok) => { const r = confirms.get(id); if (r) { confirms.delete(id); r(ok); } };
// Android UI needs Phoenix's activity, known once its first screen is up: calls made earlier wait.
const uiReady = () => new Promise((res) => { const t = () => (P.uiReady() ? res() : setTimeout(t, 250)); t(); });
const ui = {
  toast: (text, o = {}) => { uiReady().then(() => P.toast(String(text), !!o.long)).catch((e) => console.error('toast:', e)); },
  confirm: (title, message, o = {}) => uiReady().then(() => new Promise((res) => {
    const id = ++confirmId; confirms.set(id, res);
    try { P.confirm(id, String(title), String(message ?? ''), o.ok || 'OK', o.cancel || 'Cancel'); } catch (e) { confirms.delete(id); res(false); console.error('confirm:', e); }
  })),
  pickFile: (mime) => uiReady().then(() => pickFile(mime)),
};
// Local-only message changes: what this device shows; nothing is sent to Discord.
const snowflake = () => String((BigInt(Date.now()) - 1420070400000n) << 22n);
const messages = {
  addLocal: (channelId, content, o = {}) => {
    const id = snowflake(), who = o.author || 'Phoenix';
    const msg = { id, nonce: id, channel_id: String(channelId), ...(o.guildId ? { guild_id: String(o.guildId) } : {}), content: String(content), type: 0, flags: 64,
      tts: false, mention_everyone: false, mentions: [], mention_roles: [], attachments: [], embeds: o.embeds || [], components: [], pinned: false,
      timestamp: new Date().toISOString(), edited_timestamp: null,
      author: { id: '0', username: who, global_name: who, discriminator: '0000', avatar: null, bot: true, public_flags: 0 } };
    return onStore(() => dispatchNow('MESSAGE_CREATE', msg)).then(() => id);
  },
  updateLocal: (message) => onStore(() => dispatchNow('MESSAGE_UPDATE', message)),
  deleteLocal: (channelId, id) => onStore(() => dispatchNow('MESSAGE_DELETE', { id: String(id), channel_id: String(channelId) })),
};
// The value of a top-level key of a JSON object, without parsing the rest (READY is megabytes).
const jsonField = (raw, key) => {
  let depth = 0;
  for (let i = 0; i < raw.length; i++) {
    const c = raw[i];
    if (c === '{' || c === '[') { depth++; continue; }
    if (c === '}' || c === ']') { depth--; continue; }
    if (c !== '"') continue;
    let e = i + 1;
    while (e < raw.length && raw[e] !== '"') e += raw[e] === '\\' ? 2 : 1;
    const k = raw.slice(i + 1, e);
    let j = e + 1;
    while (raw[j] === ' ') j++;
    if (depth !== 1 || raw[j] !== ':' || k !== key) { i = e; continue; }
    j++;
    while (raw[j] === ' ') j++;
    let d = 0, inStr = false, end = j;
    for (; end < raw.length; end++) {
      const x = raw[end];
      if (inStr) { if (x === '\\') end++; else if (x === '"') { inStr = false; if (d === 0) { end++; break; } } continue; }
      if (x === '"') inStr = true;
      else if (x === '{' || x === '[') d++;
      else if (x === '}' || x === ']') { d--; if (d === 0) { end++; break; } }
      else if ((x === ',' || x === '}') && d === 0) break;
    }
    return JSON.parse(raw.slice(j, end));
  }
  return undefined;
};
// The signed-in user from READY (kept up to date by USER_UPDATE), for builds where the patcher found no Me store.
let meCache = null;
add(async, 1, 'READY', (ev) => { try { meCache = jsonField(ev.raw, 'user') ?? meCache; } catch (e) { console.warn('READY user:', e.message); } });
add(async, 1, 'USER_UPDATE', (ev) => { try { if (meCache && ev.data.id === meCache.id) meCache = { ...meCache, ...ev.data }; } catch {} });
const meNow = () => { try { const s = P.me(); if (s != null) return JSON.parse(s); } catch {} return meCache; };
const meAny = async () => (await onStore(meNow)) ?? (await db.get('meta', 'me').catch(() => null));
// ---- stores: Phoenix's own database (files/store/<account>/cache.sqlite), opened read-only ----
// Key/value tables with JSON values; ids in keys are zero-padded to 20 digits, e.g. channels are
// "<guild>:<channel>", messages "<channel>:<message>".
let dbPath = null;
const pad = (id) => String(id).padStart(20, '0');
const TABLE = /^[a-z][a-z0-9_]{0,63}$/;
const dbFile = () => {
  if (dbPath) return dbPath;
  const dir = P.dbDataDir();
  if (!dir) throw new Error('Phoenix is not up yet');
  const files = JSON.parse(P.dbFiles(dir + '/files/store')).map(([f]) => f).filter((f) => f.endsWith('/cache.sqlite'));
  const me = meCache && meCache.id;
  // ponytail: one Phoenix account per install in practice; with several, the READY user (if seen) picks
  dbPath = files.find((f) => me && f.includes(`/${me}/`)) || files[0] || null;
  if (!dbPath) throw new Error("Phoenix's database was not found");
  return dbPath;
};
const sql = async (query, args = [], limit = 1000) => {
  await uiReady();
  const r = JSON.parse(P.dbQuery(dbFile(), query, args.map(String), limit));
  if (r.error) throw new Error(r.error);
  return r;
};
const val = (v) => { if (typeof v !== 'string') return v; try { return JSON.parse(v); } catch { return v; } };
const table = (t) => { if (!TABLE.test(t)) throw new Error(`bad table name "${t}"`); return `"${t}"`; };
const db = {
  tables: async () => (await sql("SELECT name FROM sqlite_master WHERE type = 'table' AND name != 'android_metadata' ORDER BY name")).rows.map((r) => r[0]),
  get: async (t, key) => { const r = await sql(`SELECT v FROM ${table(t)} WHERE k = ?`, [key], 1); return r.rows.length ? val(r.rows[0][0]) : null; },
  /** [{key, value}] ordered by key: {prefix, after, before, reverse, limit (100, max 5000)} */
  scan: async (t, o = {}) => {
    const where = [], args = [];
    if (o.prefix != null) { where.push('k >= ? AND k < ?'); args.push(o.prefix, o.prefix + '￿'); }
    if (o.after != null) { where.push('k > ?'); args.push(o.after); }
    if (o.before != null) { where.push('k < ?'); args.push(o.before); }
    const n = Math.max(1, Math.min(o.limit ?? 100, 5000));
    const r = await sql(`SELECT k, v FROM ${table(t)}${where.length ? ' WHERE ' + where.join(' AND ') : ''} ORDER BY k ${o.reverse ? 'DESC' : 'ASC'} LIMIT ${n}`, args, n);
    return r.rows.map(([k, v]) => ({ key: k, value: val(v) }));
  },
  count: async (t, o = {}) => {
    const r = o.prefix != null ? await sql(`SELECT count(*) FROM ${table(t)} WHERE k >= ? AND k < ?`, [o.prefix, o.prefix + '￿'])
      : await sql(`SELECT count(*) FROM ${table(t)}`);
    return r.rows[0][0];
  },
  /** Read-only SQL: {cols, rows, more}. */
  sql: (query, args = [], limit = 1000) => sql(query, args, limit),
};
const values = (rows) => rows.map((r) => r.value);
const typed = {
  user: (id) => db.get('users', pad(id)),
  users: (o = {}) => db.scan('users', { limit: o.limit ?? 1000 }).then(values),
  channel: async (id) => { const r = await sql('SELECT v FROM channels WHERE k LIKE ? LIMIT 1', ['%:' + pad(id)], 1); return r.rows.length ? val(r.rows[0][0]) : null; },
  /** A guild's channels; guildId 0 or null for DMs and group DMs. */
  channels: (guildId, o = {}) => db.scan('channels', { prefix: pad(guildId || 0) + ':', limit: o.limit ?? 1000 }).then(values),
  privateChannels: (o = {}) => db.scan('channels', { prefix: pad(0) + ':', limit: o.limit ?? 1000 }).then(values),
  /** Newest first: {limit (50), before: messageId} */
  messages: (channelId, o = {}) => db.scan('messages', { prefix: pad(channelId) + ':', reverse: true, limit: o.limit ?? 50,
    ...(o.before != null ? { before: pad(channelId) + ':' + pad(o.before) } : {}) }).then(values),
  message: (channelId, id) => db.get('messages', pad(channelId) + ':' + pad(id)),
  readState: (channelId) => db.get('read_states', pad(channelId)),
  relationships: () => db.scan('relationships', { limit: 5000 }).then(values),
  /** Guilds as Phoenix stores them: {id, properties (name, icon, ...), roles, emojis, stickers, member_count, joined_at_ms}. */
  guilds: (o = {}) => db.scan('guilds', { limit: o.limit ?? 5000 }).then((rows) => rows.map(({ key, value }) => ({ id: String(BigInt(key)), ...value }))),
  guild: async (id) => { const v = await db.get('guilds', pad(id)); return v && { id: String(id), ...v }; },
  /** Members Phoenix has seen in a guild (not the whole member list). */
  members: (guildId, o = {}) => db.scan('members', { prefix: pad(guildId) + ':', limit: o.limit ?? 1000 }).then(values),
  member: (guildId, userId) => db.get('members', pad(guildId) + ':' + pad(userId)),
  /** Your notification settings for a guild. */
  guildSettings: (guildId) => db.get('user_guild_settings', pad(guildId)),
  /** A key of Phoenix's meta table: me, guild_order, experiments, my_members, selected, ... */
  meta: (key) => db.get('meta', key),
  /** A persisted domain: profiles, threads, scheduled_events, collectibles, connections, user_affinities, ... */
  domain: (name) => db.get('meta', 'domain.' + name),
};

// ready: at READY, or at the first gateway event if plugins started after Phoenix had handled it
let readyResolve; const ready = new Promise((r) => { readyResolve = r; });
add(async, 1, 'READY', () => readyResolve());
// Phoenix applies READY on its own path on some builds: the first gateway event means it is connected
onStore(() => readyResolve());

// Plugin settings: define({key: {type: 'switch'|'text'|'number'|'button', title, description?,
// default?, onPress?}}); values persist in storage.json and show on the plugin's Settings page.
const settingDefs = new Map(), settingListeners = new Map();
const settingValue = (id, key) => {
  const v = ((store[id] || {}).$settings || {})[key];
  return v !== undefined ? v : (settingDefs.get(id) || {})[key]?.default;
};
const setSetting = (id, key, value) => {
  const s = ((store[id] ||= {}).$settings ||= {});
  if (s[key] === value) return;
  s[key] = value;
  P.writeFile('storage.json', JSON.stringify(store));
  for (const f of settingListeners.get(id) || []) try { f(key, value); } catch (e) { console.error(`[${id}] settings.onChange:`, e); }
};
const settingsFor = (id) => ({
  define: (defs) => { settingDefs.set(id, defs || {}); },
  get: (key) => settingValue(id, key),
  set: (key, value) => setSetting(id, key, value),
  /** fn(key, value) after a setting changes; removed when the plugin is turned off. */
  onChange: (fn) => {
    if (!settingListeners.has(id)) settingListeners.set(id, new Set());
    settingListeners.get(id).add(fn);
    return track(id, () => settingListeners.get(id)?.delete(fn));
  },
});
const track = (id, unsub) => { const p = plugins.get(id); if (p) p.unsubs.push(unsub); return unsub; };
const api = (id) => ({ id,
  bridge: { onOps: (fn) => track(id, brOn(0, id, fn)), onInvoke: (fn) => track(id, brOn(1, id, fn)),
    onInvokeResult: (fn) => track(id, brOn(2, id, fn)), onDispatch: (fn) => track(id, brOn(3, id, fn)) },
  on: (n, fn) => { fn.__plugin = id; return track(id, add(sync, 0, n, fn)); },
  observe: (n, fn) => { fn.__plugin = id; return track(id, add(async, 1, n, fn)); },
  /** Runs when the plugin is turned off in Settings (undo timers, Java hooks, ...). */
  onUnload: (fn) => { const p = plugins.get(id); if (p) p.unload.push(fn); },
  /** Runs an event through Phoenix's own handler, on its store thread. Resolves to whether it was accepted. */
  dispatch: (n, data) => onStore(() => dispatchNow(n, data)),
  fetch: fetchImpl, storage: storageFor(id), events: [...names],
  // Live Phoenix stores via Phoenix's own serializers, read on its store thread (Promises).
  stores: { ...db, ...typed, me: meAny,
    raw: (ser, off) => onStore(() => parseOrNull(P.storeJson(ser, off))),
    /** fn({me, raw}) on the store thread, with synchronous readers. */
    read: (fn) => onStore(() => fn({ me: meNow, raw: (ser, off) => parseOrNull(P.storeJson(ser, off)) })) },
  ui, messages, ready,
  version: { api: API, runtime: 'rquickjs (QuickJS-ng)', arch: N.arch, inlineHooks: N.inlineHooks },
  /** What this plugin offers other plugins (phoenix.plugins.get(id)). */
  exports: {},
  plugins: {
    get: (other) => { const q = plugins.get(other); return q && q.enabled && !q.error ? q.exports : undefined; },
    list: () => __phx_plugins.list(),
  },
  native: nativeFor(id),
  settings: settingsFor(id) });
globalThis.phoenix = api('global');

// "// Name: what it does" on the first line names the plugin in Settings.
// Plugin header: leading "// @name Foo", "// @description ...", "// @version 1.0", "// @author ...",
// "// @api 1". The older first line "// Name: what it does" still works.
const meta = (id, src) => {
  const m = { name: id, description: '', version: '', author: '', api: 1 };
  const head = /^(?:[ \t]*\/\/[^\n]*\n?)*/.exec(src)[0];
  let tagged = false;
  for (const line of head.split('\n')) {
    const t = /^\s*\/\/\s*@(\w+)\s+(.*?)\s*$/.exec(line);
    if (t && t[1].toLowerCase() in m) { tagged = true; const k = t[1].toLowerCase(); m[k] = k === 'api' ? Number(t[2]) || 1 : t[2]; }
  }
  if (!tagged) { const o = /^\s*\/\/\s*([^:\n]{1,40}):\s*([^\n]*)/.exec(src); if (o) { m.name = o[1].trim(); m.description = o[2].trim(); } }
  return m;
};
const run = (p) => {
  if (p.api > API) { p.error = `needs plugin API ${p.api}; this app has ${API}`; console.warn(`plugin ${p.id}: ${p.error}`); return; }
  const fn = new Function('phoenix', 'console', p.source + `\n//# sourceURL=${p.id}.js`);
  const tag = (k) => (...a) => console[k](`[${p.id}]`, ...a);
  const a = api(p.id);
  p.exports = a.exports;
  try { fn(a, { log: tag('log'), info: tag('info'), warn: tag('warn'), error: tag('error'), debug: tag('debug') }); p.error = null; console.info(`plugin ${p.id} loaded`); }
  catch (e) { p.error = String(e); console.error(`plugin ${p.id} failed:`, e, e && e.stack); stop(p); }
};
const stop = (p) => {
  settingDefs.delete(p.id);
  for (const u of p.unsubs.splice(0)) try { u(); } catch {}
  for (const u of p.unload.splice(0)) try { u(); } catch (e) { console.error(`[${p.id}] onUnload:`, e); }
};
const load = (id, source) => {
  const p = { id, ...meta(id, source), source, enabled: !cfg.disabled.includes(id), error: null, unsubs: [], unload: [] };
  plugins.set(id, p);
  if (p.enabled) run(p); else console.info(`plugin ${id} is turned off`);
};
globalThis.__phx_plugins = {
  list: () => [...plugins.values()].map(({ id, name, description, version, author, enabled, error }) => ({ id, name, description, version, author, enabled, error,
    origin: cfg.origins?.[id] || (bundled.some((b) => b.id === id) ? 'app' : 'file') })),
  /** [{key, type, title, description, value}] from the plugin's settings.define() */
  settings: (id) => Object.entries(settingDefs.get(id) || {}).map(([key, d]) => ({ key, type: d.type || 'switch', title: d.title || key, description: d.description || '', value: settingValue(id, key) })),
  setSetting,
  pressSetting: (id, key) => { const d = (settingDefs.get(id) || {})[key]; if (d && d.onPress) try { d.onPress(); } catch (e) { console.error(`[${id}] ${key}:`, e); } },
  /** The plugin's current source (what Edit code shows). */
  source: (id) => plugins.get(id)?.source ?? null,
  /** Saves edited code: checked first, then written and restarted like an install. */
  saveSource: (id, src) => {
    try { new Function(src); } catch (e) { throw new Error('not valid JavaScript: ' + e.message); }
    const err = __phx_plugins.install(id, src);
    if (err) throw new Error(err);
  },
  /** Re-reads one plugin's file and restarts it if enabled. */
  reloadOne: (id) => {
    const p = plugins.get(id);
    if (!p) return;
    const src = P.readFile(id + '.js');
    if (src != null && src !== p.source) Object.assign(p, meta(id, src), { source: src });
    if (p.enabled) { stop(p); run(p); }
  },
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
  /** Lets the user pick a .js file and installs it (id: file name). Resolves to the id, or null if cancelled. */
  installFile: async () => {
    const f = await pickFile();
    if (!f) return null;
    const id = f.name.replace(/\.m?js$/i, '').replace(/[^\w-]+/g, '-').slice(0, 40) || 'plugin';
    try { new Function(f.text); } catch (e) { throw new Error(`${f.name} is not valid JavaScript: ${e.message}`); }
    (cfg.origins ||= {})[id] = `File: ${f.name}`;
    const err = __phx_plugins.install(id, f.text);
    if (err) throw new Error(err);
    return id;
  },
  /** Fetches a plugin from a URL and installs it (id: file name). Resolves to the id. */
  installUrl: async (url) => {
    const m = /([\w-]+)(\.m?js)?(?:[?#].*)?$/i.exec(url.trim());
    if (!/^https?:\/\//i.test(url.trim()) || !m) throw new Error('enter an http(s) URL of a .js file');
    const r = await fetchImpl(url.trim());
    if (!r.ok) throw new Error('HTTP ' + r.status);
    const src = await r.text();
    try { new Function(src); } catch (e) { throw new Error('not valid JavaScript: ' + e.message); }
    (cfg.origins ||= {})[m[1]] = url.trim();
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
