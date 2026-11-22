// Minimal ZIP reader/writer for APKs. Unchanged entries are copied compressed, as-is.
// Stored entries are aligned (4 bytes, native libs to 16 KiB) through local-header extra padding.

const dv = (u8) => new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
const td = new TextDecoder();
const te = new TextEncoder();

export function readZip(u8) {
  const v = dv(u8);
  let e = u8.length - 22;
  while (e >= 0 && v.getUint32(e, true) !== 0x06054b50) e--;
  if (e < 0) throw new Error('not a zip (no EOCD)');
  const count = v.getUint16(e + 10, true);
  let p = v.getUint32(e + 16, true);
  const entries = [];
  for (let i = 0; i < count; i++) {
    if (v.getUint32(p, true) !== 0x02014b50) throw new Error('bad central directory');
    const method = v.getUint16(p + 10, true);
    const crc = v.getUint32(p + 16, true);
    const csize = v.getUint32(p + 20, true);
    const usize = v.getUint32(p + 24, true);
    const nlen = v.getUint16(p + 28, true), xlen = v.getUint16(p + 30, true), clen = v.getUint16(p + 32, true);
    const lho = v.getUint32(p + 42, true);
    const name = td.decode(u8.subarray(p + 46, p + 46 + nlen));
    const lnl = v.getUint16(lho + 26, true), lxl = v.getUint16(lho + 28, true);
    const start = lho + 30 + lnl + lxl;
    entries.push({ name, method, crc, csize, usize, raw: u8.subarray(start, start + csize) });
    p += 46 + nlen + xlen + clen;
  }
  return entries;
}

export async function inflateRaw(u8) {
  const s = new Blob([u8]).stream().pipeThrough(new DecompressionStream('deflate-raw'));
  return new Uint8Array(await new Response(s).arrayBuffer());
}

export async function entryData(e) {
  if (e.method === 0) return e.raw;
  if (e.method === 8) return inflateRaw(e.raw);
  throw new Error(`${e.name}: unsupported method ${e.method}`);
}

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

export function crc32(u8) {
  let c = 0xffffffff;
  for (let i = 0; i < u8.length; i++) c = CRC_TABLE[(c ^ u8[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** A stored (uncompressed) entry with fresh data. */
export function storedEntry(name, data) {
  return { name, method: 0, crc: crc32(data), csize: data.length, usize: data.length, raw: data };
}

function alignFor(e) {
  if (e.method !== 0) return 0;
  return e.name.endsWith('.so') ? 16384 : 4;
}

/** Build a zip as an array of Uint8Array parts (no big concatenation). Returns {parts, cdOffset, cdSize}. */
export function writeZip(entries) {
  const parts = [];
  const cd = [];
  let off = 0;
  for (const e of entries) {
    const name = te.encode(e.name);
    const align = alignFor(e);
    // zero-filled extra field, same as zipalign
    const pad = align ? (align - ((off + 30 + name.length) % align)) % align : 0;
    const lh = new Uint8Array(30 + name.length + pad);
    const lv = dv(lh);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(4, e.method === 0 ? 10 : 20, true);
    lv.setUint16(6, 0, true);
    lv.setUint16(8, e.method, true);
    lv.setUint32(10, 0x00210000, true); // fixed DOS time 1981-01-01
    lv.setUint32(14, e.crc, true);
    lv.setUint32(18, e.csize, true);
    lv.setUint32(22, e.usize, true);
    lv.setUint16(26, name.length, true);
    lv.setUint16(28, pad, true);
    lh.set(name, 30);
    parts.push(lh, e.raw);
    cd.push({ e, name, off });
    off += lh.length + e.raw.length;
  }
  const cdStart = off;
  for (const { e, name, off: lho } of cd) {
    const c = new Uint8Array(46 + name.length);
    const cv = dv(c);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true);
    cv.setUint16(6, e.method === 0 ? 10 : 20, true);
    cv.setUint16(10, e.method, true);
    cv.setUint32(12, 0x00210000, true);
    cv.setUint32(16, e.crc, true);
    cv.setUint32(20, e.csize, true);
    cv.setUint32(24, e.usize, true);
    cv.setUint16(28, name.length, true);
    cv.setUint32(42, lho, true);
    c.set(name, 46);
    parts.push(c);
    off += c.length;
  }
  const cdSize = off - cdStart;
  const eocd = new Uint8Array(22);
  const ev = dv(eocd);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, cd.length, true);
  ev.setUint16(10, cd.length, true);
  ev.setUint32(12, cdSize, true);
  ev.setUint32(16, cdStart, true);
  parts.push(eocd);
  if (cd.length > 0xffff || off > 0xffffffff) throw new Error('zip64 not supported');
  return { parts, cdOffset: cdStart, cdSize };
}
