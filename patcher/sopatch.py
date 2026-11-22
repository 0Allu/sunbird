"""Find and patch the Phoenix access gate and the staff check in liblibdiscord_wasm_host.so.

Everything is located from anchors rather than fixed offsets, so a new Discord build only needs
a re-run as long as the logic keeps its shape:

access: the client_access evaluator is the only function that references the experiment table
        whose first entry is {"2026-11-phoenix-app-access", 26}. It is patched to return Some(true).
staff:  The Dev Tools settings builder (references "Show Dev Tools Widget") calls, in order,
        get_state, dev_analytics_env (references "LIBDISCORD_DEV_ANALYTICS"), then is_staff.
        From is_staff we read the store key, read_state, guard drop and the flag_override
        (Option<u32>) tag/value offsets, then overwrite its body with a hook that sets
        flag_override = Some(STAFF) on the shared state and returns true. All ~36 staff checks,
        inlined or not, compute (user.flags | flag_override) & 1, so they all agree.
"""
import os
import re
import shutil
import struct
import subprocess
import tempfile

from binfo import Bin

EXPERIMENT = '2026-11-phoenix-app-access'
DEVTOOLS_ROW = 'Show Dev Tools Widget'
DEV_ENV = 'LIBDISCORD_DEV_ANALYTICS'
RET_TRUE = {'x86_64': bytes.fromhex('b001c3'),            # mov al,1; ret
            'arm64': bytes.fromhex('20008052c0035fd6')}   # mov w0,#1; ret


class NotFound(Exception):
    pass


def one(xs, what):
    xs = sorted(set(xs))
    if len(xs) != 1:
        raise NotFound(f'{what}: expected 1 match, got {[hex(x) if isinstance(x, int) else x for x in xs]}')
    return xs[0]


# --- call target helpers ---------------------------------------------------------------
def call_target(b, ins):
    """('direct', addr) | ('got', slot) for a call instruction, else None."""
    if b.arch == 'x86_64':
        if ins.mnemonic != 'call':
            return None
        m = re.fullmatch(r'qword ptr \[rip ([+-]) 0x([0-9a-f]+)\]', ins.op_str)
        if m:
            return 'got', ins.address + ins.size + int(m.group(2), 16) * (1 if m.group(1) == '+' else -1)
        if re.fullmatch(r'0x[0-9a-f]+', ins.op_str):
            return 'direct', int(ins.op_str, 16)
        return None
    if ins.mnemonic == 'bl':
        return 'direct', int(ins.op_str.lstrip('#'), 16)
    return None


def resolve(b, ct):
    kind, a = ct
    return a if kind == 'direct' else b.ptr(a)


def calls(b, a, end):
    return [(i.address, call_target(b, i)) for i in b.dis(a, end) if call_target(b, i)]


# --- finders ---------------------------------------------------------------------------
def find_access(b):
    hits = []
    for s in b.find_str(EXPERIMENT):
        for slot in b.slots_to(s):
            if b.q(slot + 8) != len(EXPERIMENT):
                continue
            hits += [b.func(r)[0] for r in b.refs(slot)]
    return one(hits, 'access evaluator')


def references(b, fstart, saddrs):
    f, e = fstart, b.func(fstart)[1]
    return any(f <= r < e for s in saddrs for r in b.refs(s))


def find_is_staff(b):
    row = b.find_str(DEVTOOLS_ROW)
    env = b.find_str(DEV_ENV)
    builders = {b.func(r)[0]: r for s in row for r in b.refs(s)}
    found = []
    for f, ref in builders.items():
        cs = [resolve(b, ct) for _, ct in calls(b, f, ref)]
        for i, t in enumerate(cs[:-1]):
            if t and references(b, t, env):
                found.append(cs[i + 1])
    return one(found, 'is_staff')


