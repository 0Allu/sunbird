// Patches liblibdiscord_wasm_host.so in place, a port of crates/patcher/src/host.rs: the client-access
// evaluator returns Some(true), and its DT_NEEDED liblog.so becomes libplm.so, the loader the patcher adds
// next to it. Both found from anchors, so a new build only needs a re-run while the code keeps its shape.

export const LIBRARY = 'liblibdiscord_wasm_host.so';
/** The loader's name in the APK. Same length as liblog.so so it fits in .dynstr; the loader needs liblog.so itself. */
export const INJECTED = 'libplm.so';
const REPLACED = 'liblog.so';
/** The access evaluator is the only function referencing the experiment table whose first entry is this. */
const EXPERIMENT = '2026-11-phoenix-app-access';
const EM_AARCH64 = 183, EM_X86_64 = 62;
const RELATIVE = { [EM_AARCH64]: 1027, [EM_X86_64]: 8 };
// Some(true): an Option<bool> in the low byte of the return register
const RETURN_TRUE = { [EM_AARCH64]: [0x20, 0x00, 0x80, 0x52, 0xc0, 0x03, 0x5f, 0xd6], [EM_X86_64]: [0xb0, 0x01, 0xc3] };
const te = new TextEncoder();

/** e_machine of an ELF64 file (183 arm64, 62 x86_64), or throws. */
export function elfMachine(u8, path = 'library') {
  const v = new DataView(u8.buffer, u8.byteOffset, u8.byteLength);
  if (u8.length < 64 || v.getUint32(0) !== 0x7f454c46 || u8[4] !== 2) throw new Error(`${path}: not an ELF64 file`);
  const m = v.getUint16(18, true);
  if (!RELATIVE[m]) throw new Error(`${path}: unsupported e_machine ${m}`);
  return m;
}

/**
 * Patch in place. `needed` replaces DT_NEEDED liblog.so (same length): the loader, the Frida gadget when the
 * plugin runtime is off, or null to keep it. Returns {machine, access} (e_machine and the evaluator's address).
 */
export function patchHost(path, u8, needed = INJECTED) {
  const elf = new Elf(u8, path);
  const name = needed && elf.neededName();
  const access = elf.accessEvaluator();
  const at = elf.fileOffset(access);
  if (at === undefined) throw new Error(`${path}: access evaluator has no file backing`);
  if (needed) u8.set(te.encode(needed), name);
  u8.set(RETURN_TRUE[elf.machine], at);
  return { machine: elf.machine, access };
}

/** A copy of library `u8` whose DT_NEEDED liblog.so names `needed` instead (same length). */
export function renameNeeded(path, u8, needed) {
  if (needed.length !== REPLACED.length) throw new Error(`${needed} must be as long as ${REPLACED}`);
  const out = u8.slice();
  out.set(te.encode(needed), new Elf(out, path).neededName());
  return out;
}

// The file viewed the way crates/sigs' ElfFile maps it: PT_LOAD segments at their link addresses with
// R_*_RELATIVE addends applied (kept in a map instead of a relocated copy, the host lib is ~100 MB).
class Elf {
  constructor(u8, path) {
    this.u8 = u8;
    this.path = path;
    const v = (this.v = new DataView(u8.buffer, u8.byteOffset, u8.byteLength));
    this.machine = elfMachine(u8, path);
    const n = (o) => Number(v.getBigUint64(o, true));
    const phoff = n(32), shoff = n(40), phentsize = v.getUint16(54, true), phnum = v.getUint16(56, true);
    const shentsize = v.getUint16(58, true), shnum = v.getUint16(60, true), shstrndx = v.getUint16(62, true);
    this.loads = [];
    for (let i = 0; i < phnum; i++) {
      const b = phoff + i * phentsize, type = v.getUint32(b, true);
      if (type === 1) this.loads.push({ vaddr: n(b + 16), offset: n(b + 8), filesz: n(b + 32), memsz: n(b + 40), exec: (v.getUint32(b + 4, true) & 1) !== 0 });
      else if (type === 0x6474e550 && this.ehFrameHdr === undefined) this.ehFrameHdr = n(b + 16); // PT_GNU_EH_FRAME
    }
    const secs = [];
    for (let i = 0; i < shnum; i++) {
      const b = shoff + i * shentsize;
      secs.push({ name: v.getUint32(b, true), type: v.getUint32(b + 4, true), addr: n(b + 16), off: n(b + 24), size: n(b + 32) });
    }
    const strtab = secs[shstrndx].off;
    for (const s of secs) s.name = new TextDecoder().decode(u8.subarray(strtab + s.name, u8.indexOf(0, strtab + s.name)));
    this.secs = secs;
    this.relocs = new Map();
    for (const s of secs) {
      if (s.type === 4) this.rela(s); // SHT_RELA
      else if (s.type === 0x60000002) this.aps2(s); // SHT_ANDROID_RELA
    }
  }

