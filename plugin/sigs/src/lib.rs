//! Per-build table. The patcher finds these offsets in the exact host lib it patches and writes
//! them into this block in libplg.so (located by MAGIC), together with that lib's GNU build ID.
//! At startup the loader compares the build ID with the host lib actually loaded, so an updated
//! Discord with stale offsets runs without plugins instead of crashing.

/// Patcher-visible layout: keep in sync with web/lib/plugins.js (offsets in the comments).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Sigs {
    pub magic: [u8; 16],    // 0  "PHXPLUG-CONFIG2\0"
    pub version: u32,       // 16 0 until the patcher fills the block
    pub flags: u32,         // 20 FLAG_*
    pub build_id: [u8; 20], // 24 host lib NT_GNU_BUILD_ID
    _pad: u32,              // 44
    /// Address (host vaddr) of the MESSAGE_CREATE entry's handler field in the gateway
    /// registry. The loader walks the neighbouring {name, len, handler} entries from here.
    pub registry_anchor: u64, // 48
    /// `<Me as Serialize>` and the offset of the Me field in the store (0 = not found).
    pub me_ser: u64, // 56
    pub me_val: u64, // 64
}

pub const MAGIC: &[u8; 16] = b"PHXPLUG-CONFIG2\0";
pub const VERSION: u32 = 2;
/// Run the JS runtime (plugins come from the APK bundle and files/phoenix-plugins).
pub const FLAG_RUNTIME: u32 = 1;

#[no_mangle]
#[used]
pub static mut PHX_CONFIG: Sigs = Sigs {
    magic: *MAGIC,
    version: 0,
    flags: 0,
    build_id: [0; 20],
    _pad: 0,
    registry_anchor: 0,
    me_ser: 0,
    me_val: 0,
};

/// The block as the patcher left it (volatile: the compiler must not fold the defaults).
pub fn get() -> Option<Sigs> {
    let s = unsafe { std::ptr::read_volatile(std::ptr::addr_of!(PHX_CONFIG)) };
    (s.version == VERSION).then_some(s)
}

#[test]
fn layout_matches_patcher() {
    assert_eq!(std::mem::size_of::<Sigs>(), 72);
    assert_eq!(std::mem::offset_of!(Sigs, registry_anchor), 48);
    assert_eq!(std::mem::offset_of!(Sigs, me_val), 64);
}
