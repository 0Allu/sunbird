//! `sigscan <library.so> [EVENT...]`
//!
//! Loads an ELF file the way the dynamic linker would see it, with relative
//! relocations applied, then evaluates the build table's rules (when the build
//! ID is known) and the portable gateway rules. The output ends with a `Build`
//! entry to paste into `builds.rs`.

use std::{
   borrow::Cow,
   env,
   fs,
};

use eyre::{
   OptionExt,
   WrapErr,
   bail,
   eyre,
};
use object::{
   Endianness,
   elf::{
      self,
      FileHeader64,
   },
   read::elf::{
      FileHeader,
      ProgramHeader,
      SectionHeader,
   },
};
use plumage_sigs::{
   Build,
   BuildId,
   Dispatch,
   Image,
   Segment,
   Sig,
};

fn main() -> eyre::Result<()> {
   color_eyre::install()?;
   let mut args = env::args().skip(1);
   let path = args
      .next()
      .ok_or_eyre("usage: sigscan <library.so> [EVENT...]")?;
   let mut events = args
      .map(|name| name.parse::<Dispatch>())
      .collect::<Result<Vec<_>, _>>()?;
   if events.is_empty() {
      events = Dispatch::ALL.to_vec();
   }

   let data = fs::read(&path).wrap_err_with(|| format!("reading {path}"))?;
   let image = load(&path, &data)?;
   let build_id = image.build_id.ok_or_eyre("no NT_GNU_BUILD_ID note")?;
   let known = Build::find(build_id);
   println!("{} build {build_id}", image.file_name());
   println!(
      "table: {}",
      known.map_or("not in BUILDS", |build| build.label)
   );

   let mut failed = false;
   if let Some(build) = known {
      for rule in build.gateway {
         failed |= report("table", &rule.sig, &image);
      }
   }
   for event in &events {
      failed |= report("portable", &Sig::gateway(*event), &image);
   }

   println!("\n   Build {{");
   println!("      id: BuildId::hex(\"{build_id}\"),");
   println!("      label: \"TODO\",");
   println!("      library: \"{}\",", image.file_name());
   println!("      gateway: &[");
   for event in &events {
      println!("         gateway!({event:?}),");
   }
   println!("      ],");
   println!("   }},");
   if failed {
      bail!("some rules did not resolve");
   }
   Ok(())
}

fn report<A: plumage_sigs::Abi>(kind: &str, sig: &Sig<'_, A>, image: &Image<'_>) -> bool {
   match sig.resolve(image) {
      Ok(located) => {
         let prologue = image
            .segments
            .iter()
            .find(|segment| segment.contains(located.addr))
            .and_then(|segment| segment.bytes.get(located.addr - segment.addr..)?.get(..24))
            .map_or_default(|bytes| {
               bytes
                  .iter()
                  .map(|byte| format!("{byte:02X}"))
                  .collect::<Vec<_>>()
                  .join(" ")
            });
         println!(
            "{kind:>8} {:<20} {:#010x} {}/{} anchors, first `{}`\n{:>8} {:<20} {prologue}",
            sig.name,
            located.addr,
            located.agreeing,
            sig.anchors.len(),
            sig.anchors[located.anchor],
            "",
            ""
         );
         false
      },
      Err(err) => {
         println!("{kind:>8} {err}");
         true
      },
   }
}

