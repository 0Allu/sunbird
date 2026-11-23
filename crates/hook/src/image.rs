use std::{
   borrow::Cow,
   ffi::{
      CStr,
      c_int,
      c_void,
   },
   fmt,
   path::Path,
   slice,
   sync::atomic::{
      AtomicUsize,
      Ordering,
   },
};

use memchr::memmem;

use crate::pattern::Pattern;

const NT_GNU_BUILD_ID: u32 = 3;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// The `NT_GNU_BUILD_ID` note of an ELF module, 1 to 32 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuildId {
   len:   u8,
   bytes: [u8; 32],
}

impl BuildId {
   /// Parses lowercase or uppercase hex, as printed by `readelf -n`. Invalid
   /// input fails constant evaluation.
   #[must_use]
   #[expect(
      clippy::cast_possible_truncation,
      reason = "the length is asserted to be at most 32"
   )]
   pub const fn hex(source: &str) -> Self {
      let digits = source.as_bytes();
      assert!(
         !digits.is_empty() && digits.len() % 2 == 0 && digits.len() <= 64,
         "build ID is 1 to 32 hex bytes"
      );
      let mut bytes = [0; 32];
      let mut index = 0;
      while index < digits.len() / 2 {
         let (Some(high), Some(low)) = (nibble(digits[2 * index]), nibble(digits[2 * index + 1]))
         else {
            panic!("build ID is hex");
         };
         bytes[index] = high << 4 | low;
         index += 1;
      }
      Self {
         len: (digits.len() / 2) as u8,
         bytes,
      }
   }

   #[must_use]
   pub fn from_bytes(id: &[u8]) -> Option<Self> {
      if id.is_empty() || id.len() > 32 {
         return None;
      }
      let mut bytes = [0; 32];
      bytes[..id.len()].copy_from_slice(id);
      Some(Self {
         len: u8::try_from(id.len()).ok()?,
         bytes,
      })
   }

   #[must_use]
   pub fn as_bytes(&self) -> &[u8] {
      &self.bytes[..usize::from(self.len)]
   }

   /// Reads the GNU build ID out of the contents of a `PT_NOTE` segment.
   #[must_use]
   pub fn from_notes(mut notes: &[u8]) -> Option<Self> {
      let word = |bytes: &[u8], at: usize| -> Option<u32> {
         Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
      };
      let align4 = |len: u32| (len as usize).next_multiple_of(4);
      while notes.len() >= 12 {
         let (name_len, desc_len, kind) = (word(notes, 0)?, word(notes, 4)?, word(notes, 8)?);
         let name = notes.get(12..12 + name_len as usize)?;
         let desc_at = 12 + align4(name_len);
         let desc = notes.get(desc_at..desc_at + desc_len as usize)?;
         if kind == NT_GNU_BUILD_ID && name == b"GNU\0" {
            return Self::from_bytes(desc);
         }
         notes = notes.get(desc_at + align4(desc_len)..)?;
      }
      None
   }
}

const fn nibble(byte: u8) -> Option<u8> {
   match byte {
      b'0'..=b'9' => Some(byte - b'0'),
      b'a'..=b'f' => Some(byte - b'a' + 10),
      b'A'..=b'F' => Some(byte - b'A' + 10),
      _ => None,
   }
}

impl fmt::Display for BuildId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      self
         .as_bytes()
         .iter()
         .try_for_each(|byte| write!(formatter, "{byte:02x}"))
   }
}

impl fmt::Debug for BuildId {
   fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
      write!(formatter, "BuildId({self})")
   }
}

/// One readable `PT_LOAD` segment at its runtime address.
pub struct Segment<'a> {
   pub addr:       usize,
   pub bytes:      Cow<'a, [u8]>,
   pub executable: bool,
   pub writable:   bool,
}

impl Segment<'_> {
   #[must_use]
   pub fn contains(&self, addr: usize) -> bool {
      (self.addr..self.addr + self.bytes.len()).contains(&addr)
   }
}

/// An ELF module, either mapped in this process or rebuilt from a file with its
/// relative relocations applied, so pointer slots hold the addresses code would
/// see.
pub struct Image<'a> {
   pub path:         String,
   pub build_id:     Option<BuildId>,
   pub segments:     Vec<Segment<'a>>,
   /// Address of `.eh_frame_hdr`, from `PT_GNU_EH_FRAME`.
   pub eh_frame_hdr: Option<usize>,
}

