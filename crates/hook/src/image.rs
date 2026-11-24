use std::{
   borrow::Cow,
   fmt,
   path::Path,
};
#[cfg(unix)]
use std::{
   ffi::{
      CStr,
      c_int,
      c_void,
   },
   slice,
   sync::atomic::{
      AtomicUsize,
      Ordering,
   },
};
#[cfg(windows)]
use std::{
   ffi::{
      OsString,
      c_void,
   },
   os::windows::ffi::OsStringExt,
   slice,
   sync::atomic::{
      AtomicUsize,
      Ordering,
   },
};

use memchr::memmem;

use crate::pattern::Pattern;

/// `jmp [rip]` followed by its 8-byte target.
#[cfg(all(windows, target_arch = "x86_64"))]
const ABSOLUTE_JUMP_LEN: usize = 14;

#[cfg(all(windows, target_arch = "x86_64"))]
fn absolute_jump(to: usize) -> [u8; ABSOLUTE_JUMP_LEN] {
   let mut jump = [0; ABSOLUTE_JUMP_LEN];
   jump[..2].copy_from_slice(&[0xFF, 0x25]);
   jump[6..].copy_from_slice(&to.to_le_bytes());
   jump
}

const NT_GNU_BUILD_ID: u32 = 3;
#[cfg(unix)] const PF_X: u32 = 1;
#[cfg(unix)] const PF_W: u32 = 2;
#[cfg(unix)] const PF_R: u32 = 4;
#[cfg(unix)] const PT_ARM_EXIDX: u32 = 0x7000_0001;

/// The `NT_GNU_BUILD_ID` note of an ELF module, 1 to 32 bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuildId {
   len:   u8,
   bytes: [u8; 32],
}

impl BuildId {
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

/// A loaded or offline binary module (ELF or PE), mapped with its relative
/// relocations applied so pointer slots hold the addresses code would see.
pub struct Image<'a> {
   pub path:          String,
   pub build_id:      Option<BuildId>,
   pub segments:      Vec<Segment<'a>>,
   /// Address of `.eh_frame_hdr`, from `PT_GNU_EH_FRAME`.
   pub eh_frame_hdr:  Option<usize>,
   /// `.ARM.exidx` address and byte length, from `PT_ARM_EXIDX`. ARM32 unwinds
   /// through this table, so it carries the function starts `.eh_frame_hdr`
   /// holds on other targets.
   pub arm_exidx:     Option<(usize, usize)>,
   /// PE64 exception directory `(image_base, pdata_addr, pdata_len)` holding
   /// sorted 12-byte `RUNTIME_FUNCTION` `{begin_rva, end_rva, unwind_rva}`
   /// entries.
   pub pdata:         Option<(usize, usize, usize)>,
   /// Width of a pointer in this image, 4 for 32-bit and 8 for 64-bit. A file
   /// parsed offline keeps the target's width, not the host's.
   pub pointer_width: usize,
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
            arm_exidx: None,
            pdata: None,
            pointer_width: size_of::<usize>(),
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
               PT_ARM_EXIDX => image.arm_exidx = Some((addr, len)),
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

#[cfg(windows)]
mod win32 {
   use std::ffi::c_void;

   pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
   pub const MEM_COMMIT_RESERVE: u32 = 0x3000;
   pub const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;
   pub const IMAGE_SCN_MEM_READ: u32 = 0x4000_0000;
   pub const IMAGE_SCN_MEM_WRITE: u32 = 0x8000_0000;

   #[repr(C)]
   pub struct SystemInfo {
      pub arch_or_oem: u32,
      pub page_size:   u32,
      pub _rest:       [u8; 40],
   }

