// Merge config splits' resources.arsc (density / language) into base's, so one APK has everything.
// Split type chunks are appended to base's package next to the same type id; their string values
// are re-pointed into base's global string pool and their keys into base's key pool.

const TYPE_STRING = 0x03;
const FLAG_COMPLEX = 0x01, FLAG_COMPACT = 0x08;
const TYPE_SPARSE = 0x01, TYPE_OFFSET16 = 0x02;

const view = (u8) => new DataView(u8.buffer, u8.byteOffset, u8.byteLength);

// --- string pools as arrays of raw encoded entries ------------------------------------
export function parsePool(u8) {
  const v = view(u8);
  const count = v.getUint32(8, true), styles = v.getUint32(12, true), flags = v.getUint32(16, true);
  const sStart = v.getUint32(20, true), stStart = v.getUint32(24, true);
  const utf8 = !!(flags & 0x100);
  const raws = [];
  for (let i = 0; i < count; i++) {
    const b = sStart + v.getUint32(28 + 4 * i, true);
    let q = b, n;
    if (utf8) {
      q += u8[q] & 0x80 ? 2 : 1;
      n = u8[q++]; if (n & 0x80) n = ((n & 0x7f) << 8) | u8[q++];
      q += n + 1;
    } else {
      n = v.getUint16(q, true); q += 2;
      if (n & 0x8000) { n = ((n & 0x7fff) << 16) | v.getUint16(q, true); q += 2; }
      q += 2 * n + 2;
    }
    raws.push(u8.subarray(b, q));
  }
  const styleOffs = u8.subarray(28 + 4 * count, 28 + 4 * count + 4 * styles);
  const styleData = styles ? u8.subarray(stStart, u8.length) : new Uint8Array(0);
  return { flags, utf8, raws, styles, styleOffs, styleData };
}

export function decodeRaw(raw, utf8) {
  let q = 0, n;
  if (utf8) {
    q += raw[q] & 0x80 ? 2 : 1;
    n = raw[q++]; if (n & 0x80) n = ((n & 0x7f) << 8) | raw[q++];
    return new TextDecoder().decode(raw.subarray(q, q + n));
  }
  const v = view(raw);
  n = v.getUint16(0, true); q = 2;
  if (n & 0x8000) { n = ((n & 0x7fff) << 16) | v.getUint16(2, true); q = 4; }
  return new TextDecoder('utf-16le').decode(raw.subarray(q, q + 2 * n));
}

/** Encode a string as a raw string-pool entry. */
export function encodeRaw(str, utf8) {
  if (utf8) {
    const b = new TextEncoder().encode(str), n16 = str.length;
    const len = (n) => (n > 0x7f ? [0x80 | (n >> 8), n & 0xff] : [n]);
    return Uint8Array.from([...len(n16), ...len(b.length), ...b, 0]);
  }
  const n = str.length, head = n > 0x7fff ? [0x8000 | (n >> 16), n & 0xffff] : [n];
  const out = new Uint8Array(2 * head.length + 2 * n + 2), v = view(out);
  head.forEach((h, i) => v.setUint16(2 * i, h, true));
  for (let i = 0; i < n; i++) v.setUint16(2 * head.length + 2 * i, str.charCodeAt(i), true);
  return out;
}

export function buildPool(p) {
  const n = p.raws.length;
  const dataLen = p.raws.reduce((s, r) => s + r.length, 0);
  const pad = (4 - (dataLen % 4)) % 4;
  const sStart = 28 + 4 * n + 4 * p.styles;
  const stStart = p.styles ? sStart + dataLen + pad : 0;
  const size = sStart + dataLen + pad + p.styleData.length;
  const out = new Uint8Array(size), v = view(out);
  v.setUint16(0, 0x0001, true); v.setUint16(2, 28, true); v.setUint32(4, size, true);
  v.setUint32(8, n, true); v.setUint32(12, p.styles, true);
  v.setUint32(16, p.flags & ~0x1, true); // no longer sorted
  v.setUint32(20, sStart, true); v.setUint32(24, stStart, true);
  let o = 0;
  p.raws.forEach((r, i) => { v.setUint32(28 + 4 * i, o, true); out.set(r, sStart + o); o += r.length; });
  out.set(p.styleOffs, 28 + 4 * n);
  if (p.styles) out.set(p.styleData, stStart);
  return out;
}

