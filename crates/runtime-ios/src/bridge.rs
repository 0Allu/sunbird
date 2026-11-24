//! The host hands the guest every view and host event through
//! `vtable[DISPATCH](data, bytes, len, out)` on the guest's trait object. Only
//! the live `GuestHost` leads to that vtable, so [`arm`] looks for it once the
//! guest handles gateway events, then swaps its dispatch slot for
//! [`trampoline`], which logs each frame's size to the debug shell.

use std::{
   ffi::c_void,
   mem::transmute,
   panic::{
      AssertUnwindSafe,
      catch_unwind,
   },
   path::Path,
   ptr,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::ios::{
   self,
   DISPATCH,
   GUEST_TRAIT,
   HOST_GUEST,
   TRAIT_VTABLE,
};
use serde::Serialize;

type Dispatch = unsafe extern "C" fn(*mut c_void, *const u8, usize, *mut c_void) -> *mut c_void;

static ARMED: AtomicBool = AtomicBool::new(false);
static ORIGINAL: OnceLock<Dispatch> = OnceLock::new();

#[derive(Debug, flub::Error)]
enum BridgeError {
   #[error("{0}")]
   Anchor(String),
   #[error("no guest vtable behind any app image's pointers")]
   NotFound,
   #[error("{0} guest vtables match, refusing to guess")]
   Ambiguous(usize),
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
}

unsafe extern "C" {
   static mach_task_self_: u32;
   fn vm_read_overwrite(
      task: u32,
      address: usize,
      size: usize,
      data: usize,
      read: *mut usize,
   ) -> i32;
}

/// Hooks the bridge on a thread of its own, once.
pub fn arm() {
   if ARMED.swap(true, Ordering::AcqRel) {
      return;
   }
   let Some(state) = phoenix_runtime::state() else {
      return;
   };
   let spawned = std::thread::Builder::new()
      .name("phoenix-bridge".to_owned())
      .spawn(move || {
         match install(&state.image) {
            Ok(vtable) => log::info!("bridge: hooked the guest vtable at {vtable:#x}"),
            Err(err) => log::warn!("bridge: {err}, view events stay unhooked"),
         }
      });
   if let Err(err) = spawned {
      log::warn!("bridge: {err}");
   }
}

/// One word of the process's memory, or `None` where nothing is mapped.
fn word(addr: usize) -> Option<usize> {
   let mut value = 0usize;
   let mut read = 0usize;
   // SAFETY: the kernel writes at most `size_of::<usize>()` bytes into
   // `value`, and fails rather than faults on an unmapped address.
   let status = unsafe {
      vm_read_overwrite(
         mach_task_self_,
         addr,
         size_of::<usize>(),
         (&raw mut value) as usize,
         &raw mut read,
      )
   };
   (status == 0 && read == size_of::<usize>()).then_some(value)
}

/// The guest vtable behind `host`, a heap pointer that may be a `GuestHost`.
fn vtable_of(image: &Image<'_>, host: usize) -> Option<usize> {
   let guest = word(host.checked_add(HOST_GUEST)?)?;
   let object = word(guest.checked_add(GUEST_TRAIT)?)?;
   let vtable = word(object.checked_add(TRAIT_VTABLE)?)?;
   // A Rust vtable is a static of the guest's image, and its drop and
   // dispatch entries are code.
   let entries = [
      image.read_usize(vtable)?,
      image.read_usize(vtable + DISPATCH)?,
   ];
   entries
      .iter()
      .all(|&entry| image.is_code(entry))
      .then_some(vtable)
}

fn install(image: &Image<'static>) -> Result<usize, BridgeError> {
   ios::bridge(image).map_err(BridgeError::Anchor)?;

   // SAFETY: dyld keeps the images mapped while this scans them.
   let images = unsafe { Image::loaded() };
   let bundle = images
      .first()
      .and_then(|main| Path::new(&main.path).parent())
      .ok_or(BridgeError::NotFound)?;
   let app = images
      .iter()
      .filter(|candidate| Path::new(&candidate.path).starts_with(bundle))
      .collect::<Vec<_>>();
   let in_app = |addr: usize| {
      app.iter()
         .flat_map(|candidate| candidate.segments.iter())
         .any(|segment| segment.contains(addr))
   };

   // `GuestHost` lives on the heap, so a slot holding it points outside every
   // image.
   let mut vtables = app
      .iter()
      .flat_map(|candidate| candidate.segments.iter())
      .filter(|segment| !segment.executable)
      .flat_map(|segment| segment.bytes.as_chunks::<8>().0.iter())
      .map(|chunk| usize::from_ne_bytes(*chunk))
      .filter(|&host| host != 0 && host % 8 == 0 && !in_app(host))
      .filter_map(|host| vtable_of(image, host))
      .collect::<Vec<_>>();
   vtables.sort_unstable();
   vtables.dedup();
   let vtable = match vtables[..] {
      [] => return Err(BridgeError::NotFound),
      [only] => only,
      _ => return Err(BridgeError::Ambiguous(vtables.len())),
   };

   let slot = vtable + DISPATCH;
   let dispatch = image.read_usize(slot).ok_or(BridgeError::NotFound)?;
   // SAFETY: `vtable_of` checked that the slot holds code in the guest's
   // image, and the bridge calls it with this signature.
   let original = unsafe { transmute::<usize, Dispatch>(dispatch) };
   let _ = ORIGINAL.set(original);
   let hook = trampoline as Dispatch as usize;
   // SAFETY: the slot is an aligned vtable word in the guest's image, and the
   // bridge accepts either function.
   let previous = unsafe { image.swap_words(&[(slot, hook)]) }.map_err(BridgeError::Swap)?;
   match previous[..] {
      [held] if held == dispatch => Ok(vtable),
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
      phoenix_runtime::remote::record(&Trace::View { bytes: len });
   }));
   let Some(original) = ORIGINAL.get() else {
      return ptr::null_mut();
   };
   // SAFETY: the bridge's own arguments, passed on unchanged.
   unsafe { original(data, bytes, len, out) }
}
