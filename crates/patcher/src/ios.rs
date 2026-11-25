//! Patches a decrypted iOS IPA so Phoenix opens without the client-access
//! gate and loads the plugin runtime, the build the site hands the user to
//! sideload.

use std::collections::BTreeMap;

use phoenix_sigs::{
   PLUGIN_BUNDLE,
   file::MachOFile,
   ios,
};

use crate::{
   Error,
   zip,
};

/// `mov w0, #1; ret`, which makes an evaluator report access.
const RETURN_TRUE: [u8; 8] = [0x20, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6];

/// The Mach-O inside the framework, by the suffix of its archive path.
const FRAMEWORK: &str = "PhoenixFatClient.framework/PhoenixFatClient";
/// The Hermes bundle holding the Phoenix switch experiment.
const JS_BUNDLE: &str = "Payload/Discord.app/main.jsbundle";
/// The app's executable, which gets the load command for [`LOADER`].
const EXECUTABLE: &str = "Payload/Discord.app/Discord";
/// Where the loader and, beside it, [`PLUGIN_BUNDLE`] go.
const FRAMEWORKS: &str = "Payload/Discord.app/Frameworks/";
const LOADER: &str = "libphoenix.dylib";
/// The path [`EXECUTABLE`] loads [`LOADER`] from.
const LOADER_PATH: &str = "@executable_path/Frameworks/libphoenix.dylib";

/// Opens the access gate, offers the Phoenix switch and adds `loader` with
/// `plugins` to a decrypted IPA. The patched and added files are stored
/// uncompressed and every other entry is copied verbatim.
pub fn build_patched_ipa(
   ipa: &[u8],
   loader: &[u8],
   plugins: &BTreeMap<String, String>,
) -> Result<Vec<u8>, Error> {
   let records = zip::read(ipa)?;
   let added = [LOADER, PLUGIN_BUNDLE].map(|name| format!("{FRAMEWORKS}{name}"));
   let mut entries = records
      .iter()
      .map(|record| record.entry(ipa))
      .filter(|entry| !added.contains(&entry.name))
      .collect::<Vec<_>>();

   let framework = entries
      .iter()
      .position(|entry| entry.name.contains("/Frameworks/") && entry.name.ends_with(FRAMEWORK))
      .ok_or_else(|| Error::Host(format!("{FRAMEWORK} not found in the IPA")))?;
   let name = entries[framework].name.clone();
   let mut macho = entries[framework].data()?.into_owned();
   open_access_gate(&name, &mut macho)?;
   entries[framework] = zip::Entry::stored(name, macho)?;

   let bundle = entries
      .iter()
      .position(|entry| entry.name == JS_BUNDLE)
      .ok_or_else(|| Error::Host(format!("{JS_BUNDLE} not found in the IPA")))?;
   let name = entries[bundle].name.clone();
   let mut hermes = entries[bundle].data()?.into_owned();
   offer_switch(&name, &mut hermes)?;
   entries[bundle] = zip::Entry::stored(name, hermes)?;

   let executable = entries
      .iter()
      .position(|entry| entry.name == EXECUTABLE)
      .ok_or_else(|| Error::Host(format!("{EXECUTABLE} not found in the IPA")))?;
   let mut binary = entries[executable].data()?.into_owned();
   load_weak(EXECUTABLE, &mut binary, LOADER_PATH)?;
   entries[executable] = zip::Entry::stored(EXECUTABLE.to_owned(), binary)?;

   let bundle =
      serde_json::to_vec(plugins).map_err(|err| Error::Host(format!("plugin bundle: {err}")))?;
   let [loader_name, bundle_name] = added;
   entries.push(zip::Entry::stored(loader_name, loader)?);
   entries.push(zip::Entry::stored(bundle_name, bundle)?);

   let mut out = Vec::new();
   for part in zip::write(entries)?.parts {
      out.extend_from_slice(&part);
   }
   Ok(out)
}

const MH_MAGIC_64: u32 = 0xFEED_FACF;
const LC_SEGMENT_64: u32 = 0x19;
const LC_LOAD_DYLIB: u32 = 0xC;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const HEADER: usize = 32;
/// `cmd`, `cmdsize`, the name's offset, timestamp and both versions.
const DYLIB_COMMAND: u32 = 24;

