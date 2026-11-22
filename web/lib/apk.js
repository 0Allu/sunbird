// Build one installable, patched APK from Discord's base.apk + ABI split + config splits.
import { patchHostLib } from './arm64.js';
import { mergeArsc } from './arsc.js';
import { patchManifest } from './axml.js';
import { patchDex } from './dex.js';
import { signV2 } from './sign.js';
import { crc32, entryData, readZip, storedEntry, writeZip } from './zip.js';

const V1_SIG = /^META-INF\/([^/]+\.(SF|RSA|DSA|EC)|MANIFEST\.MF)$/;
const HOST_LIB = 'lib/arm64-v8a/liblibdiscord_wasm_host.so';
const SKIP = (n) => V1_SIG.test(n) || n === 'stamp-cert-sha256';

/**
 * @param {Uint8Array} base      base.apk
 * @param {Uint8Array} abi       config.arm64_v8a.apk
 * @param {Uint8Array[]} configs other config splits (density, language)
 * @param identity               from sign.newIdentity()
 * @returns {Promise<Uint8Array[]>} parts of the signed APK (new Blob(parts))
 */
export async function buildPatchedApk({ base, abi, configs = [], identity, log = () => {} }) {
  const baseEntries = readZip(base).filter((e) => !SKIP(e.name));
  const abiEntries = readZip(abi);
  const cfgEntries = configs.map(readZip);
  const have = new Set(baseEntries.map((e) => e.name));
  const out = [];
  let dexDone = false;

  for (const e of baseEntries) {
    if (e.name === 'AndroidManifest.xml') {
      const m = patchManifest(await entryData(e));
      log(`manifest: removed ${m.removed} split attribute(s), splits.required=false: ${m.flipped}`);
      out.push(storedEntry(e.name, m.data));
    } else if (e.name === 'resources.arsc') {
      const splits = [];
      for (const z of cfgEntries) { const a = z.find((x) => x.name === 'resources.arsc'); if (a) splits.push(await entryData(a)); }
      out.push(storedEntry(e.name, mergeArsc(await entryData(e), splits)));
      log(`resources: merged ${splits.length} config split table(s)`);
    } else if (!dexDone && /^classes\d*\.dex$/.test(e.name)) {
      const patched = await patchDex(await entryData(e));
      if (patched) { dexDone = true; out.push(storedEntry(e.name, patched)); log(`${e.name}: PhoenixLaunch forced on`); }
      else out.push(e);
    } else out.push(e);
  }
  if (!dexDone) throw new Error('PhoenixLaunch method not found in any dex');

  let soDone = false;
  for (const e of abiEntries) {
    if (!e.name.startsWith('lib/') || have.has(e.name)) continue;
    if (e.name === HOST_LIB) {
      if (e.method !== 0) throw new Error('host lib is compressed; expected stored');
      const data = e.raw; // patched in place inside the caller's split buffer (saves ~100 MB on phones)
      patchHostLib(data, (s) => log(`host lib: ${s}`));
      out.push({ ...e, raw: data, crc: crc32(data) });
      soDone = true;
    } else out.push(e);
  }
  if (!soDone) throw new Error(`${HOST_LIB} not found in the ABI split`);

  let added = 0;
  for (const z of cfgEntries)
    for (const e of z)
      if (e.name.startsWith('res/') && !have.has(e.name)) { out.push(e); have.add(e.name); added++; }
  log(`resources: added ${added} file(s) from config splits`);

  const zip = writeZip(out);
  log('signing');
  return signV2(zip, identity, log);
}