   #[link(name = "kernel32")]
   unsafe extern "system" {
      pub fn GetCurrentProcess() -> *mut c_void;
      pub fn GetSystemInfo(info: *mut SystemInfo);
      pub fn VirtualAlloc(
         addr: *const c_void,
         size: usize,
         kind: u32,
         protect: u32,
      ) -> *mut c_void;
      pub fn VirtualProtect(
         addr: *mut c_void,
         size: usize,
         new_protect: u32,
         old_protect: *mut u32,
      ) -> i32;
      pub fn K32EnumProcessModules(
         process: *mut c_void,
         modules: *mut *mut c_void,
         cb: u32,
         needed: *mut u32,
      ) -> i32;
      pub fn K32GetModuleFileNameExW(
         process: *mut c_void,
         module: *mut c_void,
         filename: *mut u16,
         size: u32,
      ) -> u32;
   }
}

#[cfg(windows)]
impl Image<'static> {
   /// Every loaded PE module in the current process.
   ///
   /// # Safety
   ///
   /// The segments borrow mapped memory. The modules stay loaded while the
   /// images live, and no thread writes the writable segments during a scan.
   pub unsafe fn loaded() -> Vec<Self> {
      // SAFETY: GetCurrentProcess returns a valid pseudo-handle.
      let process = unsafe { win32::GetCurrentProcess() };
      let mut needed = 0u32;
      let mut handles = vec![std::ptr::null_mut::<c_void>(); 256];
      loop {
         let cb = u32::try_from(handles.len() * size_of::<*mut c_void>()).unwrap_or(u32::MAX);
         // SAFETY: `handles` has space for `cb` bytes.
         let ok = unsafe {
            win32::K32EnumProcessModules(process, handles.as_mut_ptr(), cb, &raw mut needed)
         };
         if ok == 0 {
            return Vec::new();
         }
         let count = (needed as usize) / size_of::<*mut c_void>();
         if count <= handles.len() {
            handles.truncate(count);
            break;
         }
         handles.resize(count, std::ptr::null_mut());
      }

      handles
         .into_iter()
         .filter_map(|handle| {
            // SAFETY: `handle` is an HMODULE reported by K32EnumProcessModules.
            unsafe { Self::from_hmodule(process, handle) }
         })
         .collect()
   }

