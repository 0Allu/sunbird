//! Patches to `liblibdiscord_wasm_host.so`, located from anchors so a new build
//! only needs a re-run while the code keeps its shape.

use std::ops::Range;

use memchr::memmem;
use object::{
   Endianness,
   elf::{
      self,
      FileHeader32,
      FileHeader64,
   },
   read::elf::{
      FileHeader,
      SectionHeader,
   },
};
use phoenix_sigs::file::{
   ElfFile,
   Machine,
};

use crate::{
   Error,
   bytes::Bytes,
};

pub const LIBRARY: &str = "liblibdiscord_wasm_host.so";
/// The loader's name inside the APK. It replaces `liblog.so` in the host's
/// `DT_NEEDED` list, so it must be the same length to fit in `.dynstr`. The
/// loader itself needs `liblog.so`, which keeps the host's log imports bound.
pub const INJECTED: &str = "libplm.so";
const REPLACED: &str = "liblog.so";
const _: () = assert!(INJECTED.len() == REPLACED.len());

/// The client-access evaluator is the only function that references the
/// experiment table whose first entry is `{"2026-11-phoenix-app-access", 26}`.
const EXPERIMENT: &str = "2026-11-phoenix-app-access";

pub struct Patched {
   pub machine: Machine,
   pub access:  usize,
}

/// `e_machine` of an ELF file, without mapping it.
pub fn machine(path: &str, data: &[u8]) -> Result<Machine, Error> {
   let bytes = Bytes(data);
   if bytes.array::<4>(0).ok() != Some(*b"\x7fELF") {
      return Err(Error::Host(format!("{path}: not an ELF file")));
   }
   let class = data.get(4).copied();
   match (class, bytes.u16(18)?) {
      (Some(2), machine) if machine == elf::EM_AARCH64.0 => Ok(Machine::Aarch64),
      (Some(2), machine) if machine == elf::EM_X86_64.0 => Ok(Machine::X86_64),
      (Some(1), machine) if machine == elf::EM_ARM.0 => Ok(Machine::Arm),
      (_, other) => {
         Err(Error::Host(format!(
            "{path}: unsupported e_machine {other}"
         )))
      },
   }
}

/// Makes the access evaluator return true and, with `needed`, renames the
/// host's `DT_NEEDED` `liblog.so` to it.
pub fn patch(path: &str, data: &mut [u8], needed: Option<&str>) -> Result<Patched, Error> {
   let (machine, name, access, access_at) = {
      let file = ElfFile::parse(path, data).map_err(Error::Elf)?;
      let name = needed.map(|_| needed_name(path, data)).transpose()?;
      let access = access_evaluator(path, data, &file)?;
      let access_at = file
         .file_offset(access)
         .ok_or_else(|| Error::Host(format!("{path}: access evaluator has no file backing")))?;
      (file.machine, name, access, access_at)
   };

   if let (Some(at), Some(needed)) = (name, needed) {
      data[at..at + needed.len()].copy_from_slice(needed.as_bytes());
   }
   // Returns Some(true), an Option<bool> in the low byte of the return
   // register. ARM is Thumb-2, the evaluator is a Thumb function.
   let return_true: &[u8] = match machine {
      Machine::Aarch64 => &[0x20, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6],
      Machine::X86_64 => &[0xB0, 0x01, 0xC3],
      Machine::Arm => &[0x01, 0x20, 0x70, 0x47],
   };
   data[access_at..access_at + return_true.len()].copy_from_slice(return_true);
   Ok(Patched { machine, access })
}

/// Renames a library's `DT_NEEDED` `liblog.so` to `needed`, the same length.
pub fn rename_needed(path: &str, data: &mut [u8], needed: &str) -> Result<(), Error> {
   if needed.len() != REPLACED.len() {
      return Err(Error::Host(format!(
         "{needed} must be as long as {REPLACED}"
      )));
   }
   let at = needed_name(path, data)?;
   data[at..at + needed.len()].copy_from_slice(needed.as_bytes());
   Ok(())
}

/// File offset of the `liblog.so` string a `DT_NEEDED` entry names.
fn needed_name(path: &str, data: &[u8]) -> Result<usize, Error> {
   match data.get(4) {
      Some(2) => needed_name_with::<FileHeader64<Endianness>>(path, data),
      Some(1) => needed_name_with::<FileHeader32<Endianness>>(path, data),
      _ => Err(Error::Host(format!("{path}: not an ELF file"))),
   }
}

