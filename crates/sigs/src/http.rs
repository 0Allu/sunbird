//! Build-independent discovery of Desktop Phoenix's `libdiscord/crates/http`
//! `ReqwestTransport` and `SendFuture` vtable slots.
//!
//! Desktop Phoenix routes all Discord REST traffic through a single-method
//! `dyn Transport` trait (`ReqwestTransport::send(&self, req) -> Pin<Box<dyn Future>>`).
//! Swapping the `send` and `poll` vtable slots in `.rdata` intercepts every
//! outgoing request and incoming response without any inline code patching.

use memchr::memchr_iter;
use phoenix_hook::image::Image;

/// Unique error string inside `ReqwestTransport::send` when handed a mobile
/// host-file body.
const HOST_FILE_ANCHOR: &[u8] = b"a host file body needs the host's transport";

/// Located vtable slots and host allocator entry points for Desktop Phoenix's
/// `libdiscord/crates/http` transport.
#[derive(Debug, Clone)]
pub struct DesktopHttpTransport {
   /// All `.rdata` vtable slots holding `ReqwestTransport::send` (may be
   /// duplicated across LLVM CGUs).
   pub send_slots:   Vec<usize>,
   /// Original `ReqwestTransport::send` function address.
   pub send_fn:      usize,
   /// All `.rdata` vtable slots holding `SendFuture::poll`.
   pub poll_slots:   Vec<usize>,
   /// Original `SendFuture::poll` function address.
   pub poll_fn:      usize,
   /// Host's `__rust_alloc(size, align)` if located.
   pub host_alloc:   Option<usize>,
   /// Host's `__rust_dealloc(ptr, size, align)` if located.
   pub host_dealloc: Option<usize>,
}

impl DesktopHttpTransport {
   /// Scans a 64-bit PE `Image` for `ReqwestTransport::send`, its returned
   /// `SendFuture::poll` vtable, and the host's Rust allocator functions.
   #[must_use]
   pub fn locate(image: &Image<'_>) -> Option<Self> {
      if image.pointer_width != 8 || image.pdata.is_none() {
         return None;
      }

      // 1. Locate the unique anchor string inside `ReqwestTransport::send`.
      let anchor = *image.find_bytes(HOST_FILE_ANCHOR).first()?;
      let lea_addr = find_rip_lea_target(image, anchor)?;
      let (send_fn, send_end) = image.function_bounds(lea_addr)?;

      // 2. Find every `dyn Transport` vtable slot in `.rdata` pointing to `send_fn`.
      let send_slots = find_vtable_method_slots(image, send_fn, 1..=0x2000);
      if send_slots.is_empty() {
         return None;
      }

      // 3. Inside `send_fn`, find the small `box_future` helper that boxes the
      //    async state machine and returns `(ptr, poll_vtable)`.
      let (poll_fn, poll_vtable, box_future) = find_poll_vtable(image, send_fn, send_end)?;
      let mut poll_slots = find_vtable_method_slots(image, poll_fn, 0x100..=0x4000);
      let direct_slot = poll_vtable + 24;
      if !poll_slots.contains(&direct_slot) {
         poll_slots.push(direct_slot);
      }

      // 4. Locate `Discord.exe`'s own `__rust_alloc` and `__rust_dealloc` so
      //    response body growth stays 100% allocator-compatible.
      let host_alloc = find_host_alloc(image, box_future);
      let host_dealloc = find_host_dealloc(image, send_fn, send_end);

      Some(Self {
         send_slots,
         send_fn,
         poll_slots,
         poll_fn,
         host_alloc,
         host_dealloc,
      })
   }
}

/// Finds the first x86_64 RIP-relative `LEA r64, [rip + disp32]` in executable
/// segments whose effective address equals `target`.
fn find_rip_lea_target(image: &Image<'_>, target: usize) -> Option<usize> {
   for seg in &image.segments {
      if !seg.executable || seg.bytes.len() < 7 {
         continue;
      }
      let bytes = seg.bytes.as_ref();
      for pos in memchr_iter(0x8D, bytes) {
         if pos == 0 || pos + 6 > bytes.len() {
            continue;
         }
         let rex = bytes[pos - 1];
         if rex != 0x48 && rex != 0x4C {
            continue;
         }
         // ModRM with mod == 0b00 and rm == 0b101 encodes RIP-relative addressing.
         if (bytes[pos + 1] & 0xC7) != 0x05 {
            continue;
         }
         let disp = i32::from_le_bytes(bytes[pos + 2..pos + 6].try_into().ok()?);
         let next_rip = seg.addr + pos + 6;
         if next_rip.checked_add_signed(disp as isize) == Some(target) {
            return Some(seg.addr + pos - 1);
         }
      }
   }
   None
}

/// Finds all `.rdata` vtable method-0 slots (`vtable + 24`) holding `method_fn`
/// where `[drop_fn, size, align, method_fn]` passes Rust trait vtable checks.
fn find_vtable_method_slots(
   image: &Image<'_>,
   method_fn: usize,
   size_range: std::ops::RangeInclusive<usize>,
) -> Vec<usize> {
   let mut slots = Vec::new();
   for slot in image.pointers_to(method_fn) {
      let Some(vtable) = slot.checked_sub(24) else {
         continue;
      };
      let (Some(drop_fn), Some(size), Some(align)) = (
         image.read_usize(vtable),
         image.read_usize(vtable + 8),
         image.read_usize(vtable + 16),
      ) else {
         continue;
      };
      if image.is_function(drop_fn)
         && size_range.contains(&size)
         && align.is_power_of_two()
         && align <= 64
      {
         slots.push(slot);
      }
   }
   slots.sort_unstable();
   slots.dedup();
   slots
}

