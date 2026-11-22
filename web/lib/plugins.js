// Optional plugins. They run inside libplg.so (plugin/: Rust loader + rquickjs runtime), which
// the patched host lib loads through a renamed DT_NEEDED entry. The patcher writes libplg's
// config block (plugin/sigs, found by its magic): the host lib's build ID, so a different Discord
// build runs without plugins instead of crashing, and the offsets it found in this build.
// JS plugins are bundled next to it as lib/<abi>/libplg.js.so ([{id, source}] JSON).
import { Elf, findRegistrySlots, renameNeeded } from './arm64.js';
import { GADGET } from './frida.js';

export const PLUGINS = [
  {
    id: 'messageLogger',
    js: 'plugins/message-logger.js',
    name: 'MessageLogger',
    description: 'Keeps deleted messages (shown in red with "(deleted)") and shows earlier versions of edited messages.',
  },
  {
    id: 'frida',
    name: 'Frida (JS plugins)',
    description: 'Embeds frida-gadget (+40 MB) to run JavaScript plugins inside Discord, with an in-app panel to manage them and a console.',
  },
];

const MAGIC = 'PHXPLUG-CONFIG2';
const VERSION = 2;
const FLAG_RUNTIME = 1;
const te = new TextEncoder();

/** NT_GNU_BUILD_ID of an ELF (20 bytes for lld/clang builds). */
export function buildId(u8) {
  const s = new Elf(u8).sec['.note.gnu.build-id'];
  if (!s) throw new Error('host lib has no GNU build ID');
  const v = new DataView(u8.buffer, u8.byteOffset + s.off);
  const [nsz, dsz] = [v.getUint32(0, true), v.getUint32(4, true)];
  return u8.slice(s.off + 12 + ((nsz + 3) & ~3), s.off + 12 + ((nsz + 3) & ~3) + dsz);
}

/**
 * @param {Uint8Array} hostLib  liblibdiscord_wasm_host.so (patched in place: DT_NEEDED rename)
 * @param {Uint8Array} plugLib  libplg.so for the same ABI (a configured copy is returned)
 * @param {string[]} enabled    plugin ids
 * @param {Record<string,string>} sources  JS source per enabled JS plugin id
 * @returns {{lib: Uint8Array, bundle: Uint8Array|null} | null}
 */
export function configurePlugins(hostLib, plugLib, enabled, sources = {}, log = () => {}) {
  const chosen = PLUGINS.filter((p) => enabled.includes(p.id));
  if (!chosen.length) return null;
  const lib = plugLib.slice();
  const at = indexOf(lib, te.encode(MAGIC + '\0'));
  if (at < 0) throw new Error('libplg.so config block not found (old libplg?)');
  const v = new DataView(lib.buffer, lib.byteOffset + at);
  const id = buildId(hostLib);
  if (id.length !== 20) throw new Error(`unexpected build ID length ${id.length}`);
  const anchor = findRegistrySlots(hostLib, ['MESSAGE_CREATE']).MESSAGE_CREATE;
  v.setUint32(16, VERSION, true);
  v.setUint32(20, FLAG_RUNTIME, true);
  lib.set(id, at + 24);
  v.setBigUint64(48, BigInt(anchor), true);
  // 56/64: <Me as Serialize> + Me's store offset; not located by the patcher yet (stores.me() reports that)
  log(`plugins: host build ${[...id].map((b) => b.toString(16).padStart(2, '0')).join('')}, gateway registry at 0x${anchor.toString(16)}`);

  const bundle = chosen.filter((p) => p.js).map((p) => {
    if (typeof sources[p.id] !== 'string') throw new Error(`no source for plugin ${p.id}`);
    return { id: p.id, source: sources[p.id] };
  });
  if (chosen.some((p) => p.id === 'frida')) renameNeeded(lib, 'liblog.so', GADGET);
  renameNeeded(hostLib, 'liblog.so', 'libplg.so');
  log(`plugins: ${chosen.map((p) => p.name).join(', ')} (host lib now loads libplg.so)`);
  return { lib, bundle: bundle.length ? te.encode(JSON.stringify(bundle)) : null };
}

function indexOf(hay, needle) {
  outer: for (let i = hay.indexOf(needle[0]); i !== -1; i = hay.indexOf(needle[0], i + 1)) {
    for (let j = 1; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}
