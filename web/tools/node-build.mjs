// Build a patched APK from local split files with the same code and key the web page uses.
// usage: node web/tools/node-build.mjs [--package com.x --label Name --plugins messageLogger,frida --script a.js,b.js] out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { basename } from 'node:path';
import { buildPatchedApk } from '../lib/apk.js';
import { BOOTSTRAP_URL, bundleScripts, GADGET_URL, gunzip } from '../lib/frida.js';

const args = process.argv.slice(2), opt = {};
for (const k of ['package', 'label', 'plugins', 'script']) { const i = args.indexOf('--' + k); if (i >= 0) { opt[k] = args[i + 1]; args.splice(i, 2); } }
const [out, base, abi, ...configs] = args;
if (!abi) { console.error('usage: node-build.mjs out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]'); process.exit(1); }
const key = JSON.parse(readFileSync(new URL('../signing-key.json', import.meta.url), 'utf8'));
const identity = Object.fromEntries(['pkcs8', 'spki', 'cert'].map((k) => [k, new Uint8Array(Buffer.from(key[k], 'base64'))]));

const t = Date.now();
const read = (f) => new Uint8Array(readFileSync(f));
// --plugins frida: the gadget from web/plugins, plus --script files (default: the example plugin)
const scripts = (opt.script ?? fileURLToPath(new URL('../plugins/frida-example.js', import.meta.url))).split(',').filter(Boolean);
const frida = opt.plugins?.split(',').includes('frida') ? { gadget: await gunzip(read(new URL('../' + GADGET_URL, import.meta.url))),
  script: bundleScripts(scripts.map((f) => ({ name: basename(f), code: readFileSync(f, 'utf8') })), readFileSync(new URL('../' + BOOTSTRAP_URL, import.meta.url), 'utf8')) } : null;
const parts = await buildPatchedApk({ base: read(base), abi: read(abi), configs: configs.map(read), identity, packageName: opt.package, label: opt.label,
  plugins: opt.plugins ? opt.plugins.split(',') : [], pluginLib: opt.plugins ? new Uint8Array(readFileSync(new URL('../plugins/libplg-arm64.so', import.meta.url))) : null, frida, log: (s) => console.log(s) });
writeFileSync(out, Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength))));
console.log(`wrote ${out} in ${Date.now() - t} ms`);
