//! x86-64 disassembly, and on Windows live entry hooks that report a call's
//! argument registers, for the debug tools.

#[cfg(windows)] use std::ffi::c_void;
use std::ops::Range;

#[cfg(windows)]
use iced_x86::{
   BlockEncoder,
   BlockEncoderOptions,
   FlowControl,
   InstructionBlock,
};
use iced_x86::{
   Decoder,
   DecoderOptions,
   Formatter as _,
   Instruction,
   IntelFormatter,
};

use crate::image::Image;
#[cfg(windows)]
use crate::{
   Error,
   image::{
      ABSOLUTE_JUMP_LEN,
      absolute_jump,
      win32,
   },
};

/// One decoded instruction.
pub struct Line {
   pub addr:   usize,
   pub len:    usize,
   pub text:   String,
   /// The address a RIP-relative operand or a near branch points at.
   pub target: Option<usize>,
}

/// A decoded address. Only built for 64-bit hosts, where it always fits.
#[expect(clippy::cast_possible_truncation, reason = "x86_64 hosts only")]
const fn addr(value: u64) -> usize {
   value as usize
}

/// Up to `count` instructions decoded from `code`, which starts at `ip`. Stops
/// early at bytes that do not decode.
#[must_use]
pub fn disasm(code: &[u8], ip: usize, count: usize) -> Vec<Line> {
   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut formatter = IntelFormatter::new();
   let mut instruction = Instruction::default();
   let mut lines = Vec::new();
   let mut offset = 0;
   while lines.len() < count && decoder.can_decode() {
      decoder.decode_out(&mut instruction);
      if instruction.is_invalid() {
         break;
      }
      let mut text = String::new();
      formatter.format(&instruction, &mut text);
      let target = if instruction.is_ip_rel_memory_operand() {
         Some(instruction.ip_rel_memory_address())
      } else if instruction.near_branch_target() != 0 {
         Some(instruction.near_branch_target())
      } else {
         None
      };
      lines.push(Line {
         addr: ip + offset,
         len: instruction.len(),
         text,
         target: target.and_then(|target| usize::try_from(target).ok()),
      });
      offset += instruction.len();
   }
   lines
}

/// The `(offset, size)` stores a function makes through its hidden return
/// pointer, `rcx` on entry.
///
/// Decoding stops at the first `ret`, and a `memcpy` into the pointer counts
/// as one store of the copied length. It shows how large the returned
/// struct is and which fields the constructor sets, so a layout baked into a
/// caller can be checked against a new build. Control flow is followed
/// linearly, which suits the straight-line constructors it is for.
#[must_use]
pub fn sret_stores(code: &[u8], ip: usize) -> Vec<(usize, usize)> {
   use iced_x86::{
      Code,
      InstructionInfoFactory,
      Mnemonic,
      OpAccess,
      OpKind,
      Register,
   };

   const VOLATILE: [Register; 7] = [
      Register::RAX,
      Register::RCX,
      Register::RDX,
      Register::R8,
      Register::R9,
      Register::R10,
      Register::R11,
   ];
   let writes = |access: OpAccess| {
      matches!(
         access,
         OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
      )
   };

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut factory = InstructionInfoFactory::new();
   let mut holders = vec![Register::RCX];
   let mut r8_imm = None;
   let mut stores = Vec::new();
   while decoder.can_decode() {
      let instruction = decoder.decode();
      if instruction.is_invalid() || instruction.mnemonic() == Mnemonic::Ret {
         break;
      }
      if instruction.mnemonic() == Mnemonic::Call {
         if holders.contains(&Register::RCX)
            && let Some(len) = r8_imm
         {
            stores.push((0, len));
         }
         holders.retain(|register| !VOLATILE.contains(register));
         r8_imm = None;
         continue;
      }
      let info = factory.info(&instruction);
      for memory in info.used_memory() {
         if writes(memory.access())
            && holders.contains(&memory.base())
            && memory.index() == Register::None
         {
            stores.push((addr(memory.displacement()), memory.memory_size().size()));
         }
      }
      let copied = matches!(instruction.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
         && instruction.op0_kind() == OpKind::Register
         && instruction.op1_kind() == OpKind::Register
         && holders.contains(&instruction.op1_register());
      let written = info
         .used_registers()
         .iter()
         .filter(|used| writes(used.access()))
         .map(|used| used.register().full_register())
         .collect::<Vec<_>>();
      for register in written {
         holders.retain(|&held| held != register);
         if copied && register == instruction.op0_register() {
            holders.push(register);
         }
         if register == Register::R8 {
            r8_imm = (instruction.mnemonic() == Mnemonic::Mov
               && matches!(
                  instruction.op1_kind(),
                  OpKind::Immediate32 | OpKind::Immediate32to64
               ))
            .then(|| instruction.immediate32() as usize);
         }
      }
   }
   stores
}

/// One `DebugStruct::field(name, &self.field)` call in a derived `Debug::fmt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DebugField {
   /// Address and length of the field name literal.
   pub name:     usize,
   pub name_len: usize,
   /// Offset of the field in `self`.
   pub offset:   usize,
   /// The `&dyn Debug` vtable passed for the field, whose slot 3 is its `fmt`.
   pub vtable:   Option<usize>,
}

