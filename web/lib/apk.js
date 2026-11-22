// Build one installable, patched APK from Discord's base.apk + ABI split + config splits.
import { patchHostLib as patchArm64 } from './arm64.js';
import { mergeArsc } from './arsc.js';
import { patchManifest } from './axml.js';
import { patchDex } from './dex.js';
import { fridaFiles } from './frida.js';
import { configurePlugins } from './plugins.js';
import { signV2 } from './sign.js';
import { patchHostLib as patchX86 } from './x86.js';
import { crc32, entryData, readZip, storedEntry, writeZip } from './zip.js';

const V1_SIG = /^META-INF\/([^/]+\.(SF|RSA|DSA|EC)|MANIFEST\.MF)$/;
const HOST = 'liblibdiscord_wasm_host.so';
// Android ABI dir -> its config split, libplg.so build (plugin/build.sh) and host-lib patcher
export const ABIS = {
  'arm64-v8a': { split: 'config.arm64_v8a', pluginLib: 'plugins/libplg-arm64.so', patch: patchArm64, machine: 183 },
  'x86_64': { split: 'config.x86_64', pluginLib: 'plugins/libplg-x86_64.so', patch: patchX86, machine: 62 },
};
/** ABI of a config split (the one whose lib/<abi>/ holds the host lib), or undefined. */
export const abiOf = (entries) => Object.keys(ABIS).find((a) => entries.some((e) => e.name === `lib/${a}/${HOST}`));
const machine = (elf) => elf[18] | (elf[19] << 8);
const SKIP = (n) => V1_SIG.test(n) || n === 'stamp-cert-sha256';

/**
 * @param {Uint8Array} base      base.apk
 * @param {Uint8Array} abi       config.arm64_v8a.apk or config.x86_64.apk (the ABI is taken from it)
 * @param {Uint8Array[]} configs other config splits (density, language)
 * @param identity               {pkcs8, spki, cert} (web/signing-key.json)
 * @param packageName            install under another package (next to the original); default com.discord
 * @param label                  launcher name, e.g. when renamed
 * @param plugins                plugin ids to enable (see plugins.js)
 * @param pluginLib              libplg.so for that ABI (needed when plugins are enabled)
 * @param pluginSources          {id: source} for the enabled JS plugins (PLUGINS[].js)
 * @param userScripts            [{id, source}] your own JS plugins (with 'jsPlugins')
 * @param frida                  {gadget, script} for the 'frida' plugin: frida-gadget .so for that ABI and the bundled JS (frida.js)
 * @returns {Promise<Uint8Array[]>} parts of the signed APK (new Blob(parts))
 */
export async function buildPatchedApk({ base, abi, configs = [], identity, packageName = 'com.discord', label = null, plugins = [], pluginLib = null, pluginSources = {}, userScripts = [], frida = null, log = () => {} }) {
  if (plugins.length && !pluginLib) throw new Error('plugins need libplg.so');
  if (plugins.includes('frida') && !frida) throw new Error('the Frida plugin needs the gadget and a script');
  if (!/^[a-zA-Z][\w]*(\.[a-zA-Z][\w]*)+$/.test(packageName)) throw new Error(`invalid package name: ${packageName}`);
  const baseEntries = readZip(base).filter((e) => !SKIP(e.name));
  const abiEntries = readZip(abi);
  const abiName = abiOf(abiEntries);
  if (!abiName) throw new Error(`no ABI split: ${HOST} not found under ${Object.keys(ABIS).map((a) => `lib/${a}/`).join(' or ')}`);
  const { patch, machine: em } = ABIS[abiName];
  if (plugins.length && machine(pluginLib) !== em) throw new Error(`libplg.so is not built for ${abiName}`);
  if (plugins.includes('frida') && machine(frida.gadget) !== em) throw new Error(`frida-gadget is not built for ${abiName}`);
  const HOST_LIB = `lib/${abiName}/${HOST}`;
  const cfgEntries = configs.map(readZip);
  const have = new Set(baseEntries.map((e) => e.name));
  const out = [];
  let dexDone = false;

  for (const e of baseEntries) {
    if (e.name === 'AndroidManifest.xml') {
      const m = patchManifest(await entryData(e), { packageName, label });
      log(`manifest: removed ${m.removed} split attribute(s), splits.required=false: ${m.flipped}` +
        (m.renamed ? `, ${m.renamed} value(s) renamed to ${packageName}${label ? ` / "${label}"` : ''}` : ''));
      out.push(storedEntry(e.name, m.data));
    } else if (e.name === 'resources.arsc') {
      const splits = [];
      for (const z of cfgEntries) { const a = z.find((x) => x.name === 'resources.arsc'); if (a) splits.push(await entryData(a)); }
      out.push(storedEntry(e.name, mergeArsc(await entryData(e), splits, packageName === 'com.discord' ? null : packageName)));
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
      patch(data, (s) => log(`host lib: ${s}`));
      const plug = plugins.length ? configurePlugins(data, pluginLib, plugins, pluginSources, userScripts, log) : null;
      out.push({ ...e, raw: data, crc: crc32(data) });
      if (plug) out.push(storedEntry(`lib/${abiName}/libplg.so`, plug.lib));
      if (plug?.bundle) out.push(storedEntry(`lib/${abiName}/libplg.js.so`, plug.bundle)); // read from the APK by libplg
      if (plug && plugins.includes('frida')) {
        for (const [n, d] of fridaFiles(frida.gadget, frida.script, abiName)) out.push(storedEntry(n, d));
        log(`Frida: gadget ${(frida.gadget.length / 1048576).toFixed(1)} MB, script ${frida.script.length} chars`);
      }
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