// --- table / package chunk walking -----------------------------------------------------
function parseTable(u8) {
  const v = view(u8);
  if (v.getUint16(0, true) !== 0x0002) throw new Error('not a resources.arsc');
  let pool = null;
  const pkgs = [];
  for (let p = v.getUint16(2, true); p < u8.length; p += v.getUint32(p + 4, true)) {
    const t = v.getUint16(p, true), c = u8.subarray(p, p + v.getUint32(p + 4, true));
    if (t === 0x0001) pool = c; else if (t === 0x0200) pkgs.push(c);
  }
  if (pkgs.length !== 1) throw new Error(`expected 1 package, got ${pkgs.length}`);
  return { pool, pkg: parsePackage(pkgs[0]) };
}

function parsePackage(u8) {
  const v = view(u8);
  const hsz = v.getUint16(2, true), typeStr = v.getUint32(268, true), keyStr = v.getUint32(276, true);
  const header = u8.subarray(0, hsz);
  const types = u8.subarray(typeStr, typeStr + v.getUint32(typeStr + 4, true));
  const keys = u8.subarray(keyStr, keyStr + v.getUint32(keyStr + 4, true));
  const chunks = [];
  for (let p = keyStr + keys.length; p < u8.length; p += v.getUint32(p + 4, true))
    chunks.push(u8.subarray(p, p + v.getUint32(p + 4, true)));
  return { id: v.getUint32(8, true), header, types, keys, chunks };
}

// Rewrite string/key references of one type chunk (copy).
function remapType(chunk, keyMap, strBase) {
  const c = chunk.slice(), v = view(c);
  const hsz = v.getUint16(2, true), flags = c[9], n = v.getUint32(12, true), entriesStart = v.getUint32(16, true);
  const offs = [];
  if (flags & TYPE_SPARSE) for (let i = 0; i < n; i++) offs.push(v.getUint16(hsz + 4 * i + 2, true) * 4);
  else if (flags & TYPE_OFFSET16) for (let i = 0; i < n; i++) { const o = v.getUint16(hsz + 2 * i, true); if (o !== 0xffff) offs.push(o * 4); }
  else for (let i = 0; i < n; i++) { const o = v.getUint32(hsz + 4 * i, true); if (o !== 0xffffffff) offs.push(o); }
  const fixValue = (at) => { if (c[at + 3] === TYPE_STRING) v.setUint32(at + 4, v.getUint32(at + 4, true) + strBase, true); };
  for (const o of offs) {
    const e = entriesStart + o, ef = v.getUint16(e + 2, true);
    if (ef & FLAG_COMPACT) {
      const k = keyMap[v.getUint16(e, true)];
      if (k > 0xffff) throw new Error('compact entry key overflow');
      v.setUint16(e, k, true);
      if ((ef >> 8) === TYPE_STRING) v.setUint32(e + 4, v.getUint32(e + 4, true) + strBase, true);
      continue;
    }
    v.setUint32(e + 4, keyMap[v.getUint32(e + 4, true)], true);
    if (ef & FLAG_COMPLEX) {
      const esz = v.getUint16(e, true), cnt = v.getUint32(e + 12, true);
      for (let m = 0; m < cnt; m++) fixValue(e + esz + 12 * m + 4);
    } else fixValue(e + v.getUint16(e, true));
  }
  return c;
}

