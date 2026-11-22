//! Patches to `liblibdiscord_wasm_host.so`, located from anchors so a new build
//! only needs a re-run while the code keeps its shape.

use std::ops::Range;

use misstep::{
   OptionExt,
   ensure,
   report,
};
use object::{
   Endianness,
   elf::FileHeader64,
   read::elf::{
      FileHeader,
      SectionHeader,
   },
};
use plumage_sigs::file::{
   ElfFile,
   Machine,
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

pub fn patch(path: &str, data: &mut [u8]) -> misstep::Result<Patched> {
   let (machine, needed, access, access_at) = {
      let file = ElfFile::parse(path, data)?;
      let needed = needed_name(data)?;
      let access = access_evaluator(data, &file)?;
      let access_at = file
         .file_offset(access)
         .context("access evaluator has no file backing")?;
      (file.machine, needed, access, access_at)
   };

   data[needed..needed + INJECTED.len()].copy_from_slice(INJECTED.as_bytes());
   // Returns Some(true), an Option<bool> in the low byte of the return
   // register.
   let return_true: &[u8] = match machine {
      Machine::Aarch64 => &[0x20, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6],
      Machine::X86_64 => &[0xB0, 0x01, 0xC3],
   };
   data[access_at..access_at + return_true.len()].copy_from_slice(return_true);
   Ok(Patched { machine, access })
}

/// File offset of the `liblog.so` string a `DT_NEEDED` entry names.
fn needed_name(data: &[u8]) -> misstep::Result<usize> {
   const DT_NEEDED: u64 = 1;

   let header = FileHeader64::<Endianness>::parse(data)?;
   let endian = header.endian()?;
   let sections = header.sections(endian, data)?;
   let range = |name: &[u8]| -> misstep::Result<Range<usize>> {
      let (_, section) = sections
         .section_by_name(endian, name)
         .ok_or_else(|| report!("no {} section", String::from_utf8_lossy(name)))?;
      let start = usize::try_from(section.sh_offset(endian))?;
      Ok(start..start + usize::try_from(section.sh_size(endian))?)
   };
   let dynamic = data
      .get(range(b".dynamic")?)
      .context(".dynamic outside the file")?;
   let strings = range(b".dynstr")?;

   let hits = dynamic
      .as_chunks::<16>()
      .0
      .iter()
      .map(|entry| {
         let (tag, value) = entry.split_at(8);
         (
            u64::from_le_bytes(tag.try_into().unwrap()),
            u64::from_le_bytes(value.try_into().unwrap()),
         )
      })
      .filter(|&(tag, _)| tag == DT_NEEDED)
      .filter_map(|(_, value)| strings.start.checked_add(usize::try_from(value).ok()?))
      .filter(|&at| {
         at < strings.end && data.get(at..at + REPLACED.len() + 1) == Some(b"liblog.so\0")
      })
      .collect::<Vec<usize>>();

   match hits[..] {
      [at] => Ok(at),
      _ => {
         Err(report!(
            "expected one DT_NEEDED {REPLACED}, found {}",
            hits.len()
         ))
      },
   }
}

fn access_evaluator(data: &[u8], file: &ElfFile<'_>) -> misstep::Result<usize> {
   let image = &file.image;
   let text = text_range(data)?;
   let slots = image
      .find_bytes(EXPERIMENT.as_bytes())
      .into_iter()
      .flat_map(|string| image.pointers_to(string))
      .filter(|slot| image.read_usize(slot + size_of::<usize>()) == Some(EXPERIMENT.len()))
      .collect::<Vec<usize>>();

   let code = image
      .read(text.start, text.len())
      .context(".text outside every segment")?;

   let mut functions = references(file.machine, text.start, code)
      .filter(|(_, target)| slots.contains(target))
      .filter_map(|(at, _)| image.function_containing(at))
      .collect::<Vec<usize>>();
   functions.sort_unstable();
   functions.dedup();
   ensure!(
      functions.len() == 1,
      "access evaluator: expected one function referencing {EXPERIMENT}, found {functions:x?}"
   );
   Ok(functions[0])
}

fn text_range(data: &[u8]) -> misstep::Result<Range<usize>> {
   let header = FileHeader64::<Endianness>::parse(data)?;
   let endian = header.endian()?;
   let (_, text) = header
      .sections(endian, data)?
      .section_by_name(endian, b".text")
      .context("no .text section")?;
   let start = usize::try_from(text.sh_addr(endian))?;
   Ok(start..start + usize::try_from(text.sh_size(endian))?)
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
   }
}
