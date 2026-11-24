//! Patches the iOS `PhoenixFatClient` Mach-O so Phoenix opens without the
//! client-access gate, the install-time switch the site applies to a decrypted
//! IPA.

use phoenix_sigs::{
   file::MachOFile,
   ios,
};

use crate::Error;

/// `mov w0, #1; ret`, which makes an evaluator report access.
const RETURN_TRUE: [u8; 8] = [0x20, 0x00, 0x80, 0x52, 0xC0, 0x03, 0x5F, 0xD6];

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
      data[at..at + RETURN_TRUE.len()].copy_from_slice(&RETURN_TRUE);
   }
   Ok(gates)
}
