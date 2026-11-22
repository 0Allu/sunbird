import { PROXY as DEFAULT_PROXY, VERIFIED } from './config.js';
import { buildPatchedApk } from './lib/apk.js';
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
  return { abi: 'config.arm64_v8a', density: `config.${$('dpi').value}`, lang: lang === 'none' ? null : `config.${lang}` };
}

async function patchAndOffer(vc, base, abi, configs) {
  const s = step('Patch and sign');
  s.progress(0.5, 'working…');
  const parts = await buildPatchedApk({ base, abi, configs, identity: await identity(), log });
  const blob = new Blob(parts, { type: 'application/vnd.android.package-archive' });
  s.done(mb(blob.size));
  const a = $('dl');
  a.href = URL.createObjectURL(blob);
  a.download = `discord-${vc}-phoenix-unlock.apk`;
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

$('go').onclick = async () => {
  const vc = +$('ver').value;
  $('go').disabled = true; $('err').hidden = true; $('result').hidden = true;
  $('steps').textContent = ''; $('log').textContent = ''; $('progress').hidden = false;
  if (!PROXY) return showManual(vc);
  try {
    const n = splitNames();
    const base = await download(vc, 'base');
    const abi = await download(vc, n.abi);
    const configs = [await downloadDensity(vc), n.lang && (await download(vc, n.lang, true))].filter(Boolean);
    await patchAndOffer(vc, base, abi, configs);
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
        const names = readZip(u8).map((e) => e.name);
        if (names.includes('classes.dex')) found.base = u8;
        else if (names.some((x) => x.startsWith('lib/arm64-v8a/'))) found.abi = u8;
        else if (names.includes('resources.arsc')) found.configs.push(u8);
        log(`${f.name}: ${names.includes('classes.dex') ? 'base' : names.some((x) => x.startsWith('lib/')) ? 'native libs' : 'config split'}`);
      }
      if (!found.base || !found.abi) throw new Error('need at least base and config.arm64_v8a');
      await patchAndOffer(vc, found.base, found.abi, found.configs);
      $('go').disabled = false;
    } catch (e) { fail(e); }
  };
}
