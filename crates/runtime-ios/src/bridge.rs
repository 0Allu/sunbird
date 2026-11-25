//! Both directions between Phoenix's Swift host and its Rust guest.
//!
//! The host hands the guest every view and host event through
//! `vtable[dispatch](data, bytes, len, out)` on the guest's trait object, a
//! static of the guest's image. [`arm`] swaps its dispatch slot for
//! [`trampoline`].
//!
//! The guest calls the host through `call(context, bytes, len, out)` in a
//! thread-local table the host fills on each guest thread. [`tap`] runs on each
//! thread that takes gateway or view events and swaps that entry for
//! [`host_trampoline`].
//!
//! Both log what crosses to the debug shell, frames by size and host calls by
//! module and method, since frames carry message text and request headers.

use std::{
   cell::Cell,
   ffi::c_void,
   mem::transmute,
   panic::{
      AssertUnwindSafe,
      catch_unwind,
   },
   ptr,
   slice,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::ios;
use serde::Serialize;

type Dispatch = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut c_void) -> *mut c_void;
type HostCall = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut *mut u8) -> isize;

/// A thread-local variable's descriptor, whose thunk returns this thread's
/// copy.
#[repr(C)]
struct Descriptor {
   thunk:  unsafe extern "C" fn(*const Self) -> *mut HostTable,
   key:    usize,
   offset: usize,
}

#[repr(C)]
struct HostTable {
   call:    Option<HostCall>,
   log:     usize,
   context: *mut c_void,
}

static ARMED: AtomicBool = AtomicBool::new(false);
static ORIGINAL: OnceLock<Dispatch> = OnceLock::new();
static HOST_TABLE: OnceLock<usize> = OnceLock::new();
static HOST_ORIGINAL: OnceLock<HostCall> = OnceLock::new();

thread_local! {
   static TAPPED: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug, flub::Error)]
enum BridgeError {
   #[error("{0}")]
   Anchor(String),
   #[error("the dispatch slot is unmapped")]
   Unmapped,
   #[error("the dispatch slot changed while it was swapped")]
   Raced,
   #[error("swapping the dispatch slot: {0}")]
   Swap(#[source] phoenix_hook::Error),
}

/// An entry in the debug shell's event log. Frames carry message text and
/// HTTP bodies, so only their size is kept.
#[derive(Serialize)]
#[serde(tag = "source", rename_all = "camelCase")]
enum Trace {
   View { bytes: usize },
   HostCall { module: u16, method: u16 },
}

/// Finds the guest's host table, for [`tap`] to swap on each guest thread.
pub fn locate_host_table(image: &Image<'_>) {
   match ios::host_table(image) {
      Ok(descriptor) => {
         let _ = HOST_TABLE.set(descriptor);
      },
      Err(err) => log::warn!("bridge: {err}, host calls stay unhooked"),
   }
}

/// Swaps this guest thread's host call for [`host_trampoline`], once the host
/// has filled the thread's table.
pub fn tap() {
   if TAPPED.get() {
      return;
   }
   let Some(&descriptor) = HOST_TABLE.get() else {
      return;
   };
   let descriptor = descriptor as *const Descriptor;
   // SAFETY: `ios::host_table` found the descriptor in the guest's image, and
   // dyld fills its thunk and key before any of the guest's code runs.
   let table = unsafe { ((*descriptor).thunk)(descriptor) };
   // SAFETY: the thunk returns this thread's table, which only this thread
   // touches.
   let Some(table) = (unsafe { table.as_mut() }) else {
      return;
   };
   let Some(call) = table.call else {
      return;
   };
   TAPPED.set(true);
   let original = *HOST_ORIGINAL.get_or_init(|| call);
   if call as usize == host_trampoline as HostCall as usize {
      return;
   }
   if call as usize != original as usize {
      log::warn!(
         "bridge: this thread's host call is {:#x}, leaving it",
         call as usize
      );
      return;
   }
   table.call = Some(host_trampoline);
   log::info!("bridge: hooked host calls on this guest thread");
}

/// Hooks the bridge, once.
pub fn arm(image: &Image<'static>) {
   if ARMED.swap(true, Ordering::AcqRel) {
      return;
   }
   match install(image) {
      Ok(vtable) => log::info!("bridge: hooked the guest vtable at {vtable:#x}"),
      Err(err) => log::warn!("bridge: {err}, view events stay unhooked"),
   }
}

fn install(image: &Image<'static>) -> Result<usize, BridgeError> {
   let bridge = ios::bridge(image).map_err(BridgeError::Anchor)?;
   let slot = bridge.vtable + bridge.dispatch;
   let dispatch = image.read_usize(slot).ok_or(BridgeError::Unmapped)?;
   // SAFETY: `ios::bridge` checked that the slot holds code in the guest's
   // image, and the bridge calls it with this signature.
   let original = unsafe { transmute::<usize, Dispatch>(dispatch) };
   let _ = ORIGINAL.set(original);
   let hook = trampoline as Dispatch as usize;
   // SAFETY: the slot is an aligned vtable word in the guest's image, and the
   // bridge accepts either function.
   let previous = unsafe { image.swap_words(&[(slot, hook)]) }.map_err(BridgeError::Swap)?;
   match previous[..] {
      [held] if held == dispatch => Ok(bridge.vtable),
      [held] => {
         // SAFETY: as above, putting back what the slot held.
         unsafe { image.swap_words(&[(slot, held)]) }.map_err(BridgeError::Swap)?;
         Err(BridgeError::Raced)
      },
      _ => Err(BridgeError::Raced),
   }
}

unsafe extern "C" fn trampoline(
   data: *mut c_void,
   bytes: *const u8,
   len: usize,
   out: *mut c_void,
) -> *mut c_void {
   let _ = catch_unwind(AssertUnwindSafe(|| {
      tap();
      phoenix_runtime::remote::record(&Trace::View { bytes: len });
   }));
   let Some(original) = ORIGINAL.get() else {
      return ptr::null_mut();
   };
   // SAFETY: the bridge's own arguments, passed on unchanged.
   unsafe { original(data, bytes, len, out) }
}

unsafe extern "C" fn host_trampoline(
   context: *mut c_void,
   bytes: *const u8,
   len: usize,
   out: *mut *mut u8,
) -> isize {
   let _ = catch_unwind(AssertUnwindSafe(|| {
      if !phoenix_runtime::remote::enabled() || bytes.is_null() {
         return;
      }
      // SAFETY: the guest's frame, borrowed for this call.
      let frame = unsafe { slice::from_raw_parts(bytes, len) };
      if let Some((module, method)) = phoenix_wire::HostCall::target(frame) {
         phoenix_runtime::remote::record(&Trace::HostCall { module, method });
      }
   }));
   let Some(original) = HOST_ORIGINAL.get() else {
      return -1;
   };
   // SAFETY: the guest's own arguments, passed on unchanged.
   unsafe { original(context, bytes, len, out) }
}
