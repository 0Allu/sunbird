//! libplg.so. The patcher renames liblibdiscord_wasm_host.so's DT_NEEDED "liblog.so" to
//! "libplg.so" (same length); libplg itself needs liblog.so, so symbol lookup is unchanged. Our
//! constructor runs after the host lib is mapped and relocated but before its own constructors,
//! so the registry hooks are in place before Phoenix handles its first event.
use hook::module::Module;
use hook::{info, warn};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::io::{Read, Seek, SeekFrom};

#[used]
#[link_section = ".init_array"]
static INIT: extern "C" fn() = init;

extern "C" fn init() {
    let Some(cfg) = sigs::get() else { return }; // not configured by the patcher
    let Some(host) = Module::find("liblibdiscord_wasm_host.so") else { return warn!("host lib not loaded") };
    let id = host.build_id().unwrap_or_default();
    if id != cfg.build_id {
        return warn!("host lib build {} is not the one this APK was patched for ({}); plugins off", hex(id), hex(&cfg.build_id));
    }
    let n = unsafe { hook::registry::install(&host, host.base + cfg.registry_anchor as usize) };
    if n == 0 { return warn!("gateway registry not found; plugins off") }
    if cfg.flags & sigs::FLAG_RUNTIME == 0 { return }
    let me = (cfg.me_ser != 0).then(|| (host.base + cfg.me_ser as usize, cfg.me_val as usize));
    let bundle = bundle();
    info!("libplg: {n} events hooked, plugin bundle {}", bundle.as_ref().map_or("none".into(), |b| format!("{} bytes", b.len())));
    runtime::start(runtime::Config { dir: files_dir(), bundle, host_base: host.base, me });
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }

/// files/phoenix-js of this app (user-aware: /data/user/<n>/<pkg>). Frida plugins use files/phoenix-plugins.
fn files_dir() -> String {
    extern "C" { fn getuid() -> u32; }
    let cmd = std::fs::read("/proc/self/cmdline").unwrap_or_default();
    let pkg = String::from_utf8_lossy(cmd.split(|&b| b == 0 || b == b':').next().unwrap_or_default()).into_owned();
    let dir = format!("/data/user/{}/{pkg}/files/phoenix-js", unsafe { getuid() } / 100_000);
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The plugin bundle the patcher put next to us: lib/<abi>/libplg.js.so, read straight from the
/// APK (native libs are stored, not extracted) or from the lib dir if they were extracted.
fn bundle() -> Option<String> {
    #[repr(C)]
    struct DlInfo { fname: *const c_char, fbase: *mut c_void, sname: *const c_char, saddr: *mut c_void }
    extern "C" { fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int; }
    let mut i: DlInfo = unsafe { std::mem::zeroed() };
    if unsafe { dladdr(init as *const c_void, &mut i) } == 0 || i.fname.is_null() { return None; }
    let path = unsafe { CStr::from_ptr(i.fname) }.to_string_lossy().into_owned();
    let bytes = match path.split_once("!/") {
        Some((apk, inner)) => stored_entry(apk, &format!("{}/libplg.js.so", inner.rsplit_once('/')?.0))?,
        None => std::fs::read(format!("{}/libplg.js.so", path.rsplit_once('/')?.0)).ok()?,
    };
    String::from_utf8(bytes).ok()
}

/// Reads an uncompressed (stored) zip entry.
fn stored_entry(zip: &str, name: &str) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(zip).ok()?;
    let len = f.seek(SeekFrom::End(0)).ok()?;
    let tail = len.min(65_557);
    let mut t = vec![0; tail as usize];
    f.seek(SeekFrom::Start(len - tail)).ok()?;
    f.read_exact(&mut t).ok()?;
    let u16_ = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_ = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let e = (0..t.len().saturating_sub(21)).rev().find(|&o| u32_(&t, o) == 0x0605_4b50)?;
    let (cd_size, cd_off) = (u32_(&t, e + 12), u32_(&t, e + 16));
    let mut cd = vec![0; cd_size];
    f.seek(SeekFrom::Start(cd_off as u64)).ok()?;
    f.read_exact(&mut cd).ok()?;
    let mut o = 0;
    while o + 46 <= cd.len() && u32_(&cd, o) == 0x0201_4b50 {
        let (method, size, n, x, c, loc) = (u16_(&cd, o + 10), u32_(&cd, o + 20), u16_(&cd, o + 28), u16_(&cd, o + 30), u16_(&cd, o + 32), u32_(&cd, o + 42));
        if &cd[o + 46..o + 46 + n] == name.as_bytes() {
            if method != 0 { return None; }
            let mut lh = [0u8; 30];
            f.seek(SeekFrom::Start(loc as u64)).ok()?;
            f.read_exact(&mut lh).ok()?;
            f.seek(SeekFrom::Start((loc + 30 + u16_(&lh, 26) + u16_(&lh, 28)) as u64)).ok()?;
            let mut out = vec![0; size];
            f.read_exact(&mut out).ok()?;
            return Some(out);
        }
        o += 46 + n + x + c;
    }
    None
}