   /// # Safety
   ///
   /// `module` is a live `HMODULE` mapped into `process`.
   unsafe fn from_hmodule(process: *mut c_void, module: *mut c_void) -> Option<Self> {
      let base = module as usize;
      if base == 0 {
         return None;
      }
      let mut name_buf = [0u16; 1024];
      // SAFETY: `name_buf` is valid for its length.
      let name_len = unsafe {
         win32::K32GetModuleFileNameExW(
            process,
            module,
            name_buf.as_mut_ptr(),
            u32::try_from(name_buf.len()).ok()?,
         )
      } as usize;
      let path = OsString::from_wide(&name_buf[..name_len])
         .to_string_lossy()
         .into_owned();

      // SAFETY: a valid PE module starts with an IMAGE_DOS_HEADER (at least 64 bytes).
      let dos = unsafe { slice::from_raw_parts(base as *const u8, 64) };
      if &dos[0..2] != b"MZ" {
         return None;
      }
      let pe_off = usize::try_from(u32::from_le_bytes(dos[0x3C..0x40].try_into().ok()?)).ok()?;
      // SAFETY: NT headers sit within the mapped image header page.
      let nt = unsafe { slice::from_raw_parts((base + pe_off) as *const u8, 24) };
      if &nt[0..4] != b"PE\0\0" {
         return None;
      }
      let nsec = usize::from(u16::from_le_bytes(nt[6..8].try_into().ok()?));
      let opt_size = usize::from(u16::from_le_bytes(nt[20..22].try_into().ok()?));
      // SAFETY: optional header and section table follow the COFF header.
      let opt = unsafe { slice::from_raw_parts((base + pe_off + 24) as *const u8, opt_size) };
      let magic = u16::from_le_bytes(opt.get(0..2)?.try_into().ok()?);
      let (pointer_width, pdata) = match magic {
         0x020B => {
            // PE32+: Exception directory is data directory index 3 at optional header offset 112 + 3*8.
            let pdata = opt.get(136..144).and_then(|dir| {
               let rva = usize::try_from(u32::from_le_bytes(dir[0..4].try_into().ok()?)).ok()?;
               let len = usize::try_from(u32::from_le_bytes(dir[4..8].try_into().ok()?)).ok()?;
               (rva != 0 && len >= 12).then_some((base, base + rva, len))
            });
            (8, pdata)
         },
         0x010B => (4, None),
         _ => return None,
      };

      let sec_table = base + pe_off + 24 + opt_size;
      // SAFETY: `nsec` 40-byte section headers follow the optional header.
      let raw_secs = unsafe { slice::from_raw_parts(sec_table as *const u8, nsec * 40) };
      let mut segments = Vec::with_capacity(nsec);
      for hdr in raw_secs.chunks_exact(40) {
         let vsize = usize::try_from(u32::from_le_bytes(hdr[8..12].try_into().ok()?)).ok()?;
         let va = usize::try_from(u32::from_le_bytes(hdr[12..16].try_into().ok()?)).ok()?;
         let chars = u32::from_le_bytes(hdr[36..40].try_into().ok()?);
         if vsize == 0 || chars & win32::IMAGE_SCN_MEM_READ == 0 {
            continue;
         }
         let addr = base + va;
         // SAFETY: mapped readable PE section lives for the module's lifetime.
         let bytes = unsafe { slice::from_raw_parts(addr as *const u8, vsize) };
         segments.push(Segment {
            addr,
            bytes: Cow::Borrowed(bytes),
            executable: chars & win32::IMAGE_SCN_MEM_EXECUTE != 0,
            writable: chars & win32::IMAGE_SCN_MEM_WRITE != 0,
         });
      }

      Some(Self {
         path,
         build_id: None,
         segments,
         eh_frame_hdr: None,
         arm_exidx: None,
         pdata,
         pointer_width,
      })
   }

   /// Writes each `(slot, value)` pair into this module's mapped memory and
   /// returns the values the slots held. Read-only pages are made writable for
   /// the write and get their protection back afterwards.
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

#[cfg(windows)]
/// Atomically swaps each `(slot, value)` pair in mapped memory and returns the
/// values the slots held. Read-only pages are made writable for the write and
/// get their protection back afterwards.
///
/// # Safety
///
/// Every slot is a mapped, pointer-aligned word, and code that reads it accepts
/// either value.
pub unsafe fn swap_words(writes: &[(usize, usize)]) -> Result<Vec<usize>, crate::Error> {
   let mut info = win32::SystemInfo {
      arch_or_oem: 0,
      page_size:   4096,
      _rest:       [0; 40],
   };
   // SAFETY: GetSystemInfo fills the struct.
   unsafe { win32::GetSystemInfo(&raw mut info) };
   let page_size = (info.page_size as usize).max(4096);

   let mut pages = writes
      .iter()
      .map(|&(slot, _)| slot & !(page_size - 1))
      .collect::<Vec<usize>>();
   pages.sort_unstable();
   pages.dedup();

   let mut restore = Vec::with_capacity(pages.len());
   for page in pages {
      let mut old = 0u32;
      // SAFETY: `page` is a mapped page in this process.
      if unsafe {
         win32::VirtualProtect(
            page as *mut c_void,
            page_size,
            win32::PAGE_EXECUTE_READWRITE,
            &raw mut old,
         )
      } == 0
      {
         for &(done, prot) in &restore {
            let mut ignored = 0u32;
            // SAFETY: restoring previously changed page protection.
            unsafe {
               win32::VirtualProtect(done as *mut c_void, page_size, prot, &raw mut ignored);
            }
         }
         return Err(crate::Error::Protect {
            addr:   page,
            source: std::io::Error::last_os_error(),
         });
      }
      restore.push((page, old));
   }

   let previous = writes
      .iter()
      // SAFETY: the caller guarantees aligned, mapped slots, now writable.
      .map(|&(slot, value)| {
         unsafe { AtomicUsize::from_ptr(slot as *mut usize) }.swap(value, Ordering::AcqRel)
      })
      .collect();

   for (page, prot) in restore {
      let mut ignored = 0u32;
      // SAFETY: restoring the page's original protection.
      if unsafe { win32::VirtualProtect(page as *mut c_void, page_size, prot, &raw mut ignored) }
         == 0
      {
         return Err(crate::Error::Protect {
            addr:   page,
            source: std::io::Error::last_os_error(),
         });
      }
   }
   Ok(previous)
}

impl Image<'static> {
   /// Overwrites `bytes.len()` bytes at `addr` in this module, through
   /// [`Self::swap_words`] on the aligned words they cover.
   ///
   /// # Safety
   ///
   /// No thread runs or reads the bytes while they are written, which spans
   /// more than one atomic write when they cross a word boundary.
   pub unsafe fn write_code(&self, addr: usize, bytes: &[u8]) -> Result<(), crate::Error> {
      let span = addr..addr + bytes.len();
      let mut writes = Vec::new();
      for word in (span.start & !7..span.end).step_by(8) {
         let mut current: [u8; 8] = self
            .read(word, 8)
            .and_then(|current| current.try_into().ok())
            .ok_or(crate::Error::Unmapped { addr: word })?;
         for (at, byte) in (word..).zip(&mut current) {
            if span.contains(&at) {
               *byte = bytes[at - span.start];
            }
         }
         writes.push((word, usize::from_le_bytes(current)));
      }
      // SAFETY: aligned words in this module, under the caller's guarantee.
      unsafe { self.swap_words(&writes) }.map(drop)
   }

