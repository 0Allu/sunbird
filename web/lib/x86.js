// x86_64 counterpart of arm64.js: the same anchors as patcher/sopatch.py's x86_64 path, with a small
// instruction-length decoder standing in for capstone and the hook encoded by hand (same bytes clang emits).
import { Elf, one } from './arm64.js';

const EXPERIMENT = '2026-11-phoenix-app-access';
const DEVTOOLS_ROW = 'Show Dev Tools Widget';
const DEV_ENV = 'LIBDISCORD_DEV_ANALYTICS';

class X86 extends Elf {
  // lea/mov r64,[rip+d32] and call/jmp [rip+d32] -> target; same non-overlapping scan as binfo.py
  xrefs() {
    if (this._x) return this._x;
    const { addr, off, size } = this.sec['.text'];
    const t = this.u8, out = new Map();
    for (let i = off; i < off + size - 6;) {
      const b = t[i];
      let n = 0;
      if ((b === 0x48 || b === 0x4c) && (t[i + 1] === 0x8d || t[i + 1] === 0x8b) && (t[i + 2] & 0xc7) === 0x05) n = 3;
      else if (b === 0xff && (t[i + 1] === 0x15 || t[i + 1] === 0x25)) n = 2;
      if (!n) { i++; continue; }
      const pc = addr + i - off, tgt = pc + n + 4 + this.v.getInt32(i + n, true);
      if (!out.has(tgt)) out.set(tgt, []);
      out.get(tgt).push(pc);
      i += n;
    }
    return (this._x = out);
  }

  // linear sweep: [{pc, len, op, modrm, disp, imm, call}] where call = ['direct'|'got', addr]
  dis(a, end) {
    const r = [];
    for (let pc = a; pc < end;) { const ins = decode(this.u8, this.off(pc), pc); r.push(ins); pc += ins.len; }
    return r;
  }
  calls(a, end) { return this.dis(a, end).filter((i) => i.call).map((i) => i.call); }
  resolve([kind, a]) { return kind === 'direct' ? a : this.relocs().get(a); }
}

// --- instruction length decoder (general-purpose + SSE, no VEX) -------------------------
// one-byte opcodes with a ModRM byte, and their immediate size (z = 4, or 2 with 0x66)
const MODRM1 = new Set([...range(0x00, 0x40).filter((o) => (o & 7) < 4), 0x63, 0x69, 0x6b, ...range(0x80, 0x90), 0xc0, 0xc1, 0xc6, 0xc7, 0xd0, 0xd1, 0xd2, 0xd3, 0xf6, 0xf7, 0xfe, 0xff]);
function range(a, b) { return Array.from({ length: b - a }, (_, i) => a + i); }