/// Adds an `LC_LOAD_WEAK_DYLIB` for `dylib` to a thin arm64 Mach-O, in the
/// padding between its load commands and its first section. Weak, so a
/// missing loader leaves Discord running without plugins. Does nothing when
/// the binary already loads `dylib`.
fn load_weak(path: &str, data: &mut [u8], dylib: &str) -> Result<(), Error> {
   let fail = |what: &str| Error::Host(format!("{path}: {what}"));
   let word = |data: &[u8], at: usize| {
      data
         .get(at..at + 4)
         .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
         .ok_or_else(|| fail("load commands run past the file"))
   };
   if word(data, 0)? != MH_MAGIC_64 {
      return Err(fail("not a thin 64-bit Mach-O"));
   }
   let count = word(data, 16)?;
   let size = word(data, 20)?;

   let mut at = HEADER;
   let mut first_section = usize::MAX;
   for _ in 0..count {
      let (cmd, cmdsize) = (word(data, at)?, word(data, at + 4)? as usize);
      match cmd {
         LC_SEGMENT_64 => {
            for section in 0..word(data, at + 64)? as usize {
               let offset = word(data, at + 72 + section * 80 + 48)? as usize;
               if offset != 0 {
                  first_section = first_section.min(offset);
               }
            }
         },
         LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB => {
            let name = data
               .get(at + word(data, at + 8)? as usize..at + cmdsize)
               .ok_or_else(|| fail("a dylib name runs past its command"))?;
            if name.split(|&byte| byte == 0).next() == Some(dylib.as_bytes()) {
               return Ok(());
            }
         },
         _ => {},
      }
      at += cmdsize;
   }

   let name = DYLIB_COMMAND as usize;
   let cmdsize = (name + dylib.len() + 1).next_multiple_of(8);
   let cmdsize_word = u32::try_from(cmdsize).map_err(|_| fail("the dylib path is too long"))?;
   let end = HEADER + size as usize;
   let free = data
      .get_mut(end..end + cmdsize)
      .filter(|_| end + cmdsize <= first_section)
      .filter(|free| free.iter().all(|&byte| byte == 0))
      .ok_or_else(|| fail("no room for another load command"))?;
   let fields = [
      LC_LOAD_WEAK_DYLIB,
      cmdsize_word,
      DYLIB_COMMAND,
      2,
      0x1_0000,
      0x1_0000,
   ];
   for (slot, field) in free.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
      *slot = field.to_le_bytes();
   }
   free[name..name + dylib.len()].copy_from_slice(dylib.as_bytes());

   data[16..20].copy_from_slice(&(count + 1).to_le_bytes());
   data[20..24].copy_from_slice(&(size + cmdsize_word).to_le_bytes());
   Ok(())
}

/// Makes the Phoenix switch experiment's default config its enabled variation,
/// so Settings offers the switch without the staff flag.
fn offer_switch(path: &str, data: &mut [u8]) -> Result<(), Error> {
   let switch =
      ios::switch_entrypoint(data).map_err(|err| Error::Host(format!("{path}: {err}")))?;
   let enabled = switch.enabled_config;
   data.copy_within(
      enabled..enabled + ios::OBJECT_BUFFERS,
      switch.default_config,
   );
   Ok(())
}

/// Makes both `ClientAccessGate` evaluators return true in a thin arm64
/// `PhoenixFatClient`, returning their link addresses. Both are patched because
/// they share a lock and patching one crashes the client.
pub fn open_access_gate(path: &str, data: &mut [u8]) -> Result<[usize; 2], Error> {
   let (gates, offsets) = {
      let file = MachOFile::parse(path, data).map_err(Error::Elf)?;
      let gates = ios::access_gate(&file.image).map_err(Error::Host)?;
      let offset = |vaddr: usize| {
         file.file_offset(vaddr).ok_or_else(|| {
            Error::Host(format!(
               "{path}: access gate {vaddr:#x} has no file backing"
            ))
         })
      };
      (gates, [offset(gates[0])?, offset(gates[1])?])
   };

   for at in offsets {
      data
         .get_mut(at..at + RETURN_TRUE.len())
         .ok_or_else(|| Error::Host(format!("{path}: access gate at {at:#x} runs past the file")))?
         .copy_from_slice(&RETURN_TRUE);
   }
   Ok(gates)
}