def parse_is_staff(b, f):
    """Pull out key, read_state, drop and flag_override offsets from the original is_staff."""
    e = b.func(f)[1]
    ins = b.dis(f, e)
    cs = [call_target(b, i) for i in ins if call_target(b, i)]
    if len(cs) != 2:
        raise NotFound(f'is_staff @{f:#x}: expected 2 calls (read_state, drop), got {len(cs)}')
    out = {'func': f, 'size': e - f, 'read': cs[0], 'drop': cs[1]}
    txt = [(i.mnemonic, i.op_str) for i in ins]
    if b.arch == 'x86_64':
        keys = [int(m.group(2), 16) * (1 if m.group(1) == '+' else -1) + i.address + i.size
                for i in ins if i.mnemonic == 'lea' and i.op_str.startswith('rdx,')
                for m in [re.search(r'rip ([+-]) 0x([0-9a-f]+)', i.op_str)] if m]
        out['key'] = one(keys, 'is_staff key')
        tag = [int(m.group(1), 16) for mn, op in txt if mn == 'cmp'
               for m in [re.fullmatch(r'byte ptr \[rax \+ 0x([0-9a-f]+)\], 0', op)] if m]
        out['tag'] = one(tag, 'flag_override tag')
        # value: the dword load right after the tag compare (mov esi, dword ptr [rax + V])
        k = next(i for i, (mn, op) in enumerate(txt) if mn == 'cmp' and f'0x{out["tag"]:x}]' in op)
        val = [int(m.group(1), 16) for mn, op in txt[k:k + 4] if mn == 'mov'
               for m in [re.fullmatch(r'e\w\w, dword ptr \[rax \+ 0x([0-9a-f]+)\]', op)] if m]
        out['val'] = one(val, 'flag_override value')
    else:
        # adrp x1, page; add x1, x1, #lo  -> key
        key = None
        for a, c in zip(ins, ins[1:]):
            if a.mnemonic == 'adrp' and a.op_str.startswith('x1,') and c.mnemonic == 'add' and c.op_str.startswith('x1, x1,'):
                key = int(a.op_str.split('#')[1], 16) + int(c.op_str.split('#')[1], 16)
        if key is None:
            raise NotFound('is_staff key (adrp/add x1)')
        out['key'] = key
        # base: add xB, xB, #N, lsl #12 ; then ldr wT,[xB,#t] ; ldr wV,[xB,#v] ; ... cmp wT,#0 ; csel ?, wV, wzr, ne
        base = [int(m.group(1), 16) << 12 for mn, op in txt if mn == 'add'
                for m in [re.fullmatch(r'x\d+, x\d+, #(0x[0-9a-f]+|\d+), lsl #12', op)] if m]
        base = one([int(str(x), 0) for x in base], 'state field base')
        loads = {m.group(1): int(m.group(2), 16) for mn, op in txt if mn == 'ldr'
                 for m in [re.fullmatch(r'(w\d+), \[x\d+, #0x([0-9a-f]+)\]', op)] if m}
        tagreg = one([m.group(1) for mn, op in txt if mn == 'cmp'
                      for m in [re.fullmatch(r'(w\d+), #0', op)] if m and m.group(1) in loads], 'tag reg')
        k = txt.index(('cmp', f'{tagreg}, #0'))
        mn, op = txt[k + 1]  # the csel consuming that compare selects the override value
        m = re.fullmatch(r'w\d+, (w\d+), wzr, ne', op) if mn == 'csel' else None
        if not m or m.group(1) not in loads:
            raise NotFound(f'flag_override value: unexpected {mn} {op} after tag compare')
        valreg = m.group(1)
        out['tag'] = base + loads[tagreg]
        out['val'] = base + loads[valreg]
    return out


