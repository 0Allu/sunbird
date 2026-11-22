// Force PhoenixLaunch.i(ContextWrapper)Z to return true. JS port of patcher/dexpatch.py.
// The method is found by its log string (names are obfuscated), must return boolean, and its first
// two code units become `const/4 v0, #1; return v0`. Checksum and SHA-1 signature are recomputed.

const ANCHOR = new TextEncoder().encode("Phoenix didn't stay up for 3 launches");

function uleb(d, p) {
  let r = 0, s = 0, b;
  do { b = d[p++]; r |= (b & 0x7f) << s; s += 7; } while (b & 0x80);
  return [r >>> 0, p];
}

function strData(d, v, sidOff) {
  let p = v.getUint32(sidOff, true);
  [, p] = uleb(d, p);
  return p;
}

function startsWith(d, p, needle) {
  for (let i = 0; i < needle.length; i++) if (d[p + i] !== needle[i]) return false;
  return true;
}

function adler32(d, from) {
  let a = 1, b = 0;
  for (let i = from; i < d.length; ) {
    const end = Math.min(d.length, i + 3800);
    for (; i < end; i++) { a += d[i]; b += a; }
    a %= 65521; b %= 65521;
  }
  return ((b << 16) | a) >>> 0;
}

/** Returns patched dex bytes, or null if this dex doesn't contain the anchor. */
export async function patchDex(data) {
  const d = new Uint8Array(data); // copy
  const v = new DataView(d.buffer);
  const [stringIdsSize, stringIdsOff, , , protoIdsSize, protoIdsOff, , , , methodIdsOff, classDefsSize, classDefsOff] =
    Array.from({ length: 12 }, (_, i) => v.getUint32(0x38 + 4 * i, true));
  void protoIdsSize;
  const sids = new Set();
  for (let i = 0; i < stringIdsSize; i++) if (startsWith(d, strData(d, v, stringIdsOff + 4 * i), ANCHOR)) sids.add(i);
  if (!sids.size) return null;

  const hits = new Map();
  for (let c = 0; c < classDefsSize; c++) {
    const cdata = v.getUint32(classDefsOff + 32 * c + 24, true);
    if (!cdata) continue;
    let p = cdata, sf, inf, dm, vm, x;
    [sf, p] = uleb(d, p); [inf, p] = uleb(d, p); [dm, p] = uleb(d, p); [vm, p] = uleb(d, p);
    for (let i = 0; i < sf + inf; i++) { [x, p] = uleb(d, p); [x, p] = uleb(d, p); }
    let midx = 0;
    for (let k = 0; k < dm + vm; k++) {
      if (k === dm) midx = 0; // method_idx_diff restarts for the virtual list
      let diff, code;
      [diff, p] = uleb(d, p); [x, p] = uleb(d, p); [code, p] = uleb(d, p);
      midx += diff;
      if (!code) continue;
      const regs = v.getUint16(code, true), n = v.getUint32(code + 12, true), insns = code + 16;
      for (let i = 0; i < n; i++) {
        const op = d[insns + 2 * i];
        if (op === 0x1a && i + 1 < n && sids.has(v.getUint16(insns + 2 * i + 2, true))) hits.set(insns, { midx, regs, insns });
        else if (op === 0x1b && i + 2 < n && sids.has(v.getUint32(insns + 2 * i + 2, true))) hits.set(insns, { midx, regs, insns });
      }
    }
  }
  if (hits.size !== 1) throw new Error(`PhoenixLaunch anchor: expected 1 method, got ${hits.size}`);
  const { midx, regs, insns } = [...hits.values()][0];
  const proto = v.getUint16(methodIdsOff + 8 * midx + 2, true);
  const shorty = d[strData(d, v, stringIdsOff + 4 * v.getUint32(protoIdsOff + 12 * proto, true))];
  if (shorty !== 0x5a /* 'Z' */ || regs < 1) throw new Error('PhoenixLaunch anchor method does not return boolean');
  d.set([0x12, 0x10, 0x0f, 0x00], insns); // const/4 v0, #1 ; return v0
  d.set(new Uint8Array(await crypto.subtle.digest('SHA-1', d.subarray(32))), 12);
  v.setUint32(8, adler32(d, 12), true);
  return d;
}