#[cfg(unix)]
impl Image<'static> {
   /// Every module `dl_iterate_phdr` reports.
   ///
   /// # Safety
   ///
   /// The segments borrow mapped memory. The modules stay loaded while the
   /// images live, and no thread writes the writable segments during a scan.
   pub unsafe fn loaded() -> Vec<Self> {
      #[allow(
         clippy::cast_possible_truncation,
         reason = "ElfW(Addr) is pointer-sized, so these casts are lossless on every target"
      )]
      unsafe extern "C" fn visit(
         info: *mut libc::dl_phdr_info,
         _size: usize,
         out: *mut c_void,
      ) -> c_int {
         // SAFETY: the loader passes a valid info record and `out` is the Vec
         // below.
         let (info, images) = unsafe { (&*info, &mut *out.cast::<Vec<Image<'static>>>()) };
         let bias = info.dlpi_addr as usize;
         let headers = if info.dlpi_phdr.is_null() {
            &[][..]
         } else {
            // SAFETY: the loader reports dlpi_phnum headers at dlpi_phdr.
            unsafe { slice::from_raw_parts(info.dlpi_phdr, usize::from(info.dlpi_phnum)) }
         };
         let path = if info.dlpi_name.is_null() {
            String::new()
         } else {
            // SAFETY: dlpi_name is NUL-terminated when present.
            unsafe { CStr::from_ptr(info.dlpi_name) }
               .to_string_lossy()
               .into_owned()
         };

         let mut image = Image {
            path,
            build_id: None,
            segments: Vec::new(),
            eh_frame_hdr: None,
         };
         for header in headers {
            let addr = bias + header.p_vaddr as usize;
            let len = header.p_memsz as usize;
            // SAFETY: PT_LOAD and PT_NOTE ranges are mapped for the module's
            // lifetime.
            let bytes = || unsafe { slice::from_raw_parts(addr as *const u8, len) };
            match header.p_type {
               libc::PT_LOAD if header.p_flags & PF_R != 0 => {
                  image.segments.push(Segment {
                     addr,
                     bytes: Cow::Borrowed(bytes()),
                     executable: header.p_flags & PF_X != 0,
                     writable: header.p_flags & PF_W != 0,
                  });
               },
               libc::PT_NOTE if image.build_id.is_none() => {
                  image.build_id = BuildId::from_notes(bytes());
               },
               libc::PT_GNU_EH_FRAME => image.eh_frame_hdr = Some(addr),
               _ => {},
            }
         }
         images.push(image);
         0
      }

      let mut images = Vec::new();
      // SAFETY: `visit` only touches `images` through the pointer it is handed.
      unsafe { libc::dl_iterate_phdr(Some(visit), (&raw mut images).cast()) };
      images
   }
}

#[cfg(unix)]
impl Image<'static> {
   /// Writes each `(slot, value)` pair into this module's mapped memory and
   /// returns the values the slots held. Pages under `PT_GNU_RELRO` are made
   /// writable for the write and get their protection back afterwards.
   ///
   /// # Safety
   ///
   /// Every slot is a pointer-aligned word in this module, and no thread reads
   /// a slot while it is written.
   pub unsafe fn swap_words(&self, writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
      // SAFETY: forwarded from the caller.
      unsafe { swap_words(writes) }
   }
}

#[cfg(unix)]
/// Atomically swaps each `(slot, value)` pair in mapped memory and returns the
/// values the slots held. Read-only pages are made writable for the write and
/// get their protection back afterwards.
///
/// # Safety
///
/// Every slot is a mapped, pointer-aligned word, and code that reads it accepts
/// either value.
pub unsafe fn swap_words(writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
   // SAFETY: sysconf has no preconditions.
   let page_size = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
   let mut pages = writes
      .iter()
      .map(|&(slot, _)| slot & !(page_size - 1))
      .collect::<Vec<usize>>();
   pages.sort_unstable();
   pages.dedup();

   let maps = std::fs::read_to_string("/proc/self/maps")
      .map_err(|source| crate::Error::Protect { addr: 0, source })?;
   let restore = pages
      .iter()
      .map(|&page| Some((page, protection(&maps, page)?)))
      .collect::<Option<Vec<(usize, c_int)>>>()
      .ok_or_else(|| {
         crate::Error::Unmapped {
            addr: pages.first().copied().unwrap_or(0),
         }
      })?;
   let protect = |page: usize, prot: c_int| {
      // SAFETY: `page` is a mapped page, from /proc/self/maps.
      if unsafe { libc::mprotect(page as *mut c_void, page_size, prot) } == 0 {
         Ok(())
      } else {
         Err(crate::Error::Protect {
            addr:   page,
            source: std::io::Error::last_os_error(),
         })
      }
   };

   for &(page, prot) in &restore {
      protect(page, prot | libc::PROT_WRITE)?;
   }
   let previous = writes
      .iter()
      // SAFETY: the caller guarantees aligned, mapped slots, now writable.
      .map(|&(slot, value)| {
         unsafe { AtomicUsize::from_ptr(slot as *mut usize) }.swap(value, Ordering::AcqRel)
      })
      .collect();
   for &(page, prot) in &restore {
      protect(page, prot)?;
   }
   Ok(previous)
}

#[cfg(unix)]
/// Protection of the mapping holding `addr`, from `/proc/self/maps`.
fn protection(maps: &str, addr: usize) -> Option<c_int> {
   maps.lines().find_map(|line| {
      let mut fields = line.split_ascii_whitespace();
      let (start, end) = fields.next()?.split_once('-')?;
      let range = usize::from_str_radix(start, 16).ok()?..usize::from_str_radix(end, 16).ok()?;
      let perms = fields.next()?.as_bytes();
      range.contains(&addr).then(|| {
         [
            (b'r', libc::PROT_READ),
            (b'w', libc::PROT_WRITE),
            (b'x', libc::PROT_EXEC),
         ]
         .iter()
         .zip(perms)
         .filter(|((flag, _), perm)| flag == *perm)
         .fold(libc::PROT_NONE, |prot, ((_, bit), _)| prot | bit)
      })
   })
}