  fail(what) { throw new Error(`${this.path}: ${what}`); }
  section(name) { return this.secs.find((s) => s.name === name) ?? this.fail(`no ${name} section`); }
  /** File offset of link address `va`, when it has file backing. */
  fileOffset(va) {
    const l = this.loads.find((l) => va >= l.vaddr && va < l.vaddr + l.filesz);
    return l && va - l.vaddr + l.offset;
  }
  /** u64 at `va` in the relocated image (0 past the file-backed part, undefined outside every segment). */
  u64(va) {
    if (this.relocs.has(va)) return this.relocs.get(va);
    if (!this.loads.some((l) => va >= l.vaddr && va + 8 <= l.vaddr + l.memsz)) return undefined;
    const o = this.fileOffset(va);
    return o === undefined ? 0 : Number(this.v.getBigUint64(o, true));
  }
  i32(va) { const o = this.fileOffset(va); return o === undefined ? undefined : this.v.getInt32(o, true); }

  rela({ off, size }) {
    const rel = RELATIVE[this.machine];
    for (let o = off; o + 24 <= off + size; o += 24)
      if (this.v.getUint32(o + 8, true) === rel) this.relocs.set(Number(this.v.getBigUint64(o, true)), Number(this.v.getBigUint64(o + 16, true)));
  }

  // Android's packed format: APS2 then SLEB128 groups of offset deltas, info and addend deltas
  aps2({ off, size }) {
    const d = this.u8, end = off + size, rel = BigInt(RELATIVE[this.machine]);
    if (te.encode('APS2').some((b, i) => d[off + i] !== b)) this.fail('packed relocations without APS2');
    let p = off + 4;
    const next = () => {
      let r = 0n, s = 0n, b;
      do { if (p >= end) this.fail('truncated APS2 stream'); b = d[p++]; r |= BigInt(b & 0x7f) << s; s += 7n; } while (b & 0x80);
      return b & 0x40 && s < 64n ? r - (1n << s) : r;
    };
    const count = next();
    let offset = next(), addend = 0n;
    for (let done = 0n; done < count;) {
      const group = next(), flags = next();
      const delta = flags & 2n ? next() : 0n;
      let info = flags & 1n ? next() : 0n;
      if (flags & 8n && flags & 4n) addend += next();
      for (let k = 0n; k < group; k++, done++) {
        offset += flags & 2n ? delta : next();
        if (!(flags & 1n)) info = next();
        if (flags & 8n && !(flags & 4n)) addend += next();
        if ((info & 0xffffffffn) === rel) this.relocs.set(Number(offset), Number(BigInt.asUintN(64, flags & 8n ? addend : 0n)));
      }
    }
  }

  /** File offset of the liblog.so string the one DT_NEEDED entry naming it points at. */
  neededName() {
    const dyn = this.section('.dynamic'), str = this.section('.dynstr'), want = te.encode(REPLACED + '\0');
    const hits = [];
    for (let o = dyn.off; o + 16 <= dyn.off + dyn.size; o += 16) {
      if (this.v.getBigUint64(o, true) !== 1n) continue; // DT_NEEDED
      const at = str.off + Number(this.v.getBigUint64(o + 8, true));
      if (at < str.off + str.size && want.every((b, i) => this.u8[at + i] === b)) hits.push(at);
    }
    if (hits.length !== 1) this.fail(`expected one DT_NEEDED ${REPLACED}, found ${hits.length}`);
    return hits[0];
  }

  accessEvaluator() {
    const slots = new Set();
    for (const s of this.find(te.encode(EXPERIMENT)))
      for (const slot of this.pointersTo(s)) if (this.u64(slot + 8) === EXPERIMENT.length) slots.add(slot);
    const text = this.section('.text');
    const fns = new Set();
    for (const [at, target] of this.references(text)) {
      if (!slots.has(target)) continue;
      const f = this.functionContaining(at);
      if (f !== undefined) fns.add(f);
    }
    if (fns.size !== 1) this.fail(`access evaluator: expected one function referencing ${EXPERIMENT}, found [${[...fns].map((f) => f.toString(16))}]`);
    return [...fns][0];
  }

