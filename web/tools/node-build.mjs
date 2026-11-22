// Build a patched APK from local split files with the same code the web page runs.
// usage: node web/tools/node-build.mjs out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { buildPatchedApk } from '../lib/apk.js';
import { newIdentity } from '../lib/sign.js';

const [out, base, abi, ...configs] = process.argv.slice(2);
if (!abi) { console.error('usage: node-build.mjs out.apk base.apk config.arm64_v8a.apk [config.*.apk ...]'); process.exit(1); }
const idFile = 'phoenix-unlock-key.json';
const b64 = (u8) => Buffer.from(u8).toString('base64');
const unb64 = (s) => new Uint8Array(Buffer.from(s, 'base64'));
let identity;
if (existsSync(idFile)) { const j = JSON.parse(readFileSync(idFile, 'utf8')); identity = { pkcs8: unb64(j.pkcs8), spki: unb64(j.spki), cert: unb64(j.cert) }; }
else { identity = await newIdentity(); writeFileSync(idFile, JSON.stringify({ pkcs8: b64(identity.pkcs8), spki: b64(identity.spki), cert: b64(identity.cert) })); }

const t = Date.now();
const read = (f) => new Uint8Array(readFileSync(f));
const parts = await buildPatchedApk({ base: read(base), abi: read(abi), configs: configs.map(read), identity, log: (s) => console.log(s) });
writeFileSync(out, Buffer.concat(parts.map((p) => Buffer.from(p.buffer, p.byteOffset, p.byteLength))));
console.log(`wrote ${out} in ${Date.now() - t} ms`);