/// Maps `PT_LOAD` segments at their link addresses and applies `R_*_RELATIVE`
/// relocations from `.rela.dyn`, plain or Android APS2-packed. RELR and REL
/// store the addend in place, which with a zero load base is already the final
/// value.
fn load<'a>(path: &str, data: &'a [u8]) -> eyre::Result<Image<'a>> {
   let header = FileHeader64::<Endianness>::parse(data).map_err(|err| eyre!("{path}: {err}"))?;
   let endian = header.endian().map_err(|err| eyre!("{err}"))?;
   let relative = match header.e_machine(endian) {
      elf::EM_X86_64 => elf::R_X86_64_RELATIVE.0,
      elf::EM_AARCH64 => elf::R_AARCH64_RELATIVE.0,
      machine => bail!("unsupported e_machine {machine}"),
   };

   let mut image = Image {
      path:         path.to_owned(),
      build_id:     None,
      segments:     Vec::new(),
      eh_frame_hdr: None,
   };
   for segment in header
      .program_headers(endian, data)
      .map_err(|err| eyre!("{err}"))?
   {
      let bytes = segment
         .data(endian, data)
         .map_err(|()| eyre!("segment outside the file"))?;
      let flags = segment.p_flags(endian);
      match segment.p_type(endian) {
         elf::PT_LOAD => {
            let mut owned = bytes.to_vec();
            owned.resize(usize::try_from(segment.p_memsz(endian))?, 0);
            image.segments.push(Segment {
               addr:       usize::try_from(segment.p_vaddr(endian))?,
               bytes:      Cow::Owned(owned),
               executable: flags.0 & elf::PF_X.0 != 0,
               writable:   flags.0 & elf::PF_W.0 != 0,
            });
         },
         elf::PT_NOTE if image.build_id.is_none() => image.build_id = BuildId::from_notes(bytes),
         elf::PT_GNU_EH_FRAME => {
            image.eh_frame_hdr = Some(usize::try_from(segment.p_vaddr(endian))?);
         },
         _ => {},
      }
   }

   let sections = header
      .sections(endian, data)
      .map_err(|err| eyre!("{err}"))?;
   for section in sections.iter() {
      let contents = || section.data(endian, data).map_err(|err| eyre!("{err}"));
      let relocs = match section.sh_type(endian) {
         elf::SHT_RELA => plain_rela(contents()?, relative),
         elf::SHT_ANDROID_RELA => aps2(contents()?, relative)?,
         _ => continue,
      };
      for (offset, addend) in relocs {
         let segment = image
            .segments
            .iter_mut()
            .find(|segment| segment.contains(offset))
            .ok_or_else(|| eyre!("relocation at {offset:#x} outside every segment"))?;
         let at = offset - segment.addr;
         segment.bytes.to_mut()[at..at + 8].copy_from_slice(&addend.to_le_bytes());
      }
   }
   Ok(image)
}

fn plain_rela(bytes: &[u8], relative: u32) -> Vec<(usize, u64)> {
   bytes
      .as_chunks::<24>()
      .0
      .iter()
      .filter_map(|entry| {
         let word = |at: usize| u64::from_le_bytes(entry[at..at + 8].try_into().unwrap());
         let offset = usize::try_from(word(0)).ok()?;
         (word(8) & 0xFFFF_FFFF == u64::from(relative)).then_some((offset, word(16)))
      })
      .collect()
}

/// Android's packed relocation format: `APS2` then SLEB128 groups of offset
/// deltas, info and addend deltas, each optionally shared across the group.
fn aps2(bytes: &[u8], relative: u32) -> eyre::Result<Vec<(usize, u64)>> {
   const BY_INFO: i64 = 1;
   const BY_OFFSET_DELTA: i64 = 2;
   const BY_ADDEND: i64 = 4;
   const HAS_ADDEND: i64 = 8;

   let mut stream = bytes
      .strip_prefix(b"APS2")
      .ok_or_eyre("packed relocations without APS2")?;
   let mut next = || -> eyre::Result<i64> {
      let (mut value, mut shift) = (0i64, 0);
      loop {
         let (&byte, rest) = stream.split_first().ok_or_eyre("truncated APS2 stream")?;
         stream = rest;
         value |= i64::from(byte & 0x7F) << shift;
         shift += 7;
         if byte & 0x80 == 0 {
            if byte & 0x40 != 0 && shift < 64 {
               value |= -1 << shift;
            }
            return Ok(value);
         }
      }
   };

   let count = next()?;
   let (mut offset, mut addend, mut done) = (next()?, 0i64, 0);
   let mut out = Vec::new();
   while done < count {
      let group_size = next()?;
      let flags = next()?;
      let offset_delta = if flags & BY_OFFSET_DELTA != 0 {
         next()?
      } else {
         0
      };
      let mut info = if flags & BY_INFO != 0 { next()? } else { 0 };
      if flags & HAS_ADDEND != 0 && flags & BY_ADDEND != 0 {
         addend += next()?;
      }
      for _ in 0..group_size {
         offset += if flags & BY_OFFSET_DELTA != 0 {
            offset_delta
         } else {
            next()?
         };
         if flags & BY_INFO == 0 {
            info = next()?;
         }
         if flags & HAS_ADDEND != 0 && flags & BY_ADDEND == 0 {
            addend += next()?;
         }
         if info & 0xFFFF_FFFF == i64::from(relative) {
            let value = if flags & HAS_ADDEND != 0 { addend } else { 0 };
            out.push((usize::try_from(offset)?, value.cast_unsigned()));
         }
         done += 1;
      }
   }
   Ok(out)
}
