//! Where Phoenix's gateway handlers live and how they are called.
//!
//! The event set is never compiled in. [`GatewayRegistry`](registry::GatewayRegistry)
//! walks the target's registry at load time by event name, so a new Discord
//! build brings its own events and needs no table entry.
//! `cargo run -p sigscan -- liblibdiscord_wasm_host.so` dumps what it finds.

use std::ffi::c_void;

#[cfg(feature = "file")] pub mod file;
pub mod registry;

/// Libraries that carry the gateway registry, Phoenix's and the stand-in's.
pub const LIBRARIES: &[&str] = &["liblibdiscord_wasm_host.so", "libstand_in.so"];

/// Name of the plugin bundle the patcher stores next to the loader in the APK's
/// native library directory, a JSON object of plugin name to source.
pub const PLUGIN_BUNDLE: &str = "libplm.plugins.so";

/// Offset of the handler pointer in a gateway registry entry `{name ptr, name
/// len, handler}`.
pub const GATEWAY_HANDLER_FIELD: usize = 2 * size_of::<usize>();

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RawStr {
   pub ptr: *const u8,
   pub len: usize,
}

/// Phoenix's gateway event, `name` and `data` are borrowed UTF-8 with `data`
/// holding the raw dispatch JSON. `rest` is opaque and copied through
/// unchanged.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GatewayEvent {
   pub name: RawStr,
   pub data: RawStr,
   pub rest: [u64; 4],
}

/// A 32-byte Rust `Result`, returned through x8 on arm64 and a hidden pointer
/// on x86_64.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct HandlerResult(pub [u64; 4]);

impl HandlerResult {
   pub const OK: Self = Self([u64::MAX, 0, 0, 0]);

   #[must_use]
   pub const fn is_ok(&self) -> bool {
      self.0[0] == u64::MAX
   }
}

/// A gateway registry handler, `handler(store, event, ctx)`.
pub type GatewayHandler =
   unsafe extern "C" fn(*mut c_void, *const GatewayEvent, *mut c_void) -> HandlerResult;
