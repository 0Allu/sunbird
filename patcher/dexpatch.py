"""Force PhoenixLaunch.i(ContextWrapper)Z to return true, so Discord always starts Phoenix.

The method is found by its log string "Phoenix didn't stay up for 3 launches" (names are
obfuscated and change between builds), checked to return boolean, and its first two code
units become `const/4 v0, #1; return v0`. Dex checksum and signature are recomputed."""
import hashlib
import struct
import zlib

ANCHOR = "Phoenix didn't stay up for 3 launches"


def uleb(d, p):
    r = s = 0
    while True:
        b = d[p]; p += 1
        r |= (b & 0x7f) << s; s += 7
        if not b & 0x80:
            return r, p


def string_at(d, sid_off):
    p = struct.unpack_from('<I', d, sid_off)[0]
    _, p = uleb(d, p)
    return d[p:d.index(0, p)]


def patch_dex(data):
    """Returns patched bytes, or None if the anchor method isn't in this dex."""
    d = bytearray(data)
    (string_ids_size, string_ids_off, _, _, proto_ids_size, proto_ids_off, _, _,
     method_ids_size, method_ids_off, class_defs_size, class_defs_off) = struct.unpack_from('<12I', d, 0x38)
    anchor = ANCHOR.encode()
    sids = [i for i in range(string_ids_size) if string_at(d, string_ids_off + 4 * i).startswith(anchor)]
    if not sids:
        return None
    hits = []
    for c in range(class_defs_size):
        cdata = struct.unpack_from('<I', d, class_defs_off + 32 * c + 24)[0]
        if not cdata:
            continue
        p = cdata
        sf, p = uleb(d, p); inf, p = uleb(d, p); dm, p = uleb(d, p); vm, p = uleb(d, p)
        for _ in range(sf + inf):
            _, p = uleb(d, p); _, p = uleb(d, p)
        midx = 0
        for k in range(dm + vm):
            if k == dm:
                midx = 0  # method_idx_diff restarts for the virtual method list
            diff, p = uleb(d, p); _, p = uleb(d, p); code, p = uleb(d, p)
            midx += diff
            if not code:
                continue
            regs, ins_n, outs, tries, dbg, n = struct.unpack_from('<HHHHII', d, code)
            insns = code + 16
            words = struct.unpack_from(f'<{n}H', d, insns)
            for i, w in enumerate(words):
                op = w & 0xff
                if op == 0x1a and i + 1 < n and words[i + 1] in sids:
                    hits.append((midx, regs, insns))
                elif op == 0x1b and i + 2 < n and (words[i + 1] | words[i + 2] << 16) in sids:
                    hits.append((midx, regs, insns))
    hits = sorted(set(hits))
    if len(hits) != 1:
        raise ValueError(f'PhoenixLaunch anchor: expected 1 method, got {len(hits)}')
    midx, regs, insns = hits[0]
    proto = struct.unpack_from('<H', d, method_ids_off + 8 * midx + 2)[0]
    shorty = string_at(d, string_ids_off + 4 * struct.unpack_from('<I', d, proto_ids_off + 12 * proto)[0])
    if not shorty.startswith(b'Z') or regs < 1:
        raise ValueError(f'PhoenixLaunch anchor method has shorty {shorty!r}, regs {regs}')
    d[insns:insns + 4] = bytes.fromhex('12100f00')  # const/4 v0, #1 ; return v0
    d[12:32] = hashlib.sha1(d[32:]).digest()
    d[8:12] = struct.pack('<I', zlib.adler32(d[12:]))
    return bytes(d), insns