function decode(u8, o, pc) {
  const s = o;
  let rex = 0, opsz = false;
  for (;;) {
    const b = u8[o];
    if (b === 0x66) opsz = true;
    else if (![0x67, 0xf2, 0xf3, 0xf0, 0x2e, 0x3e, 0x26, 0x36, 0x64, 0x65].includes(b)) break;
    o++;
  }
  if ((u8[o] & 0xf0) === 0x40) rex = u8[o++];
  const op = u8[o++];
  const z = opsz ? 2 : 4;
  let modrm = false, imm = 0, rel = 0;
  if (op === 0x0f) {
    const op2 = u8[o++];
    if (op2 === 0x38) { o++; modrm = true; }
    else if (op2 === 0x3a) { o++; modrm = true; imm = 1; }
    else if (op2 >= 0x80 && op2 <= 0x8f) rel = 4;
    else if ([0x05, 0x0b, 0xa2, 0x31].includes(op2) || (op2 >= 0xc8 && op2 <= 0xcf)) { /* no operands */ }
    else { modrm = true; if ([0x70, 0x71, 0x72, 0x73, 0xa4, 0xac, 0xba, 0xc2, 0xc4, 0xc5, 0xc6].includes(op2)) imm = 1; }
    return finish(u8, s, o, pc, { op: 0x0f00 | op2, rex, modrm, imm, rel });
  }
  if (MODRM1.has(op)) {
    modrm = true;
    if ([0x80, 0x83, 0x6b, 0xc0, 0xc1, 0xc6].includes(op)) imm = 1;
    else if ([0x81, 0x69, 0xc7].includes(op)) imm = z;
    else if (op === 0xf6 || op === 0xf7) imm = ((u8[o] >> 3) & 7) < 2 ? (op === 0xf6 ? 1 : z) : 0;
  } else if (op < 0x40 && (op & 7) === 4) imm = 1;
  else if (op < 0x40 && (op & 7) === 5) imm = z;
  else if ((op >= 0x50 && op <= 0x5f) || (op >= 0x90 && op <= 0x9f && op !== 0x9a) || [0xc3, 0xc9, 0xcc, 0xf4].includes(op)) { /* none */ }
  else if (op === 0x68) imm = z;
  else if (op === 0x6a || op === 0xa8 || (op >= 0xb0 && op <= 0xb7)) imm = 1;
  else if (op === 0xa9) imm = z;
  else if (op >= 0xb8 && op <= 0xbf) imm = rex & 8 ? 8 : z;
  else if (op === 0xc2) imm = 2;
  else if ((op >= 0x70 && op <= 0x7f) || op === 0xeb) rel = 1;
  else if (op === 0xe8 || op === 0xe9) rel = 4;
  else throw new Error(`x86: unknown opcode 0x${op.toString(16)} at 0x${pc.toString(16)}`);
  return finish(u8, s, o, pc, { op, rex, modrm, imm, rel });
}

function finish(u8, s, o, pc, ins) {
  let disp = null, rip = false, m = null;
  if (ins.modrm) {
    m = u8[o++];
    const mod = m >> 6, rm = m & 7;
    let base = rm;
    if (mod !== 3 && rm === 4) base = u8[o++] & 7;
    if (mod === 1) { disp = (u8[o] << 24) >> 24; o += 1; }
    else if (mod === 2 || (mod === 0 && base === 5)) { disp = u8[o] | (u8[o + 1] << 8) | (u8[o + 2] << 16) | (u8[o + 3] << 24); o += 4; rip = mod === 0 && rm === 5; }
  }
  const immAt = o;
  o += ins.imm;
  let call = null;
  const len = o + ins.rel - s;
  if (ins.op === 0xe8) call = ['direct', pc + len + (u8[o] | (u8[o + 1] << 8) | (u8[o + 2] << 16) | (u8[o + 3] << 24))];
  if (ins.op === 0xff && m === 0x15) call = ['got', pc + len + disp];
  return { pc, len, op: ins.op, rex: ins.rex, modrm: m, disp, rip, imm: ins.imm ? u8[immAt] : null, call };
}

// --- finders (sopatch.py) --------------------------------------------------------------
function findAccess(e) {
  const hits = [];
  for (const s of e.findStr(EXPERIMENT))
    for (const slot of e.slotsTo(s))
      if (e.u64(slot + 8) === EXPERIMENT.length) for (const r of e.refs(slot)) hits.push(e.func(r)[0]);
  return one(hits, 'access evaluator');
}

function findIsStaff(e) {
  const env = e.findStr(DEV_ENV);
  const refsIn = (f) => { const [a, b] = e.func(f); return env.some((s) => e.refs(s).some((r) => r >= a && r < b)); };
  const builders = new Map();
  for (const s of e.findStr(DEVTOOLS_ROW)) for (const r of e.refs(s)) builders.set(e.func(r)[0], r);
  const found = [];
  for (const [f, ref] of builders) {
    const cs = e.calls(f, ref).map((c) => e.resolve(c));
    for (let i = 0; i + 1 < cs.length; i++) if (cs[i] && refsIn(cs[i])) found.push(cs[i + 1]);
  }
  return one(found, 'is_staff');
}

