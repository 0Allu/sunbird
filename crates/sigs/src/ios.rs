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