impl Image<'_> {
   #[must_use]
   pub fn file_name(&self) -> &str {
      Path::new(&self.path)
         .file_name()
         .and_then(|name| name.to_str())
         .unwrap_or(&self.path)
   }

   /// Addresses in executable segments where `pattern` matches.
   #[must_use]
   pub fn scan(&self, pattern: Pattern) -> Vec<usize> {
      self
         .segments
         .iter()
         .filter(|segment| segment.executable)
         .flat_map(|segment| {
            pattern
               .find_in(&segment.bytes)
               .into_iter()
               .map(|offset| segment.addr + offset)
         })
         .collect()
   }

   /// Addresses of `needle` in any segment. Linkers often merge `.rodata` into
   /// the executable segment, so strings are searched everywhere.
   #[must_use]
   pub fn find_bytes(&self, needle: &[u8]) -> Vec<usize> {
      self.find_in(needle, |_| true)
   }

   /// [`Image::find_bytes`], lazily in address order, so a caller can stop at
   /// the first hit it accepts instead of scanning (and faulting in) the rest.
   pub fn find_bytes_iter<'a>(&'a self, needle: &'a [u8]) -> impl Iterator<Item = usize> + 'a {
      self.segments.iter().flat_map(move |segment| {
         memmem::find_iter(&segment.bytes, needle).map(move |offset| segment.addr + offset)
      })
   }

   /// Pointer-aligned slots in non-executable segments that hold `target`.
   #[must_use]
   pub fn pointers_to(&self, target: usize) -> Vec<usize> {
      let width = size_of::<usize>();
      self
         .find_in(&target.to_ne_bytes(), |segment| !segment.executable)
         .into_iter()
         .filter(|slot| slot % width == 0)
         .collect()
   }

   fn find_in(&self, needle: &[u8], keep: impl Fn(&Segment<'_>) -> bool) -> Vec<usize> {
      self
         .segments
         .iter()
         .filter(|segment| keep(segment))
         .flat_map(|segment| {
            memmem::find_iter(&segment.bytes, needle).map(|offset| segment.addr + offset)
         })
         .collect()
   }

   #[must_use]
   pub fn read(&self, addr: usize, len: usize) -> Option<&[u8]> {
      let segment = self
         .segments
         .iter()
         .find(|segment| segment.contains(addr))?;
      let offset = addr - segment.addr;
      segment.bytes.get(offset..offset + len)
   }

   #[must_use]
   pub fn read_usize(&self, addr: usize) -> Option<usize> {
      Some(usize::from_ne_bytes(
         self.read(addr, size_of::<usize>())?.try_into().ok()?,
      ))
   }

   #[must_use]
   pub fn read_u32(&self, addr: usize) -> Option<u32> {
      Some(u32::from_le_bytes(self.read(addr, 4)?.try_into().ok()?))
   }

   #[must_use]
   pub fn is_code(&self, addr: usize) -> bool {
      self
         .segments
         .iter()
         .any(|segment| segment.executable && segment.contains(addr))
   }

   /// Whether an FDE in `.eh_frame_hdr` starts at `addr`. Linkers often put
   /// `.rodata` in the executable segment, so [`Image::is_code`] alone accepts
   /// pointers to strings. Images without a search table fall back to it.
   #[must_use]
   pub fn is_function(&self, addr: usize) -> bool {
      if self.eh_frame_hdr.is_none() {
         return self.is_code(addr);
      }
      self.function_containing(addr) == Some(addr)
   }

   /// Start of the last FDE in `.eh_frame_hdr`'s search table at or below
   /// `addr`. The table holds starts only, so an address past a function's end
   /// still maps to it.
   #[must_use]
   pub fn function_containing(&self, addr: usize) -> Option<usize> {
      const UDATA4: u8 = 0x03;
      const SDATA4: u8 = 0x0B;
      const DATAREL_SDATA4: u8 = 0x3B;

      let hdr = self.eh_frame_hdr?;
      let &[1, frame_encoding, UDATA4, DATAREL_SDATA4] = self.read(hdr, 4)? else {
         return None;
      };
      if !matches!(frame_encoding & 0x0F, UDATA4 | SDATA4) {
         return None;
      }

      let word = |at: usize| Some(i32::from_ne_bytes(self.read(at, 4)?.try_into().ok()?));
      let start = |index: usize| {
         word(hdr + 12 + index * 8).and_then(|rel| hdr.checked_add_signed(rel as isize))
      };
      let count = usize::try_from(word(hdr + 8)?).ok()?;
      let (mut low, mut high) = (0, count);
      while low < high {
         let mid = low + (high - low) / 2;
         if start(mid)? <= addr {
            low = mid + 1;
         } else {
            high = mid;
         }
      }
      start(low.checked_sub(1)?)
   }
}