/// The fields a derived `Debug::fmt` prints, in code order.
///
/// Each field is `lea rdx, [name]; mov r8d, len; lea r9, [self + off]` (or
/// `mov r9, self` after `add self, off`) with the vtable stored to `[rsp +
/// 0x20]`, then a call. `self` is the register `rcx` is first copied into.
/// Cold blocks are decoded linearly too, so fields printed out of line count.
#[must_use]
#[expect(clippy::too_many_lines, reason = "one match arm per instruction shape")]
pub fn debug_fields(code: &[u8], ip: usize) -> Vec<DebugField> {
   use std::collections::HashMap;

   use iced_x86::{
      Code,
      Mnemonic,
      OpKind,
      Register,
   };

   #[derive(Clone, Copy)]
   enum Value {
      Rip(usize),
      Field(usize),
      Imm(u64),
   }

   let mut decoder = Decoder::with_ip(64, code, ip as u64, DecoderOptions::NONE);
   let mut this = None;
   let mut added = 0usize;
   let mut regs: HashMap<Register, Value> = HashMap::new();
   let mut vtable = None;
   let mut fields = Vec::new();
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let dst = (ins.op_count() > 0 && ins.op0_kind() == OpKind::Register)
         .then(|| ins.op0_register().full_register());
      let to_this = dst.is_some() && dst == this;
      match (ins.mnemonic(), dst) {
         (Mnemonic::Mov, Some(_))
            if this.is_none()
               && matches!(ins.code(), Code::Mov_r64_rm64 | Code::Mov_rm64_r64)
               && ins.op1_kind() == OpKind::Register
               && ins.op1_register() == Register::RCX =>
         {
            this = dst;
         },
         (Mnemonic::Lea, Some(dst)) => {
            let value = if ins.is_ip_rel_memory_operand() {
               Some(Value::Rip(addr(ins.ip_rel_memory_address())))
            } else if Some(ins.memory_base()) == this && ins.memory_index() == Register::None {
               Some(Value::Field(addr(ins.memory_displacement64())))
            } else {
               None
            };
            match value {
               Some(value) => regs.insert(dst, value),
               None => regs.remove(&dst),
            };
         },
         (Mnemonic::Add, Some(_)) if to_this && ins.op1_kind() != OpKind::Register => {
            added = addr(ins.immediate(1));
         },
         (Mnemonic::Inc, Some(_)) if to_this => added = 1,
         (Mnemonic::Mov, Some(dst)) => {
            let value = match ins.op1_kind() {
               OpKind::Immediate32 | OpKind::Immediate32to64 | OpKind::Immediate64 => {
                  Some(Value::Imm(ins.immediate(1)))
               },
               OpKind::Register if Some(ins.op1_register().full_register()) == this => {
                  Some(Value::Field(added))
               },
               OpKind::Register => regs.get(&ins.op1_register().full_register()).copied(),
               _ => None,
            };
            match value {
               Some(value) => regs.insert(dst, value),
               None => regs.remove(&dst),
            };
         },
         (Mnemonic::Mov, None)
            if ins.op0_kind() == OpKind::Memory
               && ins.memory_base() == Register::RSP
               && ins.memory_displacement64() == 0x20
               && ins.op1_kind() == OpKind::Register =>
         {
            vtable = match regs.get(&ins.op1_register().full_register()) {
               Some(Value::Rip(at)) => Some(*at),
               _ => None,
            };
         },
         (Mnemonic::Call, _) => {
            if let (Some(Value::Rip(name)), Some(Value::Imm(len)), Some(Value::Field(offset))) = (
               regs.get(&Register::RDX),
               regs.get(&Register::R8),
               regs.get(&Register::R9),
            ) && (1..64).contains(len)
            {
               fields.push(DebugField {
                  name: *name,
                  name_len: addr(*len),
                  offset: *offset,
                  vtable,
               });
            }
            regs.clear();
            vtable = None;
            added = 0;
         },
         (Mnemonic::Jmp | Mnemonic::Ret, _) => {
            regs.clear();
            vtable = None;
            added = 0;
         },
         _ => {},
      }
   }
   fields
}

