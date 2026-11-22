import { PROXY as DEFAULT_PROXY, VERIFIED } from './config.js';
import { ABIS, abiOf, buildPatchedApk } from './lib/apk.js';
import { BOOTSTRAP_URL, bundleScripts, GADGET_URLS, gunzip } from './lib/frida.js';
import { PLUGINS } from './lib/plugins.js';
import { readZip } from './lib/zip.js';

const TRACKER = 'https://tracker.vendetta.rocks';
const PROXY = (new URLSearchParams(location.search).get('proxy') ?? DEFAULT_PROXY).replace(/\/$/, '');
const DENSITIES = [['ldpi', 120], ['mdpi', 160], ['hdpi', 240], ['xhdpi', 320], ['xxhdpi', 480], ['xxxhdpi', 640]];
const LANGS = ['en', 'ar', 'bg', 'cs', 'da', 'de', 'el', 'es', 'fi', 'fr', 'hi', 'hr', 'hu', 'id', 'it', 'ja', 'ko', 'lt', 'nl', 'no', 'pl', 'pt', 'ro', 'ru', 'sv', 'th', 'tr', 'uk', 'vi', 'zh'];
const $ = (id) => document.getElementById(id);

// --- setup ------------------------------------------------------------------------------
const dpi = Math.round((window.devicePixelRatio || 1) * 160);
const bestDensity = (DENSITIES.find(([, d]) => d >= dpi) || DENSITIES.at(-1))[0];
$('dpi').innerHTML = DENSITIES.map(([n, d]) => `<option value="${n}" ${n === bestDensity ? 'selected' : ''}>${n} (${d})</option>`).join('');
const myLang = (navigator.language || 'en').slice(0, 2).toLowerCase();
// x86_64 only where the browser runs on it (WSA, emulators, x86 Chromebooks); the APK is usually for this device
const looksX86 = (arch) => /Android|CrOS/.test(navigator.userAgent) && (arch === 'x86' || /x86|i686|Subsystem for Android/i.test(navigator.userAgent));
if (looksX86()) $('cpu').value = 'x86_64';
navigator.userAgentData?.getHighEntropyValues(['architecture']).then(({ architecture }) => { if (looksX86(architecture)) $('cpu').value = 'x86_64'; }).catch(() => {});
$('lang').innerHTML = ['none', ...LANGS].map((l) => `<option ${l === (LANGS.includes(myLang) ? myLang : 'en') ? 'selected' : ''}>${l}</option>`).join('');

function setVersions(latest) {
  const vs = [...new Set([...VERIFIED, ...(latest ? [latest] : [])])].sort((a, b) => b - a);
  $('ver').innerHTML = vs.map((v) => `<option value="${v}" ${v === VERIFIED[0] ? 'selected' : ''}>${v}${VERIFIED.includes(v) ? ' (verified)' : ' (latest alpha, untested)'}</option>`).join('');
}
setVersions();
fetch(`${TRACKER}/tracker/index`).then((r) => r.json()).then((j) => setVersions(j.latest?.alpha)).catch(() => {});

// --- one fixed signing key for every build, so each patched APK installs as an update -----
const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
async function identity() {
  const j = await (await fetch('signing-key.json')).json();
  return { pkcs8: unb64(j.pkcs8), spki: unb64(j.spki), cert: unb64(j.cert) };
}

// --- progress UI -------------------------------------------------------------------------
function step(label) {
  const li = document.createElement('li');
  li.innerHTML = `<span></span><span class="state">waiting</span><span class="bar"><i></i></span>`;
  li.firstChild.textContent = label;
  $('steps').append(li);
  const st = li.querySelector('.state'), bar = li.querySelector('i');
  return {
    progress(f, text) { bar.style.width = `${Math.round(f * 100)}%`; st.textContent = text ?? `${Math.round(f * 100)}%`; },
    done(text = 'done') { li.className = 'done'; bar.style.width = '100%'; st.textContent = text; },
    fail(text = 'failed') { li.className = 'fail'; st.textContent = text; },
  };
}
const log = (s) => { $('log').textContent += s + '\n'; $('log').scrollTop = 1e9; };
const mb = (n) => `${(n / 1048576).toFixed(1)} MB`;

