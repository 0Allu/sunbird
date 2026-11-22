//! Loaded ELF modules via dl_iterate_phdr: segments, build ID, in-memory dynsym.
use std::ffi::{c_char, c_int, c_void, CStr};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}
#[repr(C)]
struct DlPhdrInfo {
    addr: usize,
    name: *const c_char,
    phdr: *const Phdr,
    phnum: u16,
}
extern "C" {
    fn dl_iterate_phdr(cb: unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int, data: *mut c_void) -> c_int;
}
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_NOTE: u32 = 4;
const PF_X: u32 = 1;

#[derive(Clone, Copy)]
pub struct Module {
    pub base: usize,
    phdr: *const Phdr,
    phnum: u16,
}
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

impl Module {
    /// First loaded module whose path contains `name`.
    pub fn find(name: &str) -> Option<Module> {
        struct Q<'a> {
            name: &'a str,
            out: Option<Module>,
        }
        unsafe extern "C" fn cb(i: *mut DlPhdrInfo, _: usize, d: *mut c_void) -> c_int {
            let (i, q) = (&*i, &mut *(d as *mut Q));
            if i.name.is_null() || !CStr::from_ptr(i.name).to_string_lossy().contains(q.name) {
                return 0;
            }
            q.out = Some(Module { base: i.addr, phdr: i.phdr, phnum: i.phnum });
            1
        }
        let mut q = Q { name, out: None };
        unsafe { dl_iterate_phdr(cb, &mut q as *mut Q as *mut c_void) };
        q.out
    }

    pub fn phdrs(&self) -> &'static [Phdr] {
        unsafe { std::slice::from_raw_parts(self.phdr, self.phnum as usize) }
    }

    fn loads(&self) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
        self.phdrs().iter().filter(|p| p.p_type == PT_LOAD)
            .map(|p| (self.base + p.p_vaddr as usize, p.p_memsz as usize, p.p_flags & PF_X != 0))
    }

    pub fn contains(&self, a: usize) -> bool { self.loads().any(|(s, n, _)| a >= s && a < s + n) }
    pub fn is_code(&self, a: usize) -> bool { self.loads().any(|(s, n, x)| x && a >= s && a < s + n) }

    /// Executable segments as (address, bytes), for signature scans.
    pub fn code(&self) -> Vec<(usize, &'static [u8])> {
        self.loads().filter(|l| l.2).map(|(s, n, _)| (s, unsafe { std::slice::from_raw_parts(s as *const u8, n) })).collect()
    }

    /// NT_GNU_BUILD_ID payload.
    pub fn build_id(&self) -> Option<&'static [u8]> {
        for p in self.phdrs().iter().filter(|p| p.p_type == PT_NOTE) {
            let (mut at, end) = (self.base + p.p_vaddr as usize, self.base + (p.p_vaddr + p.p_memsz) as usize);
            while at + 12 <= end {
                let w = |o: usize| unsafe { (at as *const u32).add(o).read_unaligned() } as usize;
                let (nsz, dsz, ty) = (w(0), w(1), w(2));
                let desc = at + 12 + ((nsz + 3) & !3);
                if ty == 3 && nsz == 4 && unsafe { std::slice::from_raw_parts((at + 12) as *const u8, 4) } == b"GNU\0" {
                    return Some(unsafe { std::slice::from_raw_parts(desc as *const u8, dsz) });
                }
                at = desc + ((dsz + 3) & !3);
            }
        }
        None
    }

    /// Exported symbol from the in-memory .dynsym (works across linker namespaces, unlike dlsym).
    pub fn dlsym(&self, name: &str) -> Option<usize> {
        #[repr(C)]
        struct Sym {
            name: u32,
            info: u8,
            other: u8,
            shndx: u16,
            value: u64,
            size: u64,
        }
        let dynp = self.phdrs().iter().find(|p| p.p_type == PT_DYNAMIC)?;
        let mut d = (self.base + dynp.p_vaddr as usize) as *const [u64; 2];
        let (mut sym, mut strt, mut strsz) = (0usize, 0usize, 0usize);
        unsafe {
            while (*d)[0] != 0 {
                let v = (*d)[1] as usize;
                match (*d)[0] {
                    6 => sym = self.base + v,
                    5 => strt = self.base + v,
                    10 => strsz = v,
                    _ => {}
                }
                d = d.add(1);
            }
            if sym == 0 || strt <= sym {
                return None;
            }
            // .dynsym precedes .dynstr: bound the table by the string table's start
            let n = ((strt - sym) / std::mem::size_of::<Sym>()).min(200_000);
            let syms = std::slice::from_raw_parts(sym as *const Sym, n);
            syms.iter().skip(1)
                .find(|s| s.value != 0 && (s.name as usize) < strsz
                    && CStr::from_ptr((strt + s.name as usize) as *const c_char).to_bytes() == name.as_bytes())
                .map(|s| self.base + s.value as usize)
        }
    }
}
