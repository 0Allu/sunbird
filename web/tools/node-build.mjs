// Build a patched APK from local split files with the same code and key the web page uses.
// usage: node web/tools/node-build.mjs [--package com.x --label Name --plugins messageLogger,frida --script a.js,b.js] out.apk base.apk config.arm64_v8a.apk|config.x86_64.apk [config.*.apk ...]
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { basename } from 'node:path';
import { ABIS, abiOf, buildPatchedApk } from '../lib/apk.js';
import { PLUGINS } from '../lib/plugins.js';
import { BOOTSTRAP_URL, bundleScripts, GADGET_URLS, gunzip } from '../lib/frida.js';
import { readZip } from '../lib/zip.js';

const args = process.argv.slice(2), opt = {};
for (const k of ['package', 'label', 'plugins', 'script']) { const i = args.indexOf('--' + k); if (i >= 0) { opt[k] = args[i + 1]; args.splice(i, 2); } }
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
const enabled = opt.plugins ? opt.plugins.split(',') : [];
const pluginSources = Object.fromEntries(PLUGINS.filter((p) => p.js && enabled.includes(p.id)).map((p) => [p.id, readFileSync(new URL('../' + p.js, import.meta.url), 'utf8')]));
const parts = await buildPatchedApk({ base: read(base), abi: abiSplit, configs: configs.map(read), identity, packageName: opt.package, label: opt.label,
  plugins: enabled, pluginSources, pluginLib: opt.plugins ? new Uint8Array(readFileSync(new URL('../' + ABIS[abiName].pluginLib, import.meta.url))) : null, frida, log: (s) => console.log(s) });
writeFileSync(out, Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength))));
console.log(`wrote ${out} in ${Date.now() - t} ms`);
