//! The zip subset APKs use. Unchanged entries keep their compressed bytes.
//! Stored entries are aligned through zero-filled local extra fields, 16 KiB
//! for native libraries so the linker can map them on 16 KiB page kernels.

use std::{
   borrow::Cow,
   io::Read,
   ops::Range,
};

use flate2::read::DeflateDecoder;

use crate::{
   Error,
   bytes::Bytes,
};

const LOCAL: u32 = 0x0403_4B50;
const CENTRAL: u32 = 0x0201_4B50;
const END: u32 = 0x0605_4B50;
/// 1981-01-01 00:00, so output does not depend on the clock.
const DOS_TIME: u32 = 0x0021_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
   Stored,
   Deflated,
}

impl Method {
   const fn id(self) -> u16 {
      match self {
         Self::Stored => 0,
         Self::Deflated => 8,
      }
   }

   const fn version(self) -> u16 {
      match self {
         Self::Stored => 10,
         Self::Deflated => 20,
      }
   }
}

/// An entry as the central directory lists it, with its compressed bytes as a
/// range of the archive.
pub struct Record {
   pub name:   String,
   pub method: Method,
   pub crc:    u32,
   pub size:   u32,
   pub data:   Range<usize>,
}

impl Record {
   pub fn entry<'a>(&self, archive: &'a [u8]) -> Entry<'a> {
      Entry {
         name:   self.name.clone(),
         method: self.method,
         crc:    self.crc,
         size:   self.size,
         raw:    Cow::Borrowed(&archive[self.data.clone()]),
      }
   }
}

pub struct Entry<'a> {
   pub name:   String,
   pub method: Method,
   pub crc:    u32,
   pub size:   u32,
   /// The bytes as they sit in the archive, compressed when `method` says so.
   pub raw:    Cow<'a, [u8]>,
}

impl<'a> Entry<'a> {
   pub fn stored(name: String, data: impl Into<Cow<'a, [u8]>>) -> Result<Self, Error> {
      let raw = data.into();
      Ok(Self {
         name,
         method: Method::Stored,
         crc: crc32fast::hash(&raw),
         size: u32::try_from(raw.len()).map_err(|_| Error::Zip("entry over 4 GiB".into()))?,
         raw,
      })
   }

   pub fn data(&self) -> Result<Cow<'_, [u8]>, Error> {
      match self.method {
         Method::Stored => Ok(Cow::Borrowed(&self.raw)),
         Method::Deflated => {
            let mut out = Vec::with_capacity(self.size as usize);
            DeflateDecoder::new(&*self.raw)
               .read_to_end(&mut out)
               .map_err(|source| {
                  Error::Inflate {
                     name: self.name.clone(),
                     source,
                  }
               })?;
            Ok(Cow::Owned(out))
         },
      }
   }

   fn alignment(&self) -> usize {
      match self.method {
         Method::Deflated => 1,
         Method::Stored if self.name.as_bytes().ends_with(b".so") => 16384,
         Method::Stored => 4,
      }
   }
}

pub fn read(archive: &[u8]) -> Result<Vec<Record>, Error> {
   let bytes = Bytes(archive);
   let end = (0..=archive.len().saturating_sub(22))
      .rev()
      .find(|&at| bytes.u32(at).is_ok_and(|magic| magic == END))
      .ok_or_else(|| Error::Zip("not a zip, no end of central directory".into()))?;
   let count = bytes.u16(end + 10)?;
   let mut at = bytes.u32(end + 16)? as usize;

   let mut records = Vec::with_capacity(usize::from(count));
   for _ in 0..count {
      if bytes.u32(at)? != CENTRAL {
         return Err(Error::Zip("bad central directory".into()));
      }
      let name_len = usize::from(bytes.u16(at + 28)?);
      let name = String::from_utf8(bytes.slice(at + 46..at + 46 + name_len)?.to_vec())
         .map_err(|_| Error::Zip("entry name is not UTF-8".into()))?;
      let method = match bytes.u16(at + 10)? {
         0 => Method::Stored,
         8 => Method::Deflated,
         other => return Err(Error::Zip(format!("{name}: unsupported method {other}"))),
      };
      let local = bytes.u32(at + 42)? as usize;
      if bytes.u32(local)? != LOCAL {
         return Err(Error::Zip(format!("{name}: bad local header")));
      }
      let start =
         local + 30 + usize::from(bytes.u16(local + 26)?) + usize::from(bytes.u16(local + 28)?);
      let data = start..start + bytes.u32(at + 20)? as usize;
      bytes.slice(data.clone())?;

      records.push(Record {
         name,
         method,
         crc: bytes.u32(at + 16)?,
         size: bytes.u32(at + 24)?,
         data,
      });
      at += 46 + name_len + usize::from(bytes.u16(at + 30)?) + usize::from(bytes.u16(at + 32)?);
   }
   Ok(records)
}