fn needed_name_with<Elf>(path: &str, data: &[u8]) -> Result<usize, Error>
where
   Elf: FileHeader<Endian = Endianness>,
   Elf::Word: Into<u64>,
{
   const DT_NEEDED: u64 = 1;

   let parse = |source: object::Error| Error::Host(format!("{path}: {source}"));
   let header = Elf::parse(data).map_err(parse)?;
   let endian = header.endian().map_err(parse)?;
   let sections = header.sections(endian, data).map_err(parse)?;
   let range = |name: &[u8]| -> Result<Range<usize>, Error> {
      let (_, section) = sections.section_by_name(endian, name).ok_or_else(|| {
         Error::Host(format!(
            "{path}: no {} section",
            String::from_utf8_lossy(name)
         ))
      })?;
      let start = usize::try_from(section.sh_offset(endian).into()).map_err(|_| Error::Truncated)?;
      let size = usize::try_from(section.sh_size(endian).into()).map_err(|_| Error::Truncated)?;
      Ok(start..start + size)
   };
   let dynamic = Bytes(data).slice(range(b".dynamic")?)?;
   let strings = range(b".dynstr")?;

   // ELF64 `Dyn` is two 8-byte words, ELF32 two 4-byte words.
   let word = usize::from(header.is_type_64()) * 4 + 4;
   let read = |bytes: &[u8]| -> u64 {
      let mut buf = [0u8; 8];
      buf[..bytes.len()].copy_from_slice(bytes);
      u64::from_le_bytes(buf)
   };
   let hits = dynamic
      .chunks_exact(2 * word)
      .map(|entry| (read(&entry[..word]), read(&entry[word..])))
      .filter(|&(tag, _)| tag == DT_NEEDED)
      .filter_map(|(_, value)| strings.start.checked_add(usize::try_from(value).ok()?))
      .filter(|&at| {
         at < strings.end && data.get(at..at + REPLACED.len() + 1) == Some(b"liblog.so\0")
      })
      .collect::<Vec<usize>>();

   match hits[..] {
      [at] => Ok(at),
      _ => {
         Err(Error::Host(format!(
            "{path}: expected one DT_NEEDED {REPLACED}, found {}",
            hits.len()
         )))
      },
   }
}

fn access_evaluator(path: &str, data: &[u8], file: &ElfFile<'_>) -> Result<usize, Error> {
   let image = &file.image;
   let text = text_range(path, data)?;
   let width = image.pointer_width;
   let slots = image
      .find_bytes(EXPERIMENT.as_bytes())
      .into_iter()
      .flat_map(|string| {
         let target = (string as u64).to_le_bytes();
         image
            .segments
            .iter()
            .filter(|segment| !segment.executable)
            .flat_map(|segment| {
               memmem::find_iter(&segment.bytes[..], &target[..width])
                  .map(|at| segment.addr + at)
                  .collect::<Vec<usize>>()
            })
            .collect::<Vec<usize>>()
      })
      .filter(|slot| slot % width == 0 && image.read_usize(slot + width) == Some(EXPERIMENT.len()))
      .collect::<Vec<usize>>();

   let code = image
      .read(text.start, text.len())
      .ok_or_else(|| Error::Host(format!("{path}: .text outside every segment")))?;

   let mut functions = references(file.machine, text.start, code)
      .filter(|(_, target)| slots.contains(target))
      .filter_map(|(at, _)| image.function_containing(at))
      .collect::<Vec<usize>>();
   functions.sort_unstable();
   functions.dedup();
   match functions[..] {
      [function] => Ok(function),
      _ => {
         Err(Error::Host(format!(
            "{path}: access evaluator: expected one function referencing {EXPERIMENT}, found \
             {functions:x?}"
         )))
      },
   }
}

fn text_range(path: &str, data: &[u8]) -> Result<Range<usize>, Error> {
   match data.get(4) {
      Some(2) => text_range_with::<FileHeader64<Endianness>>(path, data),
      Some(1) => text_range_with::<FileHeader32<Endianness>>(path, data),
      _ => Err(Error::Host(format!("{path}: not an ELF file"))),
   }
}

fn text_range_with<Elf>(path: &str, data: &[u8]) -> Result<Range<usize>, Error>
where
   Elf: FileHeader<Endian = Endianness>,
   Elf::Word: Into<u64>,
{
   let parse = |source: object::Error| Error::Host(format!("{path}: {source}"));
   let header = Elf::parse(data).map_err(parse)?;
   let endian = header.endian().map_err(parse)?;
   let (_, text) = header
      .sections(endian, data)
      .map_err(parse)?
      .section_by_name(endian, b".text")
      .ok_or_else(|| Error::Host(format!("{path}: no .text section")))?;
   let start = usize::try_from(text.sh_addr(endian).into()).map_err(|_| Error::Truncated)?;
   Ok(start..start + usize::try_from(text.sh_size(endian).into()).map_err(|_| Error::Truncated)?)
}

