// Build a patched APK from local split files with the same code and key the web page uses.
// usage: node web/tools/node-build.mjs [--package com.x --label Name --plugins jsPlugins,frida --bundled messageLogger --js my.js,b.js --script frida.js] out.apk base.apk config.arm64_v8a.apk|config.x86_64.apk [config.*.apk ...]
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { basename } from 'node:path';
import { ABIS, abiOf, buildPatchedApk } from '../lib/apk.js';
import { BUNDLED } from '../lib/plugins.js';
import { BOOTSTRAP_URL, bundleScripts, GADGET_URLS, gunzip } from '../lib/frida.js';
import { readZip } from '../lib/zip.js';

const args = process.argv.slice(2), opt = {};
for (const k of ['package', 'label', 'plugins', 'script', 'js', 'bundled']) { const i = args.indexOf('--' + k); if (i >= 0) { opt[k] = args[i + 1]; args.splice(i, 2); } }
const [out, base, abi, ...configs] = args;
if (!abi) { console.error('usage: node-build.mjs out.apk base.apk config.<abi>.apk [config.*.apk ...]'); process.exit(1); }
const key = JSON.parse(readFileSync(new URL('../signing-key.json', import.meta.url), 'utf8'));
const identity = Object.fromEntries(['pkcs8', 'spki', 'cert'].map((k) => [k, new Uint8Array(Buffer.from(key[k], 'base64'))]));

const t = Date.now();
const read = (f) => new Uint8Array(readFileSync(f));
const abiSplit = read(abi), abiName = abiOf(readZip(abiSplit));
if (!abiName) { console.error(`${abi}: not an arm64-v8a or x86_64 split`); process.exit(1); }
console.log(`ABI: ${abiName}`);
// --plugins frida: the gadget from web/plugins, plus --script files (default: the example plugin)
const scripts = (opt.script ?? fileURLToPath(new URL('../plugins/frida-example.js', import.meta.url))).split(',').filter(Boolean);
const frida = opt.plugins?.split(',').includes('frida') ? { gadget: await gunzip(read(new URL('../' + GADGET_URLS[abiName], import.meta.url))),
  script: bundleScripts(scripts.map((f) => ({ name: basename(f), code: readFileSync(f, 'utf8') })), readFileSync(new URL('../' + BOOTSTRAP_URL, import.meta.url), 'utf8')) } : null;
// --plugins messageLogger (older form) = jsPlugins + --bundled messageLogger
let enabled = opt.plugins ? opt.plugins.split(',') : [];
const bundledIds = (opt.bundled ?? '').split(',').filter(Boolean).concat(enabled.filter((x) => BUNDLED.some((b) => b.id === x)));
if (bundledIds.length && !enabled.includes('jsPlugins')) enabled.push('jsPlugins');
enabled = enabled.filter((x) => !BUNDLED.some((b) => b.id === x));
// bundled picks, then --js files as your own plugins (id = file name)
const jsScripts = BUNDLED.filter((b) => bundledIds.includes(b.id)).map((b) => ({ id: b.id, source: readFileSync(new URL('../' + b.js, import.meta.url), 'utf8') }))
  .concat((opt.js ?? '').split(',').filter(Boolean).map((f) => ({ id: basename(f).replace(/\.js$/i, '').replace(/[^\w-]+/g, '-'), source: readFileSync(f, 'utf8') })));
const parts = await buildPatchedApk({ base: read(base), abi: abiSplit, configs: configs.map(read), identity, packageName: opt.package, label: opt.label,
  plugins: enabled, scripts: jsScripts,
  shadowhookNothing: enabled.length && ABIS[abiName].shadowhookNothing ? new Uint8Array(readFileSync(new URL('../' + ABIS[abiName].shadowhookNothing, import.meta.url))) : null, pluginLib: enabled.length ? new Uint8Array(readFileSync(new URL('../' + ABIS[abiName].pluginLib, import.meta.url))) : null, frida, log: (s) => console.log(s) });
writeFileSync(out, Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength))));
console.log(`wrote ${out} in ${Date.now() - t} ms`);
