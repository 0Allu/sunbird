// Binary AndroidManifest.xml edit so the merged APK installs on its own:
//  - drop android:requiredSplitTypes / android:splitTypes / android:isSplitRequired
//  - set meta-data com.android.vending.splits.required to false (Play Core missing-splits check)

const DROP_ATTRS = new Set([0x0101064e, 0x0101064f, 0x01010591]); // requiredSplitTypes, splitTypes, isSplitRequired
const ATTR_NAME = 0x01010003, ATTR_VALUE = 0x01010024;

function readPool(u8, v, p) {
  const count = v.getUint32(p + 8, true), flags = v.getUint32(p + 16, true), strStart = v.getUint32(p + 20, true);
  const utf8 = !!(flags & 0x100);
  const dec = new TextDecoder(utf8 ? 'utf-8' : 'utf-16le');
  return (i) => {
    if (i === 0xffffffff || i >= count) return null;
    let q = p + strStart + v.getUint32(p + 28 + 4 * i, true);
    if (utf8) {
      if (u8[q] & 0x80) q += 2; else q += 1; // utf-16 length
      let n = u8[q++]; if (n & 0x80) n = ((n & 0x7f) << 8) | u8[q++];
      return dec.decode(u8.subarray(q, q + n));
    }
    let n = v.getUint16(q, true); q += 2;
    if (n & 0x8000) { n = ((n & 0x7fff) << 16) | v.getUint16(q, true); q += 2; }
    return dec.decode(u8.subarray(q, q + 2 * n));
  };
}

export function patchManifest(src) {
  const u8 = new Uint8Array(src);
  const v = new DataView(u8.buffer);
  if (v.getUint16(0, true) !== 0x0003) throw new Error('not a binary XML manifest');
  let str = null, resIds = [];
  const out = [u8.subarray(0, 8)];
  let removed = 0, flipped = false;
  for (let p = v.getUint16(2, true); p < u8.length; ) {
    const type = v.getUint16(p, true), size = v.getUint32(p + 4, true);
    let chunk = u8.subarray(p, p + size);
    if (type === 0x0001) str = readPool(u8, v, p);
    else if (type === 0x0180) resIds = Array.from({ length: (size - 8) / 4 }, (_, i) => v.getUint32(p + 8 + 4 * i, true));
    else if (type === 0x0102) {
      const aStart = p + 16 + v.getUint16(p + 24, true), aSize = v.getUint16(p + 26, true), aCount = v.getUint16(p + 28, true);
      const attrs = Array.from({ length: aCount }, (_, i) => aStart + i * aSize);
      const resOf = (a) => resIds[v.getUint32(a + 4, true)];
      const keep = attrs.filter((a) => !DROP_ATTRS.has(resOf(a)));
      const tag = str(v.getUint32(p + 20, true));
      if (tag === 'meta-data' && attrs.some((a) => resOf(a) === ATTR_NAME && str(v.getUint32(a + 8, true)) === 'com.android.vending.splits.required')) {
        const val = attrs.find((a) => resOf(a) === ATTR_VALUE);
        chunk = chunk.slice();
        const cv = new DataView(chunk.buffer);
        const o = val - p;
        cv.setUint32(o + 8, 0xffffffff, true); // rawValue: none
        cv.setUint8(o + 15, 0x12); // TYPE_INT_BOOLEAN
        cv.setUint32(o + 16, 0, true); // false
        flipped = true;
      } else if (keep.length !== attrs.length) {
        removed += attrs.length - keep.length;
        const head = u8.slice(p, aStart);
        const hv = new DataView(head.buffer);
        const tail = u8.subarray(aStart + aCount * aSize, p + size);
        const n = head.length + keep.length * aSize + tail.length;
        hv.setUint32(4, n, true);
        hv.setUint16(28, keep.length, true);
        // id/class/style attribute indices are 1-based positions; recompute (0 = none)
        for (const off of [30, 32, 34]) {
          const idx = hv.getUint16(off, true);
          if (idx) hv.setUint16(off, keep.indexOf(attrs[idx - 1]) + 1, true);
        }
        chunk = new Uint8Array(n);
        chunk.set(head, 0);
        keep.forEach((a, i) => chunk.set(u8.subarray(a, a + aSize), head.length + i * aSize));
        chunk.set(tail, head.length + keep.length * aSize);
      }
    }
    out.push(chunk);
    p += size;
  }
  const total = out.reduce((s, c) => s + c.length, 0);
  const res = new Uint8Array(total);
  let o = 0;
  for (const c of out) { res.set(c, o); o += c.length; }
  new DataView(res.buffer).setUint32(4, total, true);
  return { data: res, removed, flipped };
}
