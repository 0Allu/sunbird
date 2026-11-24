//! Locators for iOS Phoenix, `PhoenixFatClient.framework`, where the Swift host
//! and the Rust guest share one image.

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

use crate::{
   Found,
   one,
};

/// The host's call into the guest, which takes `{data, vtable}` and calls
/// `vtable[DISPATCH](data, bytes, len, out)`: `mov x19, x3; ldp x8, x9, [x0];
/// cmp x2, #0; csinc x1, x1, xzr, ne; ldr x9, [x9, #0x20]; mov x0, x8; blr x9`.
const BRIDGE: Pattern = Pattern::new(
   "F3 03 03 AA 08 24 40 A9 5F 00 00 F1 21 14 9F 9A 29 11 40 F9 E0 03 08 AA 20 01 3F D6",
);

/// Offsets from the host's `GuestHost` to the guest, the guest to its trait
/// object, and the trait object to its vtable.
///
/// Read on 357.0 (build 124398). Discovery checks every hop, so a layout
/// change leaves the bridge hook off.
pub const HOST_GUEST: usize = 0x48;
pub const GUEST_TRAIT: usize = 0x10;
pub const TRAIT_VTABLE: usize = 0x08;
/// `dispatch` in the trait's vtable, after drop, size, align and one method.
pub const DISPATCH: usize = 0x20;

/// The host's call into the guest, the anchor for finding the guest's vtable.
pub fn bridge(image: &Image<'_>) -> Found<usize> {
   one("guest bridge", &image.scan(BRIDGE))
}

/// The guest's call into the host: `mov x2, x1; mov x1, x0; mov x19, x8;
/// adrp x0, TLV; add x0, x0, TLV; ldr x8, [x0]; blr x8` gets this thread's
/// host table, then `ldr x8, [x0]; cbz x8; ldr x0, [x0, #0x10]` calls its
/// first entry with its third.
const HOST_CALL: Pattern = Pattern::new(
   "E2 03 01 AA E1 03 00 AA ?? 03 08 AA ?? ?? ?? ?? ?? ?? ?? ?? 08 00 40 F9 00 01 3F D6 08 00 40 \
    F9 ?? ?? ?? B4 00 08 40 F9",
);

/// The thread-local descriptor `{thunk, key, offset}` of the guest's host
/// table `{call, log, context}`, which the host fills on each guest thread.
pub fn host_table(image: &Image<'_>) -> Found<usize> {
   let at = one("host call", &image.scan(HOST_CALL))? + 12;
   let words = image
      .read(at, 8)
      .ok_or("host call: the table load is unmapped")?;
   let adrp = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
   let add = u32::from_le_bytes([words[4], words[5], words[6], words[7]]);
   // `adrp x0` and an unshifted `add x0, x0`.
   if adrp & 0x9F00_001F != 0x9000_0000 || add & 0xFFC0_03FF != 0x9100_0000 {
      return Err(format!("host call at {at:#x}: no adrp and add into x0"));
   }
   let pages = ((((adrp >> 5) & 0x7_FFFF) << 2) | ((adrp >> 29) & 3)).cast_signed() << 11 >> 11;
   let offset = isize::try_from(i64::from(pages) << 12).map_err(|err| err.to_string())?;
   let low = ((add >> 10) & 0xFFF) as usize;
   Ok((at & !0xFFF).wrapping_add_signed(offset) + low)
}