  /** Link addresses of `needle` in every segment's file bytes (.rodata often shares the executable one). */
  find(needle) {
    const out = [];
    for (const l of this.loads) {
      const end = l.offset + l.filesz - needle.length;
      outer: for (let i = this.u8.indexOf(needle[0], l.offset); i !== -1 && i <= end; i = this.u8.indexOf(needle[0], i + 1)) {
        for (let j = 1; j < needle.length; j++) if (this.u8[i + j] !== needle[j]) continue outer;
        out.push(i - l.offset + l.vaddr);
      }
    }
    return out;
  }

  /** Pointer-aligned slots in non-executable segments that hold `target`, relocated or literal. */
  pointersTo(target) {
    const out = [];
    for (const [slot, value] of this.relocs) if (value === target && slot % 8 === 0 && this.loads.some((l) => !l.exec && slot >= l.vaddr && slot < l.vaddr + l.memsz)) out.push(slot);
    const lo = target % 2 ** 32, hi = Math.floor(target / 2 ** 32);
    for (const l of this.loads.filter((l) => !l.exec))
      for (let va = Math.ceil(l.vaddr / 8) * 8; va + 8 <= l.vaddr + l.filesz; va += 8) {
        const o = va - l.vaddr + l.offset;
        if (this.v.getUint32(o, true) === lo && this.v.getUint32(o + 4, true) === hi && !this.relocs.has(va)) out.push(va);
      }
    return out;
  }

  /**
   * [instruction, target] for every PC-relative address the code forms. On arm64 that is adrp followed within
   * four instructions by an add or 64-bit ldr on the same register; on x86_64 a REX.W lea or mov from [rip + disp32].
   */
  *references({ addr: base, off, size }) {
    if (this.machine === EM_AARCH64) {
      const n = Math.floor(size / 4), word = (i) => this.v.getUint32(off + 4 * i, true);
      for (let i = 0; i < n; i++) {
        const w = word(i);
        if ((w & 0x9f000000) >>> 0 !== 0x90000000) continue;
        let imm = ((w >>> 29) & 3) | (((w >>> 5) & 0x7ffff) << 2);
        if (imm & (1 << 20)) imm -= 1 << 21;
        const pc = base + 4 * i, page = Math.floor(pc / 4096) * 4096 + imm * 4096, reg = w & 0x1f;
        for (let k = i + 1; k < Math.min(i + 5, n); k++) {
          const next = word(k);
          if (((next >>> 5) & 0x1f) !== reg) continue;
          const imm12 = (next >>> 10) & 0xfff, op = (next & 0xffc00000) >>> 0;
          if (op === 0x91000000) yield [pc, page + imm12];
          else if (op === 0xf9400000) yield [pc, page + imm12 * 8];
        }
      }
    } else {
      const c = this.u8;
      for (let at = off; at < off + size - 7; at++) {
        if ((c[at] === 0x48 || c[at] === 0x4c) && (c[at + 1] === 0x8b || c[at + 1] === 0x8d) && (c[at + 2] & 0xc7) === 0x05)
          yield [base + at - off, base + at - off + 7 + this.v.getInt32(at + 3, true)];
      }
    }
  }

  /** Start of the last FDE in .eh_frame_hdr's search table at or below `va`. */
  functionContaining(va) {
    const hdr = this.ehFrameHdr, o = hdr === undefined ? undefined : this.fileOffset(hdr);
    if (o === undefined) return undefined;
    const [version, frameEnc, countEnc, tableEnc] = this.u8.subarray(o, o + 4);
    if (version !== 1 || countEnc !== 0x03 || tableEnc !== 0x3b || ![0x03, 0x0b].includes(frameEnc & 0x0f)) return undefined;
    const start = (i) => hdr + this.i32(hdr + 12 + i * 8);
    let low = 0, high = this.i32(hdr + 8);
    while (low < high) {
      const mid = low + ((high - low) >> 1);
      if (start(mid) <= va) low = mid + 1;
      else high = mid;
    }
    return low ? start(low - 1) : undefined;
  }
}
