//! Forces Phoenix to launch. The launcher method that falls back to the React
//! Native client is found by its log string, since names are obfuscated per
//! build, and made to return true.

use crate::{
   Error,
   bytes::Bytes,
};

const ANCHOR: &[u8] = b"Phoenix didn't stay up for 3 launches";
/// `const/4 v0, #1` then `return v0`.
const RETURN_TRUE: [u8; 4] = [0x12, 0x10, 0x0F, 0x00];

struct Dex<'a>(Bytes<'a>);

impl Dex<'_> {
   fn u16(&self, at: usize) -> Result<u16, Error> {
      self.0.u16(at)
   }

   fn u32(&self, at: usize) -> Result<usize, Error> {
      Ok(self.0.u32(at)? as usize)
   }

   fn uleb(&self, at: &mut usize) -> Result<usize, Error> {
      let (mut value, mut shift) = (0usize, 0);
      loop {
         let byte = self.0.u8(*at)?;
         *at += 1;
         if shift >= 35 {
            return Err(Error::Dex("uleb128 overflow".into()));
         }
         value |= usize::from(byte & 0x7F) << shift;
         shift += 7;
         if byte & 0x80 == 0 {
            return Ok(value);
         }
      }
   }

   /// MUTF-8 bytes of string `index`, without the terminator.
   fn string(&self, ids: usize, index: usize) -> Result<&[u8], Error> {
      let mut at = self.u32(ids + 4 * index)?;
      self.uleb(&mut at)?;
      let rest = self.0.0.get(at..).ok_or(Error::Truncated)?;
      let end = rest
         .iter()
         .position(|&byte| byte == 0)
         .ok_or_else(|| Error::Dex("unterminated string".into()))?;
      Ok(&rest[..end])
   }
}

/// Patches the launcher method in place and returns its code offset, or `None`
/// when this dex does not hold the anchor string.
pub fn patch(data: &mut [u8]) -> Result<Option<usize>, Error> {
   if data.len() < 0x70 {
      return Err(Error::Truncated);
   }
   let dex = Dex(Bytes(data));
   let header = |field: usize| dex.u32(0x38 + 4 * field);
   let (string_count, strings, proto_ids) = (header(0)?, header(1)?, header(5)?);
   let (method_ids, class_count, class_defs) = (header(9)?, header(10)?, header(11)?);

   let mut anchors = Vec::new();
   for index in 0..string_count {
      if dex.string(strings, index)?.starts_with(ANCHOR) {
         anchors.push(index);
      }
   }
   if anchors.is_empty() {
      return Ok(None);
   }

   let mut hits = Vec::new();
   for class in 0..class_count {
      let mut at = dex.u32(class_defs + 32 * class + 24)?;
      if at == 0 {
         continue;
      }
      let static_fields = dex.uleb(&mut at)?;
      let instance_fields = dex.uleb(&mut at)?;
      let direct = dex.uleb(&mut at)?;
      let virtual_methods = dex.uleb(&mut at)?;
      for _ in 0..2 * (static_fields + instance_fields) {
         dex.uleb(&mut at)?;
      }
      let mut method = 0;
      for index in 0..direct + virtual_methods {
         // method_idx_diff restarts at the virtual method list.
         if index == direct {
            method = 0;
         }
         method += dex.uleb(&mut at)?;
         dex.uleb(&mut at)?;
         let code = dex.uleb(&mut at)?;
         if code == 0 {
            continue;
         }
         let registers = dex.u16(code)?;
         let insns = code + 16;
         let units = dex.u32(code + 12)?;
         let word = |index: usize| dex.u16(insns + 2 * index).map(usize::from);
         for unit in 0..units {
            let op = word(unit)? & 0xFF;
            let string = match op {
               0x1A if unit + 1 < units => word(unit + 1)?,
               0x1B if unit + 2 < units => word(unit + 1)? | word(unit + 2)? << 16,
               _ => continue,
            };
            if anchors.contains(&string) {
               hits.push((method, registers, insns));
            }
         }
      }
   }
   hits.sort_unstable();
   hits.dedup();
   let [(method, registers, insns)] = hits[..] else {
      return Err(Error::Dex(format!(
         "launcher anchor: expected one method, found {}",
         hits.len()
      )));
   };

   let proto = usize::from(dex.u16(method_ids + 8 * method + 2)?);
   let shorty = dex.string(strings, dex.u32(proto_ids + 12 * proto)?)?;
   if !shorty.starts_with(b"Z") || registers < 1 {
      return Err(Error::Dex(format!(
         "launcher method has shorty {}, {registers} registers",
         String::from_utf8_lossy(shorty)
      )));
   }
   if data.len() < insns + RETURN_TRUE.len() {
      return Err(Error::Truncated);
   }

   data[insns..insns + RETURN_TRUE.len()].copy_from_slice(&RETURN_TRUE);
   let signature = sha1_smol::Sha1::from(&data[32..]).digest().bytes();
   data[12..32].copy_from_slice(&signature);
   let checksum = adler2::adler32_slice(&data[12..]);
   data[8..12].copy_from_slice(&checksum.to_le_bytes());
   Ok(Some(insns))
}