/** Merge split resources.arsc files into base's (optionally renaming the package). Returns new bytes. */
export function mergeArsc(baseU8, splitU8s, packageName = null) {
  const base = parseTable(baseU8);
  const gp = parsePool(base.pool);
  const kp = parsePool(base.pkg.keys);
  const keyIdx = new Map(kp.raws.map((r, i) => [decodeRaw(r, kp.utf8), i]));
  const baseTypeNames = parsePool(base.pkg.types).raws.map((r) => decodeRaw(r, true));
  let chunks = [...base.pkg.chunks];

  for (const su8 of splitU8s) {
    const s = parseTable(su8);
    if (s.pkg.id !== base.pkg.id) throw new Error('split package id differs from base');
    const sgp = parsePool(s.pool);
    if (sgp.utf8 !== gp.utf8) throw new Error('string pool encodings differ');
    const strBase = gp.raws.length;
    gp.raws.push(...sgp.raws); // styles of the split pool are dropped (config splits have none)
    const skp = parsePool(s.pkg.keys);
    const keyMap = skp.raws.map((r) => {
      const k = decodeRaw(r, skp.utf8);
      if (!keyIdx.has(k)) {
        if (skp.utf8 !== kp.utf8) throw new Error('key pool encodings differ');
        keyIdx.set(k, kp.raws.length); kp.raws.push(r);
      }
      return keyIdx.get(k);
    });
    const sTypeNames = parsePool(s.pkg.types).raws.map((r) => decodeRaw(r, true));
    for (const ch of s.pkg.chunks) {
      const t = view(ch).getUint16(0, true), id = ch[8];
      if (t === 0x0202) {
        if (sTypeNames[id - 1] !== baseTypeNames[id - 1]) throw new Error(`type ${id} name mismatch`);
        const bi = chunks.findIndex((c) => view(c).getUint16(0, true) === 0x0202 && c[8] === id);
        if (bi < 0) { chunks.push(ch); continue; }
        const spec = chunks[bi].slice(), sv = view(spec), xv = view(ch);
        const n = Math.min(sv.getUint32(12, true), xv.getUint32(12, true));
        for (let i = 0; i < n; i++) sv.setUint32(16 + 4 * i, sv.getUint32(16 + 4 * i, true) | xv.getUint32(16 + 4 * i, true), true);
        chunks[bi] = spec;
      } else if (t === 0x0201) {
        const fixed = remapType(ch, keyMap, strBase);
        let at = chunks.length;
        for (let i = chunks.length - 1; i >= 0; i--) if (chunks[i][8] === id && [0x0201, 0x0202].includes(view(chunks[i]).getUint16(0, true))) { at = i + 1; break; }
        chunks.splice(at, 0, fixed);
        const si = chunks.findIndex((c) => view(c).getUint16(0, true) === 0x0202 && c[8] === id);
        if (si >= 0) { const sv = view(chunks[si] = chunks[si].slice()); sv.setUint16(10, sv.getUint16(10, true) + 1, true); }
      }
    }
  }

  const newKeys = buildPool(kp), newPool = buildPool(gp);
  const hdr = base.pkg.header.slice(), hv = view(hdr);
  if (packageName) { // char16_t name[128] at offset 12; getIdentifier() looks packages up by this name
    if (packageName.length > 127) throw new Error('package name too long');
    for (let i = 0; i < 128; i++) hv.setUint16(12 + 2 * i, i < packageName.length ? packageName.charCodeAt(i) : 0, true);
  }
  hv.setUint32(268, hdr.length, true);
  hv.setUint32(276, hdr.length + base.pkg.types.length, true);
  const pkgSize = hdr.length + base.pkg.types.length + newKeys.length + chunks.reduce((s, c) => s + c.length, 0);
  hv.setUint32(4, pkgSize, true);
  const total = 12 + newPool.length + pkgSize;
  const out = new Uint8Array(total), ov = view(out);
  ov.setUint16(0, 0x0002, true); ov.setUint16(2, 12, true); ov.setUint32(4, total, true); ov.setUint32(8, 1, true);
  let o = 12;
  for (const part of [newPool, hdr, base.pkg.types, newKeys, ...chunks]) { out.set(part, o); o += part.length; }
  return out;
}