# --- hook assembly ---------------------------------------------------------------------
HOOK_X86 = r'''
.text
.globl hook
hook:
  push %rbx
  sub $0x40, %rsp
  mov %rdi, %rsi
  mov %rsp, %rdi
  lea KEY(%rip), %rdx
  {READ}
  mov (%rsp), %rax
  test %rax, %rax
  je 1f
  movb $1, {TAG}(%rax)
  orl $1, {VAL}(%rax)
1:
  mov %rsp, %rdi
  {DROP}
  mov $1, %al
  add $0x40, %rsp
  pop %rbx
  ret
'''
HOOK_ARM64 = r'''
.text
.globl hook
hook:
  sub sp, sp, #0x40
  stp x29, x30, [sp, #0x20]
  add x29, sp, #0x20
  adrp x1, KEY
  add x1, x1, :lo12:KEY
  mov x8, sp
  {READ}
  ldr x9, [sp]
  cbz x9, 1f
  mov x10, #{TAG}
  mov w11, #1
  strb w11, [x9, x10]
  mov x10, #{VAL}
  ldr w11, [x9, x10]
  orr w11, w11, #1
  str w11, [x9, x10]
1:
  mov x0, sp
  {DROP}
  mov w0, #1
  ldp x29, x30, [sp, #0x20]
  add sp, sp, #0x40
  ret
'''


def _call_asm(arch, ct, name):
    kind, a = ct
    if arch == 'x86_64':
        return f'call *{name}(%rip)' if kind == 'got' else f'call {name}'
    if kind == 'got':
        return f'adrp x16, {name}\n  ldr x16, [x16, :lo12:{name}]\n  blr x16'
    return f'bl {name}'


def assemble_hook(arch, info, ndk_bin):
    src = (HOOK_X86 if arch == 'x86_64' else HOOK_ARM64).format(
        READ=_call_asm(arch, info['read'], 'READ'), DROP=_call_asm(arch, info['drop'], 'DROP'),
        TAG=hex(info['tag']), VAL=hex(info['val']))
    target = 'x86_64-linux-android' if arch == 'x86_64' else 'aarch64-linux-android'
    clang = shutil.which('clang', path=ndk_bin) or os.path.join(ndk_bin, 'clang.exe')
    objcopy = shutil.which('llvm-objcopy', path=ndk_bin) or os.path.join(ndk_bin, 'llvm-objcopy.exe')
    with tempfile.TemporaryDirectory() as t:
        s, elf, raw = (os.path.join(t, n) for n in ('hook.S', 'hook.elf', 'hook.bin'))
        open(s, 'w').write(src)
        syms = {'KEY': info['key'], 'READ': info['read'][1], 'DROP': info['drop'][1]}
        cmd = [clang, f'--target={target}', '-nostdlib', '-static', '-fuse-ld=lld', '-Wl,-e,hook',
               f'-Wl,-Ttext={info["func"]:#x}', '-Wl,--no-relax', s, '-o', elf] + \
              [f'-Wl,--defsym={k}={v:#x}' for k, v in syms.items()]
        subprocess.run(cmd, check=True)
        subprocess.run([objcopy, '-O', 'binary', '--only-section=.text', elf, raw], check=True)
        code = open(raw, 'rb').read()
    if len(code) > info['size']:
        raise NotFound(f'hook is {len(code)} bytes, is_staff only {info["size"]}')
    return code


# --- entry -----------------------------------------------------------------------------
def patch_so(data, ndk_bin, log=print):
    b = Bin(data)
    acc = find_access(b)
    b.write(acc, RET_TRUE[b.arch])
    log(f'[{b.arch}] access evaluator @ {acc:#x} -> return Some(true)')
    st = parse_is_staff(b, find_is_staff(b))
    code = assemble_hook(b.arch, st, ndk_bin)
    b.write(st['func'], code)
    log(f'[{b.arch}] is_staff @ {st["func"]:#x}: key={st["key"]:#x} read={st["read"][0]}:{st["read"][1]:#x} '
        f'drop={st["drop"][0]}:{st["drop"][1]:#x} flag_override tag=+{st["tag"]:#x} val=+{st["val"]:#x} '
        f'-> hook ({len(code)} bytes)')
    return bytes(b.d), {'arch': b.arch, 'access': acc, **st}