function parseIsStaff(e, f) {
  const [, end] = e.func(f);
  const ins = e.dis(f, end);
  const cs = ins.filter((i) => i.call).map((i) => i.call);
  if (cs.length !== 2) throw new Error(`is_staff: expected 2 calls, got ${cs.length}`);
  // lea rdx, [rip+KEY]
  const key = one(ins.filter((i) => i.op === 0x8d && i.rip && (i.rex & 0xc) === 8 && ((i.modrm >> 3) & 7) === 2).map((i) => i.pc + i.len + i.disp), 'is_staff key');
  // cmp byte ptr [rax+TAG], 0 ; then within 4 instructions mov e??, dword ptr [rax+VAL]
  const isTag = (i) => i.op === 0x80 && (i.modrm & 0xc7) !== 0xc0 && ((i.modrm >> 3) & 7) === 7 && (i.modrm & 7) === 0 && i.modrm >> 6 && i.disp > 0 && i.imm === 0;
  const tag = one(ins.filter(isTag).map((i) => i.disp), 'flag_override tag');
  const k = ins.findIndex((i) => isTag(i) && i.disp === tag);
  const val = one(ins.slice(k, k + 4).filter((i) => i.op === 0x8b && !(i.rex & 0xc) && (i.modrm & 7) === 0 && i.modrm >> 6 && i.modrm >> 6 !== 3 && i.disp > 0).map((i) => i.disp), 'flag_override value');
  return { func: f, size: end - f, key, read: cs[0], drop: cs[1], tag, val };
}

// --- hook (sopatch.py HOOK_X86, as clang assembles it) ---------------------------------
const le32 = (n) => [n & 0xff, (n >> 8) & 0xff, (n >> 16) & 0xff, (n >>> 24) & 0xff];
// [rax + d] with ModRM reg field r
const raxDisp = (r, d) => (d === 0 ? [r << 3] : d >= -128 && d < 128 ? [0x40 | (r << 3), d & 0xff] : [0x80 | (r << 3), ...le32(d)]);

function hook(s) {
  const b = [];
  const ripRel = (pre, t) => { b.push(...pre); b.push(...le32(t - (s.func + b.length + 4))); };
  const call = ([kind, t]) => ripRel(kind === 'got' ? [0xff, 0x15] : [0xe8], t);
  b.push(0x53, 0x48, 0x83, 0xec, 0x40, 0x48, 0x89, 0xfe, 0x48, 0x89, 0xe7); // push rbx; sub rsp,0x40; mov rsi,rdi; mov rdi,rsp
  ripRel([0x48, 0x8d, 0x15], s.key);                                       // lea rdx,[rip+KEY]
  call(s.read);
  b.push(0x48, 0x8b, 0x04, 0x24, 0x48, 0x85, 0xc0);                         // mov rax,[rsp]; test rax,rax
  const set = [0xc6, ...raxDisp(0, s.tag), 1, 0x83, ...raxDisp(1, s.val), 1]; // movb $1,TAG(%rax); orl $1,VAL(%rax)
  b.push(0x74, set.length, ...set);                                        // je 1f
  b.push(0x48, 0x89, 0xe7);                                                // 1: mov rdi,rsp
  call(s.drop);
  b.push(0xb0, 0x01, 0x48, 0x83, 0xc4, 0x40, 0x5b, 0xc3);                   // mov al,1; add rsp,0x40; pop rbx; ret
  return Uint8Array.from(b);
}

/** Patch in place. Returns what was found. */
export function patchHostLib(u8, log = () => {}) {
  const e = new X86(u8);
  if (e.rtype !== 8) throw new Error('not an x86_64 library');
  const acc = findAccess(e);
  u8.set([0xb0, 0x01, 0xc3], e.off(acc)); // mov al,1; ret
  log(`access evaluator @ 0x${acc.toString(16)} -> Some(true)`);
  const s = parseIsStaff(e, findIsStaff(e));
  const code = hook(s);
  if (code.length > s.size) throw new Error('hook does not fit in is_staff');
  u8.set(code, e.off(s.func));
  log(`is_staff @ 0x${s.func.toString(16)} (flag_override +0x${s.tag.toString(16)}/+0x${s.val.toString(16)}) -> staff hook`);
  return { access: acc, ...s };
}