/// `(instruction, target)` for every PC-relative address the code forms. On
/// arm64 that is `adrp` followed within four instructions by an `add` or 64-bit
/// `ldr` on the same register. On x86_64 it is a REX.W `lea` or `mov` from
/// `[rip + disp32]`.
fn references(
   machine: Machine,
   base: usize,
   code: &[u8],
) -> Box<dyn Iterator<Item = (usize, usize)> + '_> {
   match machine {
      Machine::Aarch64 => {
         let words = code
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect::<Vec<u32>>();

         Box::new((0..words.len()).flat_map(move |index| {
            let word = words[index];
            let pc = base + 4 * index;
            let is_adrp = word & 0x9F00_0000 == 0x9000_0000;
            let page = is_adrp
               .then(|| {
                  let imm = i64::from((word >> 29) & 3 | ((word >> 5) & 0x7_FFFF) << 2);
                  let imm = if imm & (1 << 20) != 0 {
                     imm - (1 << 21)
                  } else {
                     imm
                  };
                  isize::try_from(imm << 12).ok()
               })
               .flatten()
               .map(|offset| (pc & !0xFFF).wrapping_add_signed(offset));
            let register = word & 0x1F;

            let partners = words
               .get(index + 1..(index + 5).min(words.len()))
               .unwrap_or_default();
            partners
               .iter()
               .filter_map(move |&next| {
                  let page = page?;
                  let same = (next >> 5) & 0x1F == register;
                  let imm12 = ((next >> 10) & 0xFFF) as usize;
                  match next & 0xFFC0_0000 {
                     0x9100_0000 if same => Some((pc, page + imm12)),
                     0xF940_0000 if same => Some((pc, page + imm12 * 8)),
                     _ => None,
                  }
               })
               .collect::<Vec<_>>()
         }))
      },

      Machine::X86_64 => {
         Box::new((0..code.len().saturating_sub(7)).filter_map(move |at| {
            let &[rex, opcode, modrm, ..] = &code[at..] else {
               return None;
            };
            let rip_relative =
               matches!(rex, 0x48 | 0x4C) && matches!(opcode, 0x8B | 0x8D) && modrm & 0xC7 == 0x05;
            if !rip_relative {
               return None;
            }
            let disp = i32::from_le_bytes(code[at + 3..at + 7].try_into().ok()?);
            Some((
               base + at,
               (base + at + 7).wrapping_add_signed(disp as isize),
            ))
         }))
      },

      // Thumb-2 forms a `.data.rel.ro` address as `ldr Rn, [pc, #x]` loading
      // `target - (add_pc + 4)`, then `add Rn, pc`. The literal pool word holds
      // the offset, the target itself never appears.
      Machine::Arm => {
         let word = |at: usize| -> Option<i32> {
            let off = at.checked_sub(base)?;
            Some(i32::from_le_bytes(code.get(off..off + 4)?.try_into().ok()?))
         };
         let half = |at: usize| -> Option<u16> {
            let off = at.checked_sub(base)?;
            Some(u16::from_le_bytes(code.get(off..off + 2)?.try_into().ok()?))
         };
         let mut out = Vec::new();
         let mut literal = [None; 16];
         let (mut at, end) = (base, base + code.len());
         while let Some(h1) = half(at) {
            if matches!(h1 >> 11, 0b11101 | 0b11110 | 0b11111) {
               if (h1 & 0xFF7F) == 0xF85F
                  && let Some(h2) = half(at + 2)
               {
                  let up = (h1 >> 7) & 1 == 1;
                  let (rt, imm) = ((h2 >> 12) & 0xF, usize::from(h2 & 0xFFF));
                  let page = (at + 4) & !3;
                  let la = if up { page + imm } else { page.wrapping_sub(imm) };
                  literal[rt as usize] = word(la);
               }
               at += 4;
               continue;
            }
            if (h1 & 0xF800) == 0x4800 {
               let rt = (h1 >> 8) & 7;
               let la = ((at + 4) & !3) + usize::from(h1 & 0xFF) * 4;
               literal[rt as usize] = word(la);
            } else if (h1 & 0xFF78) == 0x4478 {
               let rn = (((h1 >> 7) & 1) << 3) | (h1 & 7);
               if let Some(offset) = literal[rn as usize] {
                  out.push((at, (at + 4).wrapping_add_signed(offset as isize)));
               }
            }
            at += 2;
            if at >= end {
               break;
            }
         }
         Box::new(out.into_iter())
      },
   }
}