   /// Sends calls to the function at `addr` to `detour` with an absolute jump
   /// over its first `prologue_len` bytes, and returns a trampoline that runs
   /// those bytes and jumps back past them: the detour calls the trampoline to
   /// reach the original. The detour can run as soon as the jump is written,
   /// before this returns, so hook from `DllMain` or make it cope with a
   /// trampoline not stored yet.
   ///
   /// `prologue` must match at `addr`, cover whole instructions with no
   /// RIP-relative operands, and span at least the jump's 14 bytes, so the
   /// displaced instructions run unchanged from the trampoline.
   ///
   /// # Safety
   ///
   /// `detour` has the function's exact ABI, and no thread runs the
   /// function's first bytes while they are written.
   #[cfg(all(windows, target_arch = "x86_64"))]
   pub unsafe fn hook_entry(
      &self,
      addr: usize,
      prologue: Pattern,
      prologue_len: usize,
      detour: usize,
   ) -> Result<usize, crate::Error> {
      let refuse = |message: &str| {
         crate::Error::Attach {
            backend: "inline",
            target:  addr,
            message: message.to_owned(),
         }
      };
      let displaced = self
         .read(addr, prologue_len)
         .ok_or(crate::Error::Unmapped { addr })?;
      if prologue_len < ABSOLUTE_JUMP_LEN || prologue.find_in(displaced).first() != Some(&0) {
         return Err(refuse("the prologue does not match"));
      }
      let mut code = displaced.to_vec();
      code.extend(absolute_jump(addr + prologue_len));
      // SAFETY: a fresh allocation, filled before anything runs it.
      let trampoline = unsafe {
         win32::VirtualAlloc(
            std::ptr::null(),
            code.len(),
            win32::MEM_COMMIT_RESERVE,
            win32::PAGE_EXECUTE_READWRITE,
         )
      };
      if trampoline.is_null() {
         return Err(refuse("no memory for the trampoline"));
      }
      // SAFETY: `trampoline` holds `code.len()` writable bytes.
      unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), trampoline.cast::<u8>(), code.len()) };
      // SAFETY: forwarded from the caller.
      unsafe { self.write_code(addr, &absolute_jump(detour)) }?;
      Ok(trampoline as usize)
   }
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
      let width = self.pointer_width;
      self
         .find_in(&target.to_le_bytes()[..width], |segment| {
            !segment.executable
         })
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
      let mut word = [0u8; 8];
      let bytes = self.read(addr, self.pointer_width)?;
      word[..self.pointer_width].copy_from_slice(bytes);
      Some(usize::try_from(u64::from_le_bytes(word)).unwrap_or(usize::MAX))
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

   /// Whether an unwind table starts a function at `addr`. Linkers often put
   /// `.rodata` in the executable segment, so [`Image::is_code`] alone accepts
   /// pointers to strings. Images without a search table fall back to it.
   #[must_use]
   pub fn is_function(&self, addr: usize) -> bool {
      if let Some((base, len)) = self.arm_exidx {
         // Thumb code pointers carry bit 0, the exidx table holds the even
         // function address.
         let target = addr & !1;
         return self.exidx_containing(base, len / 8, target) == Some(target);
      }
      if self.pdata.is_some() {
         return self.function_containing(addr) == Some(addr);
      }
      if self.eh_frame_hdr.is_none() {
         return self.is_code(addr);
      }
      self.function_containing(addr) == Some(addr)
   }

   /// Start of the last function at or below `addr` in the unwind table. The
   /// table holds starts only, so an address past a function's end still maps
   /// to it. ARM32 reads `.ARM.exidx`, PE64 reads `.pdata`, every other target
   /// `.eh_frame_hdr`.
   #[must_use]
   pub fn function_containing(&self, addr: usize) -> Option<usize> {
      if let Some((base, len)) = self.arm_exidx {
         return self.exidx_containing(base, len / 8, addr);
      }
      if let Some((image_base, base, len)) = self.pdata {
         return self
            .pdata_bounds(image_base, base, len / 12, addr)
            .map(|(start, _)| start);
      }

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

   /// Binary search over `.ARM.exidx`, whose `count` entries are 8 bytes each
   /// and sorted by function address. The first word is a prel31 offset from the
   /// entry to the function it unwinds.
   fn exidx_containing(&self, base: usize, count: usize, addr: usize) -> Option<usize> {
      let start = |index: usize| {
         let at = base + index * 8;
         let word = self.read_u32(at)?;
         let offset = word & 0x7FFF_FFFF;
         let offset = if offset & 0x4000_0000 != 0 {
            (offset | 0x8000_0000) as i32
         } else {
            offset as i32
         };
         at.checked_add_signed(offset as isize)
      };
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

   /// Exact `[start, end)` bounds of the function containing `addr` when the
   /// image carries a PE64 `.pdata` exception directory.
   #[must_use]
   pub fn function_bounds(&self, addr: usize) -> Option<(usize, usize)> {
      let (image_base, base, len) = self.pdata?;
      self.pdata_bounds(image_base, base, len / 12, addr)
   }

   /// Binary search over PE64 `.pdata`, whose `count` `RUNTIME_FUNCTION` entries
   /// are 12 bytes (`begin_rva`, `end_rva`, `unwind_rva`), sorted by `begin_rva`.
   fn pdata_bounds(
      &self,
      image_base: usize,
      base: usize,
      count: usize,
      addr: usize,
   ) -> Option<(usize, usize)> {
      let rva = u32::try_from(addr.checked_sub(image_base)?).ok()?;
      let begin = |index: usize| self.read_u32(base + index * 12);
      let (mut low, mut high) = (0, count);
      while low < high {
         let mid = low + (high - low) / 2;
         if begin(mid)? <= rva {
            low = mid + 1;
         } else {
            high = mid;
         }
      }
      let index = low.checked_sub(1)?;
      let start = begin(index)?;
      let end = self.read_u32(base + index * 12 + 4)?;
      (rva < end).then(|| (image_base + start as usize, image_base + end as usize))
   }
}
