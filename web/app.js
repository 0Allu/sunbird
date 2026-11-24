import { PROXY as DEFAULT_PROXY, VERIFIED } from './config.js';

const TRACKER = 'https://tracker.vendetta.rocks';
const PROXY = (new URLSearchParams(location.search).get('proxy') ?? DEFAULT_PROXY).replace(/\/$/, '');
const DENSITIES = [['ldpi', 120], ['mdpi', 160], ['hdpi', 240], ['xhdpi', 320], ['xxhdpi', 480], ['xxxhdpi', 640]];
const LANGS = ['en', 'ar', 'bg', 'cs', 'da', 'de', 'el', 'es', 'fi', 'fr', 'hi', 'hr', 'hu', 'id', 'it', 'ja', 'ko', 'lt', 'nl', 'no', 'pl', 'pt', 'ro', 'ru', 'sv', 'th', 'tr', 'uk', 'vi', 'zh'];
// The only ABI with a loader build (scripts/site.nu): its split and the libraries the plugin runtime adds,
// the loader first under its name in the APK.
const ABI = { split: 'config.arm64_v8a', libs: { 'libplm.so': 'plugins/libphoenix-arm64.so', 'libshadowhook_nothing.so': 'plugins/libshadowhook_nothing-arm64.so' } };
const GADGET_URL = 'plugins/frida-gadget-17.27.4-android-arm64.so.gz';
const BOOTSTRAP_URL = 'plugins/frida-bootstrap.js';
const $ = (id) => document.getElementById(id);

// --- setup ------------------------------------------------------------------------------
const dpi = Math.round((window.devicePixelRatio || 1) * 160);
const bestDensity = (DENSITIES.find(([, d]) => d >= dpi) || DENSITIES.at(-1))[0];
$('dpi').innerHTML = DENSITIES.map(([n, d]) => `<option value="${n}" ${n === bestDensity ? 'selected' : ''}>${n} (${d})</option>`).join('');
const myLang = (navigator.language || 'en').slice(0, 2).toLowerCase();
$('lang').innerHTML = ['none', ...LANGS].map((l) => `<option ${l === (LANGS.includes(myLang) ? myLang : 'en') ? 'selected' : ''}>${l}</option>`).join('');

function setVersions(latest) {
  const vs = [...new Set([...VERIFIED, ...(latest ? [latest] : [])])].sort((a, b) => b - a);
  $('ver').innerHTML = vs.map((v) => `<option value="${v}" ${v === VERIFIED[0] ? 'selected' : ''}>${v}${VERIFIED.includes(v) ? ' (verified)' : ' (latest alpha, untested)'}</option>`).join('');
}
setVersions();
fetch(`${TRACKER}/tracker/index`).then((r) => r.json()).then((j) => setVersions(j.latest?.alpha)).catch(() => {});

async function fetchOk(url) {
  const res = await fetch(url);
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  return res;
}
const fetchBytes = async (url) => new Uint8Array(await (await fetchOk(url)).arrayBuffer());

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
  return { abi: ABI.split, density: `config.${$('dpi').value}`, lang: lang === 'none' ? null : `config.${lang}` };
}

// Splits of any kind, told apart by the patcher. Their buffers move to the worker.
function patch(apks, libraries, plugins, frida, packageName, label) {
  return new Promise((resolve, reject) => {
    const worker = new Worker('patcher.js', { type: 'module' });
    worker.onmessage = ({ data }) => {
      if (data.log) return log(data.log);
      worker.terminate();
      data.error ? reject(new Error(data.error)) : resolve(data.apk);
    };
    worker.onerror = (e) => { worker.terminate(); reject(new Error(e.message)); };
    const buffers = [...apks, ...libraries.map((l) => l.bytes), ...(frida ? [frida.gadget] : [])].map((b) => b.buffer);
    worker.postMessage({ apks, libraries, plugins, frida, packageName, label }, buffers);
  });
}

