"""Minimal ELF view of liblibdiscord_wasm_host.so for x86_64 / arm64: sections, vaddr<->file
offset mapping, Android APS2 relocations, string search, and code xrefs. No pyelftools."""
import bisect
import re
import struct

import numpy as np
from capstone import CS_ARCH_X86, CS_MODE_64, Cs

try:
    from capstone import CS_ARCH_ARM64 as CS_ARM64
except ImportError:  # capstone 6
    from capstone import CS_ARCH_AARCH64 as CS_ARM64
from capstone import CS_MODE_ARM

EM_X86_64, EM_AARCH64 = 62, 183
R_RELATIVE = {EM_X86_64: 8, EM_AARCH64: 1027}


class Bin:
    def __init__(self, data: bytes):
        self.d = bytearray(data)
        d = self.d
        assert d[:4] == b'\x7fELF' and d[4] == 2, 'not ELF64'
        self.machine = struct.unpack_from('<H', d, 18)[0]
        assert self.machine in R_RELATIVE, f'unsupported machine {self.machine}'
        self.arch = 'x86_64' if self.machine == EM_X86_64 else 'arm64'
        phoff, shoff = struct.unpack_from('<QQ', d, 32)
        phentsize, phnum, shentsize, shnum, shstrndx = struct.unpack_from('<HHHHH', d, 54)
        self.loads = []
        for i in range(phnum):
            p_type, _, p_off, p_vaddr, _, p_filesz = struct.unpack_from('<IIQQQQ', d, phoff + i * phentsize)
            if p_type == 1:
                self.loads.append((p_vaddr, p_off, p_filesz))
        secs = [struct.unpack_from('<IIQQQQ', d, shoff + i * shentsize) for i in range(shnum)]
        stro = secs[shstrndx][4]
        self.sec = {}
        for name, _, _, addr, off, size in secs:
            n = d[stro + name:d.index(0, stro + name)].decode()
            self.sec[n] = (addr, off, size)
        self.md = Cs(CS_ARCH_X86, CS_MODE_64) if self.arch == 'x86_64' else Cs(CS_ARM64, CS_MODE_ARM)
        self._rel = None
        self._xref = None
        self._starts = None

    # --- address mapping -------------------------------------------------------------
    def off(self, va):
        for v, o, sz in self.loads:
            if v <= va < v + sz:
                return va - v + o
        raise ValueError(hex(va))

    def read(self, va, n):
        o = self.off(va)
        return bytes(self.d[o:o + n])

    def q(self, va):
        return struct.unpack('<Q', self.read(va, 8))[0]

    def write(self, va, b):
        o = self.off(va)
        self.d[o:o + len(b)] = b

    # --- relocations (Android APS2-packed RELA) ------------------------------------
    def relocs(self):
        """{slot_vaddr: addend} for R_*_RELATIVE (pointer slots in .data.rel.ro/.got)."""
        if self._rel is not None:
            return self._rel
        addr, off, size = self.sec['.rela.dyn']
        d = self.d
        out = {}
        if d[off:off + 4] == b'APS2':
            p = [off + 4]

            def sl():
                r = s = 0
                while True:
                    b = d[p[0]]; p[0] += 1
                    r |= (b & 0x7f) << s; s += 7
                    if not b & 0x80:
                        return r - (1 << s) if b & 0x40 else r

            n, roff, add, cnt = sl(), sl(), 0, 0
            while cnt < n:
                gs, fl = sl(), sl()
                god = sl() if fl & 2 else 0
                info = sl() if fl & 1 else 0
                if fl & 8 and fl & 4:
                    add += sl()
                for _ in range(gs):
                    roff += god if fl & 2 else sl()
                    if not fl & 1:
                        info = sl()
                    if fl & 8 and not fl & 4:
                        add += sl()
                    if info & 0xffffffff == R_RELATIVE[self.machine]:
                        out[roff] = add if fl & 8 else 0
                    cnt += 1
        else:
            for i in range(off, off + size, 24):
                ro, info, ad = struct.unpack_from('<QQq', d, i)
                if info & 0xffffffff == R_RELATIVE[self.machine]:
                    out[ro] = ad
        self._rel = out
        return out

    def slots_to(self, target):
        return [s for s, a in self.relocs().items() if a == target]

    def ptr(self, slot):
        """Value a pointer slot holds after relocation."""
        return self.relocs().get(slot)

    # --- strings -------------------------------------------------------------------
    def find_str(self, s):
        addr, off, size = self.sec['.rodata']
        b = s.encode()
        return [addr + m.start() for m in re.finditer(re.escape(b), bytes(self.d[off:off + size]))]

    # --- functions (from .eh_frame_hdr) --------------------------------------------
    def starts(self):
        if self._starts is None:
            addr, off, size = self.sec['.eh_frame_hdr']
            cnt = struct.unpack_from('<I', self.d, off + 8)[0]
            self._starts = sorted(struct.unpack_from('<i', self.d, off + 12 + i * 8)[0] + addr for i in range(cnt))
        return self._starts

    def func(self, a):
        st = self.starts()
        i = bisect.bisect_right(st, a) - 1
        return st[i], st[i + 1] if i + 1 < len(st) else a + 0x1000

    def dis(self, a, b):
        return list(self.md.disasm(self.read(a, b - a), a))

    # --- code xrefs ------------------------------------------------------------------
    def xrefs(self):
        """{target_vaddr: [instruction vaddr, ...]} for PC-relative address materialisation.
        x86_64: lea/mov/call/jmp [rip+disp]. arm64: adrp + add / ldr (64-bit) on the same reg."""
        if self._xref is not None:
            return self._xref
        addr, off, size = self.sec['.text']
        out = {}
        if self.arch == 'x86_64':
            text = bytes(self.d[off:off + size])
            # REX.W lea/mov r64,[rip+d32]; call/jmp [rip+d32]
            for m in re.finditer(rb'[\x48\x4c][\x8d\x8b][\x05\x0d\x15\x1d\x25\x2d\x35\x3d]|\xff[\x15\x25]', text):
                i = m.start()
                n = 3 if text[i] != 0xff else 2
                ln = n + 4
                t = addr + i + ln + struct.unpack_from('<i', text, i + n)[0]
                out.setdefault(t, []).append(addr + i)
        else:
            w = np.frombuffer(bytes(self.d[off:off + size - size % 4]), dtype='<u4')
            pc = addr + 4 * np.arange(len(w), dtype=np.int64)
            isadrp = (w & 0x9f000000) == 0x90000000
            idx = np.nonzero(isadrp)[0]
            aw = w[idx].astype(np.int64)
            imm = ((aw >> 29) & 3) | (((aw >> 5) & 0x7ffff) << 2)
            imm = np.where(imm & (1 << 20), imm - (1 << 21), imm)
            page = (pc[idx] & ~0xfff) + (imm << 12)
            rd = aw & 0x1f
            for k in range(1, 5):  # partner within the next 4 instructions
                j = idx + k
                ok = j < len(w)
                jj, ii, pg, r = j[ok], idx[ok], page[ok], rd[ok]
                nw = w[jj].astype(np.int64)
                rn = (nw >> 5) & 0x1f
                isadd = ((nw & 0xffc00000) == 0x91000000) & (rn == r)
                isldr = ((nw & 0xffc00000) == 0xf9400000) & (rn == r)
                imm12 = (nw >> 10) & 0xfff
                tgt = np.where(isadd, pg + imm12, pg + imm12 * 8)
                sel = isadd | isldr
                for a, t in zip(pc[ii[sel]].tolist(), tgt[sel].tolist()):
                    out.setdefault(t, []).append(a)
        self._xref = out
        return out

    def refs(self, target):
        return sorted(set(self.xrefs().get(target, [])))