/// `(offset, value)` of each `mov byte [..], imm8` into the struct a call
/// just returned through `rcx`.
///
/// `before` ends at the call and holds its `lea rcx, [base + disp]`; `after`
/// starts right behind it and is read up to the next branch or call, through
/// registers loaded with `lea reg, [base + disp]`.
#[must_use]
pub fn sret_byte_stores(
   before: &[u8],
   before_ip: usize,
   after: &[u8],
   after_ip: usize,
) -> Vec<(usize, u8)> {
   use std::collections::HashMap;

   use iced_x86::{
      FlowControl,
      Mnemonic,
      OpKind,
      Register,
   };

   let mut out = None;
   let mut decoder = Decoder::with_ip(64, before, before_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() {
         continue;
      }
      let writes_rcx = ins.op_count() > 0
         && ins.op0_kind() == OpKind::Register
         && ins.op0_register().full_register() == Register::RCX;
      if writes_rcx {
         out = (ins.mnemonic() == Mnemonic::Lea
            && !ins.is_ip_rel_memory_operand()
            && ins.memory_index() == Register::None)
            .then(|| (ins.memory_base(), ins.memory_displacement64().cast_signed()));
      }
   }
   let Some((base, disp)) = out else {
      return Vec::new();
   };

   let mut aliases: HashMap<Register, i64> = HashMap::from([(base, 0)]);
   let mut stores = Vec::new();
   let mut decoder = Decoder::with_ip(64, after, after_ip as u64, DecoderOptions::NONE);
   while decoder.can_decode() {
      let ins = decoder.decode();
      if ins.is_invalid() || ins.flow_control() != FlowControl::Next {
         break;
      }
      if ins.op_count() > 0 && ins.op0_kind() == OpKind::Register {
         let dst = ins.op0_register().full_register();
         if dst != base {
            aliases.remove(&dst);
            if ins.mnemonic() == Mnemonic::Lea
               && ins.memory_base() == base
               && ins.memory_index() == Register::None
            {
               aliases.insert(dst, ins.memory_displacement64().cast_signed() - disp);
            }
         }
         continue;
      }
      if ins.mnemonic() == Mnemonic::Mov
         && ins.op0_kind() == OpKind::Memory
         && ins.memory_size().size() == 1
         && ins.op1_kind() == OpKind::Immediate8
         && ins.memory_index() == Register::None
         && let Some(&delta) = aliases.get(&ins.memory_base())
      {
         let at = ins.memory_displacement64().cast_signed() + delta
            - if ins.memory_base() == base { disp } else { 0 };
         if let Ok(at) = usize::try_from(at) {
            stores.push((at, ins.immediate8()));
         }
      }
   }
   stores
}

/// An instruction that points at an address, and the function holding it.
pub struct CodeRef {
   pub function: usize,
   pub line:     Line,
}

/// The instructions in `image` whose RIP-relative operand or near branch
/// points into `targets`, confirmed by decoding their function.
#[must_use]
pub fn code_refs(image: &Image<'_>, targets: Range<usize>) -> Vec<CodeRef> {
   image
      .rip_refs(targets)
      .into_iter()
      .filter_map(|(field, target)| {
         let (start, end) = image.function_bounds(field)?;
         let line = disasm(image.read(start, end - start)?, start, usize::MAX)
            .into_iter()
            .find(|line| (line.addr..line.addr + line.len).contains(&field))?;
         (line.target == Some(target)).then_some(CodeRef {
            function: start,
            line,
         })
      })
      .collect()
}

