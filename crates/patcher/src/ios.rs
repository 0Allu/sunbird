//! Patches a decrypted iOS IPA so Phoenix opens without the client-access
//! gate, the install-time switch the site applies before the user sideloads.

use phoenix_sigs::{
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
/// The Hermes bundle holding `isStaffEnv`.
const JS_BUNDLE: &str = "Payload/Discord.app/main.jsbundle";
/// `LoadConstTrue r1; Ret r1`, so `isStaffEnv` returns true and Discord offers
/// the Phoenix launch switch. This enables every staff experiment, so it is a
/// blunt lever kept for parity with the site.
const STAFF_TRUE: [u8; 4] = [0x95, 0x01, 0x76, 0x01];

/// Opens the access gate and forces `isStaffEnv` true in a decrypted IPA.
///
/// Replaces the site's `web/lib/ios.js`. The two patched files are restored
/// uncompressed and every other entry is copied verbatim.
pub fn build_patched_ipa(ipa: &[u8]) -> Result<Vec<u8>, Error> {
   let records = zip::read(ipa)?;
   let mut entries = records
      .iter()
      .map(|record| record.entry(ipa))
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
   patch_staff(&name, &mut hermes)?;
   entries[bundle] = zip::Entry::stored(name, hermes)?;

   let mut out = Vec::new();
   for part in zip::write(entries)?.parts {
      out.extend_from_slice(&part);
   }
   Ok(out)
}

/// Forces `isStaffEnv` to return true in the Hermes bundle.
fn patch_staff(path: &str, data: &mut [u8]) -> Result<(), Error> {
   let at = ios::staff_env(data).map_err(|err| Error::Host(format!("{path}: {err}")))?;
   data[at..at + STAFF_TRUE.len()].copy_from_slice(&STAFF_TRUE);
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