// --- downloads ---------------------------------------------------------------------------
async function download(vc, split, optional) {
  const s = step(`Download ${split}`);
  const res = await fetch(`${PROXY}/tracker/download/${vc}/${split}`);
  if (!res.ok) {
    if (optional) { s.done('not available'); return null; }
    s.fail(`HTTP ${res.status}`); throw new Error(`${split}: HTTP ${res.status}`);
  }
  const total = +res.headers.get('Content-Length') || 0;
  const reader = res.body.getReader();
  let buf = new Uint8Array(total || 1 << 24), n = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    if (n + value.length > buf.length) { const b = new Uint8Array(Math.max(buf.length * 2, n + value.length)); b.set(buf.subarray(0, n)); buf = b; }
    buf.set(value, n); n += value.length;
    s.progress(total ? n / total : 0, total ? `${mb(n)} / ${mb(total)}` : mb(n));
  }
  s.done(mb(n));
  return buf.length === n ? buf : buf.slice(0, n);
}

function splitNames() {
  const lang = $('lang').value;
  return { abi: ABIS[$('cpu').value].split, density: `config.${$('dpi').value}`, lang: lang === 'none' ? null : `config.${lang}` };
}

async function patchAndOffer(vc, base, abi, configs, abiName) {
  const plugins = [...document.querySelectorAll('#plugins input:checked')].map((i) => i.value);
  const frida = plugins.includes('frida') ? await fridaInput(abiName) : null;
  const s = step('Patch and sign');
  s.progress(0.5, 'working…');
  const packageName = chosenPackage(), side = packageName !== 'com.discord';
  const pluginLib = plugins.length ? new Uint8Array(await (await fetch(ABIS[abiName].pluginLib)).arrayBuffer()) : null;
  const nothingUrl = plugins.length && ABIS[abiName].shadowhookNothing;
  const shadowhookNothing = nothingUrl ? new Uint8Array(await (await fetch(nothingUrl)).arrayBuffer()) : null;
  const pluginSources = Object.fromEntries(await Promise.all(PLUGINS.filter((p) => p.js && plugins.includes(p.id))
    .map(async (p) => [p.id, await (await fetch(p.js)).text()])));
  const userScripts = plugins.includes('jsPlugins') ? await jsInput() : [];
  const parts = await buildPatchedApk({ base, abi, configs, identity: await identity(), packageName, label: side ? 'Discord Phoenix' : null, plugins, pluginLib, shadowhookNothing, pluginSources, userScripts, frida, log });
  const blob = new Blob(parts, { type: 'application/vnd.android.package-archive' });
  s.done(mb(blob.size));
  $('note-side').hidden = !side; $('note-replace').hidden = side;
  const a = $('dl');
  a.href = URL.createObjectURL(blob);
  a.download = `discord-${vc}-phoenix-unlock${side ? '-side' : ''}.apk`;
  $('result').hidden = false;
  a.scrollIntoView({ behavior: 'smooth' });
}

function fail(e) {
  console.error(e);
  $('err').textContent = `Something went wrong: ${e.message || e}`;
  $('err').hidden = false;
  $('go').disabled = false;
}

// The tracker only mirrors some density splits per build (356207: hdpi and xxhdpi), so fall back
// to the nearest one it has. Any density works; Android scales the drawables.
async function downloadDensity(vc) {
  const want = DENSITIES.find(([n]) => n === $('dpi').value)[1];
  const order = [...DENSITIES].sort((a, b) => Math.abs(a[1] - want) - Math.abs(b[1] - want) || b[1] - a[1]);
  for (const [n] of order) {
    const buf = await download(vc, `config.${n}`, true);
    if (buf) return buf;
  }
  throw new Error('this build has no density split on the tracker');
}

// --- plugins ------------------------------------------------------------------------------
for (const p of PLUGINS) {
  const l = document.createElement('label');
  l.className = 'plugin';
  l.innerHTML = '<input type="checkbox"><b></b><span></span>';
  l.querySelector('input').value = p.id;
  l.querySelector('b').textContent = p.name;
  l.querySelector('span').textContent = p.description;
  $('plugins').append(l);
}

// JS plugins (rquickjs): picked .js files and the editor, each bundled as its own plugin
const jsBox = document.querySelector('#plugins input[value=jsPlugins]');
jsBox.onchange = () => { $('jsp').hidden = !jsBox.checked; };
const pluginId = (name) => name.replace(/\.js$/i, '').replace(/[^\w-]+/g, '-').slice(0, 40) || 'plugin';
async function jsInput() {
  const scripts = await Promise.all([...$('jsp-files').files].map(async (f) => ({ id: pluginId(f.name), source: await f.text() })));
  const code = $('jsp-code').value.trim();
  if (code) scripts.push({ id: 'my-plugin', source: code });
  if (scripts.length) log(`JS plugins: ${scripts.map((x) => x.id).join(', ')}`);
  return scripts;
}

