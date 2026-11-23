//! Staff unlock. Phoenix checks staff as `(user.flags | flag_override) & 1`
//! against its store state, inlined in about 30 places. Every gateway handler
//! gets that state as its first argument, so the gateway detour sets
//! `flag_override` there while the reducer holds it, and every copy of the
//! check reads it.

use std::{
   ffi::c_void,
   ptr,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         Ordering,
      },
   },
};

use phoenix_hook::{
   Image,
   Pattern,
};

/// The tail of a staff check: `add x8, x8, #0x8000` then the three
/// `flag_override` loads and the `csel` that ORs the staff bit. The offsets in
/// the first two loads are wildcarded and read back from the match, so the
/// write targets whatever field this build reads. 5 copies on 356207.
const STAFF_CHECK: Pattern = Pattern::new(
   "08 21 40 91 09 75 42 F9 3F 09 00 F1 61 00 00 54 F3 03 1F 2A 0D 00 00 14 0A ?? ?? B9 0B ?? ?? \
    B9 0C F1 44 B9 5F 01 00 71 6A 11 9F 1A",
);
/// Byte offsets of the `add` and the tag and value `ldr` inside a match.
const ADD_BASE: usize = 0;
const TAG_LOAD: usize = 24;
const VALUE_LOAD: usize = 28;
const STAFF: u32 = 1;

static UNLOCKED: AtomicBool = AtomicBool::new(true);
static OVERRIDE: OnceLock<Option<Field>> = OnceLock::new();

/// `flag_override: Option<u32>` in the store state, as byte offsets of its tag
/// and value decoded from the staff check.
#[derive(Clone, Copy)]
struct Field {
   tag:   usize,
   value: usize,
}

#[must_use]
pub fn enabled() -> bool {
   UNLOCKED.load(Ordering::Acquire)
}

pub fn set_enabled(on: bool) {
   UNLOCKED.store(on, Ordering::Release);
}

/// Finds the `flag_override` offsets off the main thread, since the scan reads
/// most of the host library.
pub fn start(on: bool, image: &'static Image<'static>) {
   set_enabled(on);
   std::thread::spawn(|| {
      OVERRIDE.get_or_init(|| locate(image));
   });
}

/// The `flag_override` field decoded from a staff check, when every match
/// agrees on the offsets.
fn locate(image: &Image<'_>) -> Option<Field> {
   let mut field: Option<Field> = None;
   for base in image.scan(STAFF_CHECK) {
      let add = image.read_u32(base + ADD_BASE)?;
      let tag = image.read_u32(base + TAG_LOAD)?;
      let value = image.read_u32(base + VALUE_LOAD)?;
      let found = Field {
         tag:   add_offset(add) + ldr_offset(tag),
         value: add_offset(add) + ldr_offset(value),
      };
      if field.is_some_and(|seen| seen.tag != found.tag || seen.value != found.value) {
         log::warn!("staff: staff checks disagree on flag_override, the flag stays off");
         return None;
      }
      field = Some(found);
   }
   if field.is_none() {
      log::warn!("staff: no staff check found, the flag stays off");
   }
   field
}

/// The byte offset an `add xd, xn, #imm{, lsl #12}` adds.
const fn add_offset(word: u32) -> usize {
   let imm = ((word >> 10) & 0xFFF) as usize;
   imm << if (word >> 22) & 1 == 1 { 12 } else { 0 }
}

/// The byte offset a 32-bit `ldr wt, [xn, #imm]` loads from, scaled by 4.
const fn ldr_offset(word: u32) -> usize {
   (((word >> 10) & 0xFFF) as usize) << 2
}

/// Sets or clears the staff bit in `flag_override`, before each gateway event
/// so it survives Phoenix rebuilding the state.
///
/// # Safety
///
/// `state` is the store state a gateway handler was called with, and the
/// caller runs inside that handler.
pub unsafe fn apply(state: *mut c_void) {
   let Some(Some(field)) = OVERRIDE.get() else {
      return;
   };
   // SAFETY: the staff check reads both fields from this state as u32s.
   unsafe {
      let tag = state.byte_add(field.tag).cast::<u32>();
      let value = state.byte_add(field.value).cast::<u32>();
      let had = ptr::read(tag) != 0;
      let current = if had { ptr::read(value) } else { 0 };
      let wanted = if enabled() {
         current | STAFF
      } else {
         current & !STAFF
      };
      if wanted == current {
         return;
      }
      ptr::write(value, wanted);
      ptr::write(tag, u32::from(wanted != 0 || had));
   }
}