/// How far before a match a string literal can start. Rust merges literals
/// into one blob, so code loads the start of the literal, not the match.
const LITERAL_REACH: usize = 512;

/// The code that loads the string literal holding the bytes at `at`: the
/// loads of the closest literal start at or before it.
#[must_use]
pub fn literal_loads(image: &Image<'_>, at: usize) -> Vec<CodeRef> {
   let mut loads = code_refs(image, at.saturating_sub(LITERAL_REACH)..at + 1);
   let start = loads.iter().filter_map(|load| load.line.target).max();
   loads.retain(|load| load.line.target == start);
   loads
}

/// Copies `len` bytes of this process's memory at `addr`, or the readable
/// start of them. Unmapped memory gives `None` instead of a fault.
#[cfg(windows)]
#[must_use]
pub fn read_memory(addr: usize, len: usize) -> Option<Vec<u8>> {
   let mut buffer = vec![0u8; len];
   let mut read = 0usize;
   // SAFETY: `buffer` holds `len` writable bytes, and ReadProcessMemory
   // checks the source range instead of faulting.
   let ok = unsafe {
      win32::ReadProcessMemory(
         win32::GetCurrentProcess(),
         addr as *const c_void,
         buffer.as_mut_ptr().cast(),
         len,
         &raw mut read,
      )
   };
   (ok != 0 || read > 0).then(|| {
      buffer.truncate(read);
      buffer
   })
}

/// The registers at a hooked function's entry, as the stub saved them.
#[cfg(windows)]
#[repr(C)]
pub struct Regs {
   pub r9:  u64,
   pub r8:  u64,
   pub rdx: u64,
   pub rcx: u64,
   /// The caller's return address.
   pub ret: u64,
}

#[cfg(windows)]
impl Regs {
   /// The first four integer arguments, `rcx`, `rdx`, `r8` and `r9`.
   #[must_use]
   pub const fn args(&self) -> [u64; 4] {
      [self.rcx, self.rdx, self.r8, self.r9]
   }
}

/// Called on every entry of an observed function, on the caller's thread,
/// with the context passed to [`observe`]. It must not unwind.
#[cfg(windows)]
pub type Observer = extern "C" fn(ctx: usize, regs: &Regs);

/// A function [`observe`] redirects through its stub.
#[cfg(windows)]
pub struct Observed {
   addr:     usize,
   original: [u8; ABSOLUTE_JUMP_LEN],
}

#[cfg(windows)]
const STUB_PAGE: usize = 0x1000;
#[cfg(windows)]
const TRAMPOLINE_AT: usize = 0x100;

#[cfg(windows)]
impl Observed {
   #[must_use]
   pub const fn addr(&self) -> usize {
      self.addr
   }

   /// Puts the function's first bytes back. The stub and trampoline stay
   /// allocated, since a thread may still run them.
   ///
   /// # Safety
   ///
   /// As for [`observe`].
   pub unsafe fn remove(&self, image: &Image<'static>) -> Result<(), Error> {
      // SAFETY: forwarded from the caller.
      unsafe { hot_patch(image, self.addr, &self.original) }
   }
}