/// A written archive as parts, entry data borrowed from the inputs, so a
/// 280 MB APK costs no second copy.
pub struct Archive<'a> {
   pub parts:     Vec<Cow<'a, [u8]>>,
   /// The part holding the central directory, followed by the end record.
   pub directory: usize,
}

pub fn write(entries: Vec<Entry<'_>>) -> Result<Archive<'_>, Error> {
   let too_big = || Error::Zip("zip64 is not supported".into());
   let mut parts = Vec::with_capacity(2 * entries.len() + 2);
   let mut central = Vec::new();
   let mut offset = 0usize;

   for entry in entries {
      let align = entry.alignment();
      let pad = (align - (offset + 30 + entry.name.len()) % align) % align;
      let compressed = u32::try_from(entry.raw.len()).map_err(|_| too_big())?;
      let name_len = u16::try_from(entry.name.len()).map_err(|_| too_big())?;

      let mut local = Vec::with_capacity(30 + entry.name.len() + pad);
      put32(&mut local, LOCAL);
      put16(&mut local, entry.method.version());
      put16(&mut local, 0);
      put16(&mut local, entry.method.id());
      put32(&mut local, DOS_TIME);
      put32(&mut local, entry.crc);
      put32(&mut local, compressed);
      put32(&mut local, entry.size);
      put16(&mut local, name_len);
      put16(&mut local, u16::try_from(pad).map_err(|_| too_big())?);
      local.extend_from_slice(entry.name.as_bytes());
      local.resize(local.len() + pad, 0);

      put32(&mut central, CENTRAL);
      put16(&mut central, 20);
      put16(&mut central, entry.method.version());
      put16(&mut central, 0);
      put16(&mut central, entry.method.id());
      put32(&mut central, DOS_TIME);
      put32(&mut central, entry.crc);
      put32(&mut central, compressed);
      put32(&mut central, entry.size);
      put16(&mut central, name_len);
      central.extend_from_slice(&[0; 12]);
      put32(&mut central, u32::try_from(offset).map_err(|_| too_big())?);
      central.extend_from_slice(entry.name.as_bytes());

      offset += local.len() + entry.raw.len();
      parts.push(Cow::Owned(local));
      parts.push(entry.raw);
   }

   let count = u16::try_from(parts.len() / 2).map_err(|_| too_big())?;
   let mut end = Vec::with_capacity(22);
   put32(&mut end, END);
   put32(&mut end, 0);
   put16(&mut end, count);
   put16(&mut end, count);
   put32(
      &mut end,
      u32::try_from(central.len()).map_err(|_| too_big())?,
   );
   put32(&mut end, u32::try_from(offset).map_err(|_| too_big())?);
   put16(&mut end, 0);
   if offset
      .checked_add(central.len())
      .is_none_or(|size| size > u32::MAX as usize)
   {
      return Err(too_big());
   }

   let directory = parts.len();
   parts.push(Cow::Owned(central));
   parts.push(Cow::Owned(end));
   Ok(Archive { parts, directory })
}

fn put16(out: &mut Vec<u8>, value: u16) {
   out.extend_from_slice(&value.to_le_bytes());
}

fn put32(out: &mut Vec<u8>, value: u32) {
   out.extend_from_slice(&value.to_le_bytes());
}
