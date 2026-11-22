// Optional plugins. Each runs inside libplg.so (plugin/plugin.c), which the patched host lib loads
// through a renamed DT_NEEDED entry. The patcher locates what a plugin needs in this Discord build
// and writes it into libplg.so's config block (found by its magic).
import { findRegistrySlots, renameNeeded } from './arm64.js';
import { GADGET } from './frida.js';

export const PLUGINS = [
  {
    id: 'messageLogger',
    flag: 1 << 0,
    name: 'MessageLogger',
    description: 'Keeps deleted messages (shown in red with "(deleted)") and shows earlier versions of edited messages.',
  },
  {
    id: 'frida',
    flag: 0, // not handled by plugin.c: frida-gadget is chained in as libplg.so's dependency (see frida.js)
    name: 'Frida (JS plugins)',
    description: 'Embeds frida-gadget (+40 MB) to run JavaScript plugins inside Discord, with an in-app panel to manage them and a console.',
  },
];

const MAGIC = 'PHXPLUG-CONFIG1';
const MSG_EVENTS = ['MESSAGE_CREATE', 'MESSAGE_UPDATE', 'MESSAGE_DELETE', 'MESSAGE_DELETE_BULK'];

/**
 * @param {Uint8Array} hostLib  liblibdiscord_wasm_host.so (patched in place: DT_NEEDED rename)
 * @param {Uint8Array} plugLib  libplg.so for the same ABI (a configured copy is returned)
 * @param {string[]} enabled    plugin ids
 */
export function configurePlugins(hostLib, plugLib, enabled, log = () => {}) {
  const chosen = PLUGINS.filter((p) => enabled.includes(p.id));
  if (!chosen.length) return null;
  const lib = plugLib.slice();
  const at = indexOf(lib, new TextEncoder().encode(MAGIC + '\0'));
  if (at < 0) throw new Error('libplg.so config block not found');
  const v = new DataView(lib.buffer, lib.byteOffset + at);
  let flags = 0;
  for (const p of chosen) flags |= p.flag;
  if (flags & 1) {
    const s = findRegistrySlots(hostLib, MSG_EVENTS);
    MSG_EVENTS.forEach((n, i) => v.setBigUint64(24 + 8 * i, BigInt(s[n]), true));
    log(`MessageLogger: registry handlers at ${MSG_EVENTS.map((n) => '0x' + s[n].toString(16)).join(', ')}`);
  }
  v.setUint32(20, flags, true);
  if (chosen.some((p) => p.id === 'frida')) renameNeeded(lib, 'liblog.so', GADGET);
  renameNeeded(hostLib, 'liblog.so', 'libplg.so');
  log(`plugins: ${chosen.map((p) => p.name).join(', ')} (host lib now loads libplg.so)`);
  return lib;
}

function indexOf(hay, needle) {
  outer: for (let i = hay.indexOf(needle[0]); i !== -1; i = hay.indexOf(needle[0], i + 1)) {
    for (let j = 1; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}