/// Makes every call to the function at `addr` run `observer(ctx, regs)` first,
/// then the function unchanged.
///
/// The displaced prologue is decoded and relocated, so unlike
/// [`Image::hook_entry`] it needs no known pattern. The entry is patched while
/// other threads run: one that reaches it meanwhile spins on a `jmp $` until
/// the jump is whole.
///
/// # Safety
///
/// `addr` starts a function of `image` that nothing branches back into within
/// its first 14 bytes, and no thread is paused inside those bytes.
#[cfg(windows)]
pub unsafe fn observe(
   image: &Image<'static>,
   addr: usize,
   observer: Observer,
   ctx: usize,
) -> Result<Observed, Error> {
   let refuse = |message: &str| {
      Error::Attach {
         backend: "observe",
         target:  addr,
         message: message.to_owned(),
      }
   };
   if addr % 8 == 7 {
      return Err(refuse(
         "the entry ends an 8-byte word, so it cannot be parked atomically",
      ));
   }
   if let Some((start, end)) = image.function_bounds(addr) {
      if start != addr {
         return Err(refuse("not a function start"));
      }
      if end - addr < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function is shorter than the 14-byte jump"));
      }
   }
   let code = image.read(addr, 32).ok_or(Error::Unmapped { addr })?;
   let original: [u8; ABSOLUTE_JUMP_LEN] = code[..ABSOLUTE_JUMP_LEN]
      .try_into()
      .map_err(|_| Error::Unmapped { addr })?;
   let mut decoder = Decoder::with_ip(64, code, addr as u64, DecoderOptions::NONE);
   let mut displaced = Vec::new();
   let mut covered = 0;
   while covered < ABSOLUTE_JUMP_LEN {
      let instruction = decoder.decode();
      if instruction.is_invalid() {
         return Err(refuse("the prologue does not decode"));
      }
      covered += instruction.len();
      let ends = matches!(
         instruction.flow_control(),
         FlowControl::Return
            | FlowControl::UnconditionalBranch
            | FlowControl::IndirectBranch
            | FlowControl::Interrupt
      );
      if ends && covered < ABSOLUTE_JUMP_LEN {
         return Err(refuse("the function ends within the 14-byte jump"));
      }
      displaced.push(instruction);
   }

   let page = alloc_near(addr).ok_or_else(|| refuse("no free memory within 1.75 GiB"))?;
   let trampoline = page + TRAMPOLINE_AT;
   let mut relocated = BlockEncoder::encode(
      64,
      InstructionBlock::new(&displaced, trampoline as u64),
      BlockEncoderOptions::NONE,
   )
   .map_err(|err| refuse(&format!("relocating the prologue: {err}")))?
   .code_buffer;
   relocated.extend(absolute_jump(addr + covered));
   let stub = stub(observer, ctx, trampoline);
   debug_assert!(stub.len() <= TRAMPOLINE_AT && relocated.len() <= STUB_PAGE - TRAMPOLINE_AT);
   // SAFETY: a fresh page of `STUB_PAGE` bytes, filled before the entry
   // jumps to it.
   unsafe {
      std::ptr::copy_nonoverlapping(stub.as_ptr(), page as *mut u8, stub.len());
      std::ptr::copy_nonoverlapping(relocated.as_ptr(), trampoline as *mut u8, relocated.len());
   }
   // SAFETY: forwarded from the caller.
   unsafe { hot_patch(image, addr, &absolute_jump(page)) }?;
   Ok(Observed { addr, original })
}

/// Writes 14 bytes at a function entry other threads may be calling: first a
/// `jmp $` that parks them, then the tail, then the head.
#[cfg(windows)]
unsafe fn hot_patch(
   image: &Image<'static>,
   addr: usize,
   bytes: &[u8; ABSOLUTE_JUMP_LEN],
) -> Result<(), Error> {
   // SAFETY: `addr % 8 != 7`, so each two-byte write is one atomic word swap,
   // and the tail is only reached through the head.
   unsafe {
      image.write_code(addr, &[0xEB, 0xFE])?;
      image.write_code(addr + 2, &bytes[2..])?;
      image.write_code(addr, &bytes[..2])
   }
}

/// A fresh executable page within reach of RIP-relative operands at `addr`.
#[cfg(windows)]
fn alloc_near(addr: usize) -> Option<usize> {
   const GRANULARITY: usize = 0x1_0000;
   const REACH: usize = 0x7000_0000;
   let home = addr & !(GRANULARITY - 1);
   (1..REACH / GRANULARITY)
      .flat_map(|step| {
         [
            home.checked_sub(step * GRANULARITY),
            home.checked_add(step * GRANULARITY),
         ]
      })
      .flatten()
      .find_map(|candidate| {
         // SAFETY: asks for a fresh mapping at a free address, or fails.
         let page = unsafe {
            win32::VirtualAlloc(
               candidate as *const c_void,
               STUB_PAGE,
               win32::MEM_COMMIT_RESERVE,
               win32::PAGE_EXECUTE_READWRITE,
            )
         };
         (!page.is_null()).then_some(page as usize)
      })
}

