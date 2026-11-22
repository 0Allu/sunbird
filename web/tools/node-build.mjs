// Build a patched APK from local split files with the same code and key the web page uses.
// usage: node web/tools/node-build.mjs [--package com.x --label Name] out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]
import { readFileSync, writeFileSync } from 'node:fs';
import { buildPatchedApk } from '../lib/apk.js';

const args = process.argv.slice(2), opt = {};
for (const k of ['package', 'label']) { const i = args.indexOf('--' + k); if (i >= 0) { opt[k] = args[i + 1]; args.splice(i, 2); } }
const [out, base, abi, ...configs] = args;
if (!abi) { console.error('usage: node-build.mjs out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]'); process.exit(1); }
const key = JSON.parse(readFileSync(new URL('../signing-key.json', import.meta.url), 'utf8'));
const identity = Object.fromEntries(['pkcs8', 'spki', 'cert'].map((k) => [k, new Uint8Array(Buffer.from(key[k], 'base64'))]));

const t = Date.now();
const read = (f) => new Uint8Array(readFileSync(f));
const parts = await buildPatchedApk({ base: read(base), abi: read(abi), configs: configs.map(read), identity, packageName: opt.package, label: opt.label, log: (s) => console.log(s) });
writeFileSync(out, Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength))));
console.log(`wrote ${out} in ${Date.now() - t} ms`);
