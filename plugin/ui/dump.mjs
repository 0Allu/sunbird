// node dump.mjs <capture.hex|capture.bin> [--ops]
// Decodes a Phoenix UI-op batch with uiops.js, verifies encode(decode(b)) == b,
// and prints created nodes as a tree (plus non-create ops). --ops prints the raw op list.
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
(0, eval)(readFileSync(join(here, 'uiops.js'), 'utf8'));
const { decode, encode } = globalThis.PhxUI;

const file = process.argv[2];
if (!file) { console.error('usage: node dump.mjs <file.hex|file.bin> [--ops]'); process.exit(1); }
const raw = readFileSync(file);
const txt = raw.toString('latin1').trim();
const bytes = /^[0-9a-fA-F\s]+$/.test(txt) ? Uint8Array.from(Buffer.from(txt.replace(/\s+/g, ''), 'hex')) : new Uint8Array(raw);

const ops = decode(bytes);
const re = encode(ops);
const same = re.length === bytes.length && re.every((b, i) => b === bytes[i]);
console.log(`# ${file}: ${bytes.length} bytes, ${ops.length} ops, round-trip ${same ? 'OK' : 'MISMATCH'}`);

const fmt = (v) => {
  switch (v.t) {
    case 'null': case 'true': case 'false': return v.t;
    case 'i32': return Math.abs(v.v) > 0xffffff ? '0x' + (v.v >>> 0).toString(16) : String(v.v);
    case 'i64': return v.v > 0xffffffn ? '0x' + v.v.toString(16) + 'n' : v.v + 'n';
    case 'f64': return String(v.v);
    case 'str': return JSON.stringify(v.v);
    case 'rect': return '[' + v.v.join(',') + ']';
    case 'arr': return '[' + v.v.map(fmt).join(', ') + ']';
    case 'obj': case 'map': return '{' + v.v.map((e) => e.k + ': ' + fmt(e.v)).join(', ') + '}';
    case 'words': return 'words[' + v.v.join(',') + ']';
    case 'wpatch': return `wpatch(list ${v.v.list}, ${JSON.stringify(v.v.set)})`;
  }
};
const fmtProps = (ps) => ps.map((p) => `${p.k}=${fmt(p.v)}`).join(' ');
const hex = (n) => '#' + n.toString(16);

if (process.argv.includes('--ops')) {
  for (const o of ops) console.log(JSON.stringify(o, (k, v) => (typeof v === 'bigint' ? v + 'n' : v)));
  process.exit(same ? 0 : 2);
}

// tree from this batch's create + children/childEdits ops
const nodes = new Map(), kids = new Map(), hasParent = new Set();
for (const o of ops) {
  if (o.op === 'create') nodes.set(o.id, o);
  if (o.op === 'children') kids.set(o.id, o.ids);
  if (o.op === 'childEdits') kids.set(o.id, [...(kids.get(o.id) || []), ...o.edits.flatMap((e) => e.ins)]);
}
for (const ids of kids.values()) ids.forEach((i) => hasParent.add(i));
const print = (id, depth, seen) => {
  const n = nodes.get(id), pad = '  '.repeat(depth);
  if (seen.has(id)) return;
  seen.add(id);
  console.log(pad + hex(id) + ' ' + (n ? `<${n.type}> ${fmtProps(n.props)}` : '(existing)'));
  for (const c of kids.get(id) || []) print(c, depth + 1, seen);
};
const seen = new Set();
const roots = [...new Set([...nodes.keys(), ...kids.keys()])].filter((id) => !hasParent.has(id));
for (const r of roots) print(r, 0, seen);
for (const o of ops) {
  if (o.op === 'create' || o.op === 'children') continue;
  if (o.op === 'update') console.log(`update ${hex(o.id)} ${fmtProps(o.props)}`);
  else if (o.op === 'childEdits') console.log(`childEdits ${hex(o.id)} ` + o.edits.map((e) => `keep ${e.keep} del ${e.del} ins [${e.ins.map(hex)}]`).join('; '));
  else if (o.op === 'delete') console.log(`delete [${o.ids.map(hex)}]`);
  else if (o.op === 'scrollTo') console.log(`scrollTo ${hex(o.id)} ${fmt(o.v)}`);
  else console.log(o.op, JSON.stringify(o));
}
process.exit(same ? 0 : 2);