async function patchAndOffer(vc, apks) {
  const plugins = await pluginInput();
  const frida = $('frida-on').checked ? await fridaInput() : null;
  const s = step('Patch and sign');
  s.progress(0.5, 'working…');
  const packageName = chosenPackage(), side = packageName !== 'com.discord';
  const libraries = !plugins ? [] : await Promise.all(Object.entries(ABI.libs).map(async ([name, url]) => ({ name, bytes: await fetchBytes(url) })));
  const blob = await patch(apks, libraries, plugins, frida, packageName, side ? 'Discord Phoenix' : undefined);
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

// --- plugin runtime: the bundled plugins (web/plugins, tools/bundle-plugins.mjs), picked .js files and the editor ---
($('runtime').onchange = () => { $('jsp').hidden = !$('runtime').checked; })(); // browsers may restore the box on reload
fetchOk('plugins/index.json').then((r) => r.json()).then((list) => {
  for (const p of list) {
    const l = document.createElement('label');
    l.className = 'plugin';
    l.innerHTML = '<input type="checkbox"><b></b><span></span>';
    l.querySelector('input').value = p.name;
    l.querySelector('input').checked = p.name !== 'example'; // a demo of the API: opt-in
    l.querySelector('b').textContent = p.name;
    l.querySelector('span').textContent = p.description;
    if (p.warning) {
      const w = document.createElement('small');
      w.className = 'warn';
      w.textContent = `\u26A0 ${p.warning}`;
      l.append(w);
    }
    $('jsp-bundled').append(l);
  }
}).catch(fail);
/** [{name, source}], or null with the runtime off */
async function pluginInput() {
  if (!$('runtime').checked) return null;
  const picked = [...document.querySelectorAll('#jsp-bundled input:checked')].map((i) => i.value);
  const plugins = await Promise.all(picked.map(async (name) => ({ name, source: await (await fetchOk(`plugins/${name}.js`)).text() })));
  plugins.push(...await Promise.all([...$('jsp-files').files].map(async (f) => ({ name: f.name.replace(/\.[^.]*$/, ''), source: await f.text() }))));
  const code = $('jsp-code').value.trim();
  if (code) plugins.push({ name: 'my-plugin', source: code });
  return plugins;
}

// Frida: Frida scripts from picked files and the editor (prefilled with the example), run by the bootstrap
($('frida-on').onchange = () => { $('frida').hidden = !$('frida-on').checked; })();
fetch('plugins/frida-example.js').then((r) => r.text()).then((t) => { if (!$('frida-code').value) $('frida-code').value = t; }).catch(() => {});
async function fridaInput() {
  const scripts = await Promise.all([...$('frida-files').files].map(async (f) => ({ name: f.name, code: await f.text() })));
  const code = $('frida-code').value.trim();
  if (code) scripts.push({ name: 'editor.js', code });
  // a typo fails the build here instead of showing up only in the app's console
  for (const { name, code } of scripts) {
    try { new Function(code); } catch (e) { throw new Error(`${name}: ${e.message}`); }
  }
  const bootstrap = await (await fetchOk(BOOTSTRAP_URL)).text();
  const s = step('Download frida-gadget');
  const res = await fetch(GADGET_URL);
  if (!res.ok) { s.fail(`HTTP ${res.status}`); throw new Error(`frida-gadget: HTTP ${res.status}`); }
  const gadget = new Uint8Array(await res.arrayBuffer());
  s.done(mb(gadget.length));
  log(`Frida: ${scripts.map((x) => x.name).join(', ')}`);
  return { gadget, bootstrap, scripts };
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
    await patchAndOffer(vc, [base, abi, ...configs]);
    $('go').disabled = false;
  } catch (e) {
    if (e instanceof TypeError) { log(`download blocked (${e.message}); switching to manual mode`); return showManual(vc); }
    fail(e);
  }
};

// --- manual mode: user downloads the splits, the patcher tells them apart by content -----
function showManual(vc) {
  const n = splitNames();
  $('links').innerHTML = ['base', n.abi, n.density, n.lang].filter(Boolean)
    .map((s) => `<li><a href="${TRACKER}/tracker/download/${vc}/${s}" target="_blank" rel="noopener">${s}.apk</a></li>`).join('');
  $('manual').hidden = false;
  $('files').onchange = async (ev) => {
    try {
      const apks = await Promise.all([...ev.target.files].map(async (f) => new Uint8Array(await f.arrayBuffer())));
      await patchAndOffer(vc, apks);
      $('go').disabled = false;
    } catch (e) { fail(e); }
  };
}