// Frida: Frida scripts from picked files and the editor (prefilled with the example), bundled into one script
const fridaBox = document.querySelector('#plugins input[value=frida]');
fridaBox.onchange = () => { $('frida').hidden = !fridaBox.checked; };
fetch('plugins/frida-example.js').then((r) => r.text()).then((t) => { if (!$('frida-code').value) $('frida-code').value = t; }).catch(() => {});
async function fridaInput(abiName) {
  const scripts = await Promise.all([...$('frida-files').files].map(async (f) => ({ name: f.name, code: await f.text() })));
  const code = $('frida-code').value.trim();
  if (code) scripts.push({ name: 'editor.js', code });
  const script = bundleScripts(scripts, await (await fetch(BOOTSTRAP_URL)).text());
  const s = step('Download frida-gadget');
  const res = await fetch(GADGET_URLS[abiName]);
  if (!res.ok) { s.fail(`HTTP ${res.status}`); throw new Error(`frida-gadget: HTTP ${res.status}`); }
  const gadget = await gunzip(new Uint8Array(await res.arrayBuffer()));
  s.done(mb(gadget.length));
  log(`Frida: ${scripts.map((x) => x.name).join(', ')}`);
  return { gadget, script };
}

// --- package name: "Replace" keeps com.discord, "Next to" uses the editable name ---------
const PKG_RE = /^[a-zA-Z][a-zA-Z0-9_]*(\.[a-zA-Z][a-zA-Z0-9_]*)+$/;
const chosenPackage = () => ($('mode').value === 'replace' ? 'com.discord' : $('pkg').value.trim());
function checkPackage() {
  const p = $('pkg').value.trim();
  const bad = $('mode').value === 'side' && (!PKG_RE.test(p) || p === 'com.discord' || p.length > 127);
  $('pkg').setAttribute('aria-invalid', bad);
  return !bad;
}
$('mode').onchange = () => { $('pkg-row').hidden = $('mode').value === 'replace'; checkPackage(); };
$('pkg').oninput = checkPackage;

$('go').onclick = async () => {
  if (!checkPackage()) {
    $('err').textContent = 'Package name must look like com.example.app (letters, digits, _; at least two parts) and differ from com.discord.';
    $('err').hidden = false; $('pkg').focus();
    return;
  }
  const vc = +$('ver').value;
  $('go').disabled = true; $('err').hidden = true; $('result').hidden = true;
  $('steps').textContent = ''; $('log').textContent = ''; $('progress').hidden = false;
  if (!PROXY) return showManual(vc);
  try {
    const n = splitNames();
    const base = await download(vc, 'base');
    const abi = await download(vc, n.abi);
    const configs = [await downloadDensity(vc), n.lang && (await download(vc, n.lang, true))].filter(Boolean);
    await patchAndOffer(vc, base, abi, configs, $('cpu').value);
    $('go').disabled = false;
  } catch (e) {
    if (e instanceof TypeError) { log(`download blocked (${e.message}); switching to manual mode`); return showManual(vc); }
    fail(e);
  }
};

// --- manual mode: user downloads the splits, we tell them apart by content ---------------
function showManual(vc) {
  const n = splitNames();
  $('links').innerHTML = ['base', n.abi, n.density, n.lang].filter(Boolean)
    .map((s) => `<li><a href="${TRACKER}/tracker/download/${vc}/${s}" target="_blank" rel="noopener">${s}.apk</a></li>`).join('');
  $('manual').hidden = false;
  $('files').onchange = async (ev) => {
    try {
      const found = { configs: [] };
      for (const f of ev.target.files) {
        const u8 = new Uint8Array(await f.arrayBuffer());
        const entries = readZip(u8), names = entries.map((e) => e.name), abi = abiOf(entries);
        if (names.includes('classes.dex')) found.base = u8;
        else if (abi) { found.abi = u8; found.abiName = abi; }
        else if (names.includes('resources.arsc')) found.configs.push(u8);
        log(`${f.name}: ${names.includes('classes.dex') ? 'base' : abi ? `native libs (${abi})` : names.some((x) => x.startsWith('lib/')) ? 'native libs (unsupported CPU, ignored)' : 'config split'}`);
      }
      if (!found.base || !found.abi) throw new Error('need at least base and config.arm64_v8a or config.x86_64');
      await patchAndOffer(vc, found.base, found.abi, found.configs, found.abiName);
      $('go').disabled = false;
    } catch (e) { fail(e); }
  };
}
