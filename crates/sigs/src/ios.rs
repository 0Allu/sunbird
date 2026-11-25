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
/// `vtable[dispatch](data, bytes, len, out)`: `mov x19, x3; ldp x8, x9, [x0];
/// cmp x2, #0; csinc x1, x1, xzr, ne; ldr x9, [x9, #0x20]; mov x0, x8; blr x9`.
const BRIDGE: Pattern = Pattern::new(
   "F3 03 03 AA 08 24 40 A9 5F 00 00 F1 21 14 9F 9A 29 ?? ?? F9 E0 03 08 AA 20 01 3F D6",
);

/// The guest boxing itself for the host, once per constructor: `mov w0, #0x18;
/// bl malloc; cbz x0; ldur q0; str q0, [x0]; ldr x8; str x8, [x0, #0x10];
/// adrp x8, VTABLE; add x8, x8, VTABLE; stp x0, x8, [sp, #8]`.
const GUEST_BOX: Pattern = Pattern::new(
   "00 03 80 52 ?? ?? ?? 94 E0 01 00 B4 E0 83 C0 3C 00 00 80 3D E8 0F 40 F9 08 08 00 F9 ?? ?? ?? \
    ?? ?? ?? ?? 91 E0 A3 00 A9 E0 23 00 91 E1 03 13 AA",
);

/// The guest's trait vtable, which the host calls every view and host event
/// through.
pub struct Bridge {
   pub call:     usize,
   pub vtable:   usize,
   /// Offset of `dispatch` in the vtable, 0x20 on 357.0, after drop, size,
   /// align and one method.
   pub dispatch: usize,
}

pub fn bridge(image: &Image<'_>) -> Found<Bridge> {
   let call = one("guest bridge", &image.scan(BRIDGE))?;
   let load = image
      .read(call + 16, 4)
      .ok_or("guest bridge: the vtable load is unmapped")?;
   let load = u32::from_le_bytes([load[0], load[1], load[2], load[3]]);
   // `ldr x9, [x9, #imm]`, whose 12-bit immediate counts words.
   if load & 0xFFC0_03FF != 0xF940_0129 {
      return Err(format!("guest bridge at {call:#x}: no vtable load into x9"));
   }
   let dispatch = ((load >> 10) & 0xFFF) as usize * 8;

   let boxes = image.scan(GUEST_BOX);
   let vtables = boxes
      .iter()
      .map(|&at| adrp_add(image, at + 28, 8))
      .collect::<Found<Vec<usize>>>()?;
   let vtable = match vtables[..] {
      [first, ref rest @ ..] if rest.iter().all(|&other| other == first) => first,
      _ => {
         return Err(format!(
            "guest vtable: {} boxing sites name {} vtables",
            boxes.len(),
            vtables.len()
         ));
      },
   };
   let entry = image
      .read_usize(vtable + dispatch)
      .ok_or("guest vtable: the dispatch slot is unmapped")?;
   if !image.is_code(entry) {
      return Err(format!(
         "guest vtable at {vtable:#x}: dispatch {entry:#x} is not code"
      ));
   }
   Ok(Bridge {
      call,
      vtable,
      dispatch,
   })
}

/// The address an `adrp xN; add xN, xN, #lo` pair at `at` builds.
fn adrp_add(image: &Image<'_>, at: usize, register: u32) -> Found<usize> {
   let words = image
      .read(at, 8)
      .ok_or_else(|| format!("{at:#x}: the address load is unmapped"))?;
   let adrp = u32::from_le_bytes([words[0], words[1], words[2], words[3]]);
   let add = u32::from_le_bytes([words[4], words[5], words[6], words[7]]);
   // `adrp` and an unshifted 64-bit `add` with the same source and target.
   if adrp & 0x9F00_001F != 0x9000_0000 | register
      || add & 0xFFC0_03FF != 0x9100_0000 | register << 5 | register
   {
      return Err(format!("{at:#x}: no adrp and add into x{register}"));
   }
   let pages = ((((adrp >> 5) & 0x7_FFFF) << 2) | ((adrp >> 29) & 3)).cast_signed() << 11 >> 11;
   let offset = isize::try_from(i64::from(pages) << 12).map_err(|err| err.to_string())?;
   let low = ((add >> 10) & 0xFFF) as usize;
   Ok((at & !0xFFF).wrapping_add_signed(offset) + low)
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
   adrp_add(image, at, 0).map_err(|err| format!("host call: {err}"))
}

/// The two `ClientAccessGate` evaluators the launch gate reads before running
/// Phoenix. Each starts 8 bytes before this body, which loads the gate and
/// walks the experiment table. Both must become `mov w0, #1; ret` to open the
/// gate: patching only one crashes the client, since they share a lock.
///
/// Read on 357.0 (build 124398). The experiment-table reference locates one of
/// them, so the body is the anchor that also catches its twin.
const ACCESS_GATE: Pattern = Pattern::new(
   "09 01 40 F9 3F 09 00 F1 C0 00 00 54 08 A9 47 F9 1F 05 00 B1 60 00 00 54 08 B4 50 39 68 00 00 \
    36",
);

/// The first bytes of a Hermes bytecode bundle like `main.jsbundle`.
pub const HERMES_MAGIC: [u8; 8] = 0x1F19_03C1_03BC_1FC6_u64.to_le_bytes();

/// The `isStaffEnv` getter's prologue in the Hermes bytecode, up to its
/// `LoadConstFalse; Ret` body. Read on Discord 357.0 (1.12.2).
const STAFF_ENV: Pattern =
   Pattern::new("89 01 02 93 00 D5 06 01 00 93 01 34 02 00 3B 02 02 03 45 05 02 00 22 13");

/// The `isStaffEnv` getter in `main.jsbundle`, as a file offset.
pub fn staff_env(bundle: &[u8]) -> Found<usize> {
   one("isStaffEnv", &STAFF_ENV.find_in(bundle))
}

/// The two access-gate evaluators' entry points, which open the gate when both
/// return `Some(true)`.
pub fn access_gate(image: &Image<'_>) -> Found<[usize; 2]> {
   match image.scan(ACCESS_GATE)[..] {
      [a, b] => Ok([a - 8, b - 8]),
      ref hits => {
         Err(format!(
            "access gate: expected 2 evaluators, found {}",
            hits.len()
         ))
      },
   }
}