/// Saves the argument registers, calls `observer(ctx, &regs)` on a 16-byte
/// aligned stack with its shadow space, restores them and jumps to
/// `trampoline`.
#[cfg(windows)]
fn stub(observer: Observer, ctx: usize, trampoline: usize) -> Vec<u8> {
   let mut code = vec![
      0x51, 0x52, 0x41, 0x50, 0x41, 0x51, // push rcx, rdx, r8, r9
      0x48, 0x83, 0xEC, 0x68, // sub rsp, 0x68
      0xF3, 0x0F, 0x7F, 0x44, 0x24, 0x20, // movdqu [rsp+0x20], xmm0
      0xF3, 0x0F, 0x7F, 0x4C, 0x24, 0x30, // movdqu [rsp+0x30], xmm1
      0xF3, 0x0F, 0x7F, 0x54, 0x24, 0x40, // movdqu [rsp+0x40], xmm2
      0xF3, 0x0F, 0x7F, 0x5C, 0x24, 0x50, // movdqu [rsp+0x50], xmm3
      0x48, 0x8D, 0x54, 0x24, 0x68, // lea rdx, [rsp+0x68]
      0x48, 0xB9, // mov rcx, imm64
   ];
   code.extend(ctx.to_le_bytes());
   code.extend([0x48, 0xB8]); // mov rax, imm64
   code.extend((observer as usize).to_le_bytes());
   code.extend([
      0xFF, 0xD0, // call rax
      0xF3, 0x0F, 0x6F, 0x44, 0x24, 0x20, // movdqu xmm0, [rsp+0x20]
      0xF3, 0x0F, 0x6F, 0x4C, 0x24, 0x30, // movdqu xmm1, [rsp+0x30]
      0xF3, 0x0F, 0x6F, 0x54, 0x24, 0x40, // movdqu xmm2, [rsp+0x40]
      0xF3, 0x0F, 0x6F, 0x5C, 0x24, 0x50, // movdqu xmm3, [rsp+0x50]
      0x48, 0x83, 0xC4, 0x68, // add rsp, 0x68
      0x41, 0x59, 0x41, 0x58, 0x5A, 0x59, // pop r9, r8, rdx, rcx
   ]);
   code.extend(absolute_jump(trampoline));
   code
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn sret_stores_follow_the_return_pointer() {
      let code = [
         0x48, 0x89, 0xC8, // mov rax, rcx
         0x88, 0x90, 0x23, 0x03, 0x00, 0x00, // mov [rax+0x323], dl
         0x48, 0x83, 0xA0, 0x80, 0x00, 0x00, 0x00, 0x00, // and qword ptr [rax+0x80], 0
         0x48, 0x89, 0x51, 0x08, // mov [rcx+8], rdx
         0x48, 0x89, 0xC1, // mov rcx, rax
         0x41, 0xB8, 0x40, 0x00, 0x00, 0x00, // mov r8d, 0x40
         0xE8, 0x00, 0x00, 0x00, 0x00, // call memcpy
         0x48, 0x89, 0x48, 0x10, // mov [rax+0x10], rcx: rax no longer holds it
         0xC3, // ret
         0x48, 0x89, 0x01, // mov [rcx], rax: past the ret
      ];
      assert_eq!(sret_stores(&code, 0x1000), [
         (0x323, 1),
         (0x80, 8),
         (8, 8),
         (0, 0x40)
      ]);
   }

   #[cfg(windows)]
   extern "C" fn ignore(_: usize, _: &Regs) {}

   #[cfg(windows)]
   #[test]
   fn stub_decodes_as_written() {
      let stub = stub(ignore, 7, 0x1000);
      // The jump's 8-byte target is data.
      let code = &stub[..stub.len() - 8];
      let text = disasm(code, 0, 64)
         .into_iter()
         .map(|line| line.text)
         .collect::<Vec<_>>();
      assert_eq!(text[..6], [
         "push rcx",
         "push rdx",
         "push r8",
         "push r9",
         "sub rsp,68h",
         "movdqu [rsp+20h],xmm0"
      ]);
      assert!(text.contains(&"lea rdx,[rsp+68h]".to_owned()));
      assert!(text.contains(&"mov rcx,7".to_owned()));
      assert!(text.contains(&"call rax".to_owned()));
      assert_eq!(text[text.len() - 6..text.len() - 1], [
         "add rsp,68h",
         "pop r9",
         "pop r8",
         "pop rdx",
         "pop rcx"
      ]);
      assert!(
         text
            .last()
            .is_some_and(|last| last.starts_with("jmp qword ptr"))
      );
      assert_eq!(
         code.len(),
         disasm(code, 0, 64)
            .iter()
            .map(|line| line.len)
            .sum::<usize>()
      );
   }
}