/// Iterates every direct `call rel32` (`E8`) target inside `[start, end)` that
/// lands on a `.pdata` function start.
fn direct_calls(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   let Some(code) = image.read(start, end.saturating_sub(start)) else {
      return Vec::new();
   };
   let mut targets = Vec::new();
   for pos in memchr_iter(0xE8, code) {
      let Some(raw) = code.get(pos + 1..pos + 5) else {
         continue;
      };
      let disp = i32::from_le_bytes(raw.try_into().unwrap_or([0; 4]));
      let next_rip = start + pos + 5;
      if let Some(callee) = next_rip.checked_add_signed(disp as isize)
         && image.is_function(callee)
      {
         targets.push(callee);
      }
   }
   targets
}

/// Scans small callees of `send_fn` for the `Box::pin` helper that loads the
/// `SendFuture` vtable via `lea rdx, [rip + disp32]` (`48 8D 15`).
fn find_poll_vtable(
   image: &Image<'_>,
   send_fn: usize,
   send_end: usize,
) -> Option<(usize, usize, usize)> {
   for callee in direct_calls(image, send_fn, send_end) {
      let Some((cs, ce)) = image.function_bounds(callee) else {
         continue;
      };
      if ce - cs > 0x80 {
         continue;
      }
      let Some(code) = image.read(cs, ce - cs) else {
         continue;
      };
      for pos in memchr_iter(0x8D, code) {
         if pos == 0 || pos + 6 > code.len() || code[pos - 1] != 0x48 || code[pos + 1] != 0x15 {
            continue;
         }
         let disp = i32::from_le_bytes(code[pos + 2..pos + 6].try_into().ok()?);
         let Some(vtable) = (cs + pos + 6).checked_add_signed(disp as isize) else {
            continue;
         };
         let (Some(drop_fn), Some(size), Some(align), Some(poll_fn)) = (
            image.read_usize(vtable),
            image.read_usize(vtable + 8),
            image.read_usize(vtable + 16),
            image.read_usize(vtable + 24),
         ) else {
            continue;
         };
         if image.is_function(drop_fn)
            && image.is_function(poll_fn)
            && (0x100..=0x4000).contains(&size)
            && align == 8
         {
            return Some((poll_fn, vtable, cs));
         }
      }
   }
   None
}

/// Follows `box_future`'s alloc wrapper to `Discord.exe`'s `__rust_alloc(size, align)`.
fn find_host_alloc(image: &Image<'_>, box_future: usize) -> Option<usize> {
   let (bs, be) = image.function_bounds(box_future)?;
   for wrapper in direct_calls(image, bs, be) {
      let (ws, we) = image.function_bounds(wrapper)?;
      if we - ws > 0x60 {
         continue;
      }
      let code = image.read(ws, we - ws)?;
      // Pattern in alloc wrapper: `mov edx, 8` (`BA 08 00 00 00`), `mov rcx, rsi` (`48 89 F1`),
      // `call __rust_alloc` (`E8 <rel32>`).
      for pos in memchr_iter(0xE8, code) {
         if pos < 5 || pos + 5 > code.len() {
            continue;
         }
         let before = &code[pos.saturating_sub(12)..pos];
         if !before.windows(5).any(|w| w == [0xBA, 0x08, 0x00, 0x00, 0x00]) {
            continue;
         }
         let disp = i32::from_le_bytes(code[pos + 1..pos + 5].try_into().ok()?);
         if let Some(alloc) = (ws + pos + 5).checked_add_signed(disp as isize)
            && image.is_function(alloc)
         {
            return Some(alloc);
         }
      }
   }
   None
}

/// Follows `send_fn` → request builder helper → `Method::drop` (`mov r8d, 1; jmp __rust_dealloc`).
fn find_host_dealloc(image: &Image<'_>, send_fn: usize, send_end: usize) -> Option<usize> {
   // `mov r8d, 1; jmp rel32` = `41 B8 01 00 00 00 E9 <rel32>`
   const TAIL_JUMP: &[u8] = &[0x41, 0xB8, 0x01, 0x00, 0x00, 0x00, 0xE9];

   for callee in direct_calls(image, send_fn, send_end) {
      let Some((cs, ce)) = image.function_bounds(callee) else {
         continue;
      };
      if ce - cs > 0x400 {
         continue;
      }
      for inner in direct_calls(image, cs, ce) {
         let Some((is, ie)) = image.function_bounds(inner) else {
            continue;
         };
         if ie - is > 0x40 {
            continue;
         }
         let Some(code) = image.read(is, ie - is) else {
            continue;
         };
         if let Some(pos) = memchr::memmem::find(code, TAIL_JUMP)
            && let Some(raw) = code.get(pos + TAIL_JUMP.len()..pos + TAIL_JUMP.len() + 4)
         {
            let disp = i32::from_le_bytes(raw.try_into().ok()?);
            if let Some(dealloc) =
               (is + pos + TAIL_JUMP.len() + 4).checked_add_signed(disp as isize)
               && image.is_function(dealloc)
            {
               return Some(dealloc);
            }
         }
      }
   }
   None
}
