//! Where Desktop Phoenix (`Discord.exe`) keeps what the runtime patches.
//!
//! Found from strings, call graphs and instruction shapes, never fixed
//! addresses. The runtime installs from these, and `sigscan check` runs them on
//! a new build before anyone ships against it.

use std::{
   collections::{
      HashMap,
      HashSet,
   },
   sync::{
      Arc,
      Mutex,
      PoisonError,
   },
};

use phoenix_hook::{
   image::Image,
   pattern::Pattern,
};

use crate::{
   Found,
   StaffField,
   one,
};

/// `MessageRow::render` and `build_message_body` prologue: 8 pushes and
/// `mov eax, imm32`, before `call __chkstk`.
pub const ROW_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 B8 ?? ?? ?? ??");
pub const ROW_PROLOGUE_LEN: usize = 17;
/// `host::section`, `WindowRoot::render`, and `Store::delete_message` prologue:
/// 8 pushes and `sub rsp, imm32`.
pub const FRAME_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 48 81 EC ?? ?? ?? ??");
pub const FRAME_PROLOGUE_LEN: usize = 19;
/// `libdiscord_media::fetch` prologue: 6 pushes and `sub rsp, imm32`.
pub const MEDIA_FETCH_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 56 57 53 48 81 EC ?? ?? ?? ??");
pub const MEDIA_FETCH_PROLOGUE_LEN: usize = 15;

/// The 22-byte `.rdata` string naming settings page 24, relabeled in place.
pub const SETTINGS_LABEL: &[u8] = b"PhoenixDESKTOP_PHOENIX";

/// What the runtime fixes at compile time. Every Mana and GPUI struct it builds
/// is decoded from the binary instead, and must fit the scratch buffer it is
/// built in.
pub mod layout {
   /// Bytes reserved for a `gpui::Div`, which is moved by value everywhere.
   pub const DIV_CAP: usize = 0x100;
   /// Bytes reserved for each other Mana struct (`Switch`, `Button`, `Modal`,
   /// `TextInput`, `TextArea`, `MenuItem`).
   pub const SCRATCH_CAP: usize = 0x800;
   /// `gpui::AnyElement`, a boxed element and its vtable.
   pub const ANY_ELEMENT: usize = 0x20;
   /// `gpui::Entity<T>`: id, `Weak` and `TypeId`.
   pub const ENTITY: usize = 0x20;
   /// `MenuItem::Separator`, a variant with no payload.
   pub const MENU_ITEM_SEPARATOR_TAG: u64 = 4;
}

/// Every direct `call rel32` into code, as `callee << 32 | site` RVAs sorted
/// by callee. One pass replaces a `.text` scan per caller lookup.
struct CallIndex {
   key:   (usize, usize),
   base:  usize,
   calls: Vec<u64>,
}

static CALLS: Mutex<Option<Arc<CallIndex>>> = Mutex::new(None);

/// The shared [`CallIndex`] for `image`, built on first use.
fn call_index(image: &Image<'_>) -> Arc<CallIndex> {
   let base = image
      .segments
      .iter()
      .map(|segment| segment.addr)
      .min()
      .unwrap_or(0);
   let code: Vec<(usize, usize)> = image
      .segments
      .iter()
      .filter(|segment| segment.executable)
      .map(|segment| (segment.addr, segment.addr + segment.bytes.len()))
      .collect();
   let key = (base, code.iter().map(|&(start, end)| end - start).sum());
   let mut cached = CALLS.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(index) = cached.as_ref().filter(|index| index.key == key) {
      return Arc::clone(index);
   }
   let in_code = |target: usize| {
      code
         .iter()
         .any(|&(start, end)| (start..end).contains(&target))
   };
   let rva = |addr: usize| u32::try_from(addr - base).ok().map(u64::from);
   let mut calls = Vec::new();
   for segment in image.segments.iter().filter(|segment| segment.executable) {
      for (offset, window) in segment.bytes.windows(5).enumerate() {
         if window[0] != 0xE8 {
            continue;
         }
         let rel = i32::from_le_bytes([window[1], window[2], window[3], window[4]]);
         let site = segment.addr + offset;
         if let Some(callee) = (site + 5).checked_add_signed(rel as isize)
            && in_code(callee)
            && let (Some(callee), Some(site)) = (rva(callee), rva(site))
         {
            calls.push(callee << 32 | site);
         }
      }
   }
   calls.sort_unstable();
   let index = Arc::new(CallIndex { key, base, calls });
   *cached = Some(Arc::clone(&index));
   index
}

/// Frees the shared call index once every locator has run.
pub fn release_call_index() {
   *CALLS.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

impl CallIndex {
   /// The call sites of `callee`.
   fn sites(&self, callee: usize) -> impl Iterator<Item = usize> + '_ {
      let key = callee
         .checked_sub(self.base)
         .and_then(|rva| u32::try_from(rva).ok())
         .map(u64::from);
      let (start, end) = key.map_or((0, 0), |rva| {
         (
            self.calls.partition_point(|&call| call >> 32 < rva),
            self.calls.partition_point(|&call| call >> 32 <= rva),
         )
      });
      self.calls[start..end]
         .iter()
         .map(|&call| self.base + (call & 0xFFFF_FFFF) as usize)
   }

   /// `(site, callee)` for every call whose callee passes `keep`.
   fn matching(&self, keep: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
      self
         .calls
         .iter()
         .map(|&call| {
            (
               self.base + (call & 0xFFFF_FFFF) as usize,
               self.base + (call >> 32) as usize,
            )
         })
         .filter(|&(_, callee)| keep(callee))
         .collect()
   }
}

/// `mov [reg + d], rax; lea rax, [rip + vtable]; mov [reg + d + 8], rax`,
/// storing an `Rc<dyn Fn>`, and the `mov ecx, size` of the return copy that
/// may follow within 16 bytes.
fn rc_store(code: &[u8]) -> Option<(usize, Option<usize>)> {
   let mem = |at: usize| -> Option<(u8, usize, usize)> {
      let (rex, op, modrm) = (*code.get(at)?, *code.get(at + 1)?, *code.get(at + 2)?);
      if rex != 0x48 || op != 0x89 || modrm & 0x38 != 0 || modrm & 7 == 4 {
         return None;
      }
      match modrm >> 6 {
         1 => Some((modrm, usize::from(*code.get(at + 3)?), 4)),
         2 => {
            let disp = code.get(at + 3..at + 7)?;
            Some((modrm, u32::from_le_bytes(disp.try_into().ok()?) as usize, 7))
         },
         _ => None,
      }
   };
   (0..code.len()).find_map(|at| {
      let (modrm, field, len) = mem(at)?;
      let lea = at + len;
      if code.get(lea..lea + 3)? != [0x48, 0x8D, 0x05] {
         return None;
      }
      let (modrm2, next, len2) = mem(lea + 7)?;
      if modrm2 != modrm || next != field + 8 {
         return None;
      }
      let tail = code.get(lea + 7 + len2..).unwrap_or_default();
      let tail = &tail[..tail.len().min(16)];
      let copy = tail
         .windows(5)
         .find(|w| w[0] == 0xB9)
         .filter(|_| tail.windows(2).any(|w| w == [0xF3, 0xA4]))
         .map(|w| u32::from_le_bytes([w[1], w[2], w[3], w[4]]) as usize);
      Some((field, copy))
   })
}

/// The field a `SharedString` setter replaces: `movzx eax, byte [reg + d];
/// cmp al, 0xff; ...; cmp al, 0x19`, the drop of the old value.
#[cfg(target_arch = "x86_64")]
fn shared_string_setter(code: &[u8]) -> Option<usize> {
   (0..code.len()).find_map(|at| {
      let w = code.get(at..at + 9)?;
      if w[0..2] != [0x0F, 0xB6] || w[2] & 0xF8 != 0x80 || w[2] == 0x84 || w[7..9] != [0x3C, 0xFF] {
         return None;
      }
      let after = code.get(at + 9..(at + 21).min(code.len()))?;
      after
         .windows(2)
         .any(|w| w == [0x3C, 0x19])
         .then(|| u32::from_le_bytes([w[3], w[4], w[5], w[6]]) as usize)
   })
}

/// Offsets of `mov byte [reg + d8], 0xff` stores, a `None` written into an
/// `Option<SharedString>`.
#[cfg(target_arch = "x86_64")]
fn none_stores(code: &[u8]) -> Vec<usize> {
   code
      .windows(4)
      .filter(|w| w[0] == 0xC6 && w[1] & 0xF8 == 0x40 && w[1] != 0x44 && w[3] == 0xFF)
      .map(|w| usize::from(w[2]))
      .collect()
}

/// The 32-bit immediate after the first `prefix`.
fn imm_after(code: &[u8], prefix: &[u8]) -> Option<usize> {
   let at = code.windows(prefix.len()).position(|w| w == prefix)? + prefix.len();
   let imm = code.get(at..at + 4)?;
   Some(u32::from_le_bytes(imm.try_into().ok()?) as usize)
}

fn function_code<'a>(image: &'a Image<'_>, func: usize, name: &str) -> Found<&'a [u8]> {
   let (start, end) = image
      .function_bounds(func)
      .ok_or_else(|| format!("{name} has no .pdata entry"))?;
   image
      .read(start, end - start)
      .ok_or_else(|| format!("{name} is unmapped"))
}

fn fits(name: &str, size: usize, cap: usize) -> Found<usize> {
   if size == 0 || size > cap {
      return Err(format!(
         "{name} is {size:#x} bytes, the runtime reserves {cap:#x}"
      ));
   }
   Ok(size)
}

fn imm32(image: &Image<'_>, at: usize) -> Option<usize> {
   image.read_u32(at).map(|value| value as usize)
}

fn rel32_target(image: &Image<'_>, field: usize) -> Option<usize> {
   let rel = image.read_u32(field)?.cast_signed();
   (field + 4).checked_add_signed(rel as isize)
}

/// Starts of the `lea reg, [rip + target]` instructions.
fn lea_refs(image: &Image<'_>, target: usize) -> Vec<usize> {
   lea_refs_many(image, &[target])
      .remove(&target)
      .unwrap_or_default()
}

/// [`lea_refs`] for every target in one pass over `.text`. Each pass is about
/// a second on `Discord.exe`, so locators batch their string loads.
fn lea_refs_many(image: &Image<'_>, targets: &[usize]) -> HashMap<usize, Vec<usize>> {
   let mut sorted = targets.to_vec();
   sorted.sort_unstable();
   sorted.dedup();
   let mut found: HashMap<usize, Vec<usize>> = HashMap::new();
   let (Some(&low), Some(&high)) = (sorted.first(), sorted.last()) else {
      return found;
   };
   for segment in image.segments.iter().filter(|segment| segment.executable) {
      let bytes = &segment.bytes;
      for (offset, window) in bytes.windows(7).enumerate() {
         if window[1] != 0x8D || window[0] & 0xF0 != 0x40 || window[2] & 0xC7 != 0x05 {
            continue;
         }
         let rel = i32::from_le_bytes([window[3], window[4], window[5], window[6]]);
         let start = segment.addr + offset;
         if let Some(target) = (start + 7).checked_add_signed(rel as isize)
            && (low..=high).contains(&target)
            && sorted.binary_search(&target).is_ok()
         {
            found.entry(target).or_default().push(start);
         }
      }
   }
   found
}

/// `(site, callee)` for every `call rel32` whose callee passes `keep`.
fn call_sites(image: &Image<'_>, keep: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
   call_index(image).matching(keep)
}

/// The functions that call `callee`.
fn callers(image: &Image<'_>, callee: usize) -> HashSet<usize> {
   call_index(image)
      .sites(callee)
      .filter_map(|site| Some(image.function_bounds(site)?.0))
      .collect()
}

/// Direct calls in `[start, start + len)` to what looks like a function
/// entry, in order.
fn calls_in(image: &Image<'_>, start: usize, len: usize) -> Vec<(usize, usize)> {
   let Some(bytes) = image.read(start, len) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   let mut idx = 0;
   while idx + 5 <= bytes.len() {
      if bytes[idx] == 0xE8 {
         let site = start + idx;
         let rel = i32::from_le_bytes([
            bytes[idx + 1],
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
         ]);
         let target = (site + 5).wrapping_add_signed(rel as isize);
         let entry = image.is_function(target)
            || image.is_code(target)
               && image.read(target, 1).is_some_and(|first| {
                  matches!(
                     first[0],
                     0xE9
                        | 0x48
                        | 0x4C
                        | 0x40
                        | 0x41
                        | 0x55
                        | 0x53
                        | 0x56
                        | 0x57
                        | 0x89
                        | 0x8B
                        | 0xB8
                        | 0x31
                        | 0x33
                  )
               });
         if entry {
            out.push((site, target));
            idx += 5;
            continue;
         }
      }
      idx += 1;
   }
   out
}

/// The distinct code targets of direct calls in `[start, end)`.
fn callees(image: &Image<'_>, start: usize, end: usize) -> Vec<usize> {
   let Some(bytes) = image.read(start, end.saturating_sub(start)) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   for (idx, window) in bytes.windows(5).enumerate() {
      if window[0] != 0xE8 {
         continue;
      }
      let rel = i32::from_le_bytes([window[1], window[2], window[3], window[4]]);
      let target = (start + idx + 5).wrapping_add_signed(rel as isize);
      if image.is_code(target) && !out.contains(&target) {
         out.push(target);
      }
   }
   out
}

fn prologue_at(image: &Image<'_>, func: usize, prologue: Pattern, len: usize) -> bool {
   image
      .read(func, len)
      .is_some_and(|head| prologue.find_in(head).first() == Some(&0))
}

/// The function `ClientAccessGate` reads through, which the runtime makes
/// answer "no gate". Its one small caller is `signed_in && access(..) != 2`.
pub fn access_gate(image: &Image<'_>) -> Found<usize> {
   const CALLER: Pattern =
      Pattern::new("83 79 ?? 03 75 0C E8 ?? ?? ?? ?? 3C 02 0F 95 C0 EB 02 31 C0");
   let caller = one("access gate caller", &image.scan(CALLER))?;
   let access = rel32_target(image, caller + 7).ok_or("access gate call does not decode")?;
   if image.is_code(access) {
      Ok(access)
   } else {
      Err(format!(
         "access gate call at {:#x} leaves the code",
         caller + 6
      ))
   }
}

/// The settings page 24 label.
pub fn settings_label(image: &Image<'_>) -> Found<usize> {
   one("settings label", &image.find_bytes(SETTINGS_LABEL))
}

/// The `cmp r8d, 0xf` immediate in `is_page_visible`, which the runtime
/// lowers so page 24 is always shown.
pub fn settings_visibility(image: &Image<'_>) -> Found<usize> {
   /// `movzx ecx, cl; mov al, 1; lea r8d, [rcx-9]; cmp r8d, 0xf; ja ...`
   const GUARD: Pattern = Pattern::new("0F B6 C9 B0 01 44 8D 41 F7 41 83 F8 0F 77");
   Ok(one("settings visibility guard", &image.scan(GUARD))? + 12)
}

/// `host::section`, which renders a settings page, and where its state keeps
/// the active page.
pub struct Section {
   pub func:        usize,
   pub page_offset: usize,
}

pub fn settings_section(image: &Image<'_>) -> Found<Section> {
   /// `mov bl, [r15 + disp32]`, loading `self.active_page`.
   const ACTIVE_PAGE_LOAD: Pattern = Pattern::new("41 8A 9F ?? ?? ?? ??");
   let anchor = one("account-loading", &image.find_bytes(b"account-loading"))?;
   let lea = one("account-loading load", &lea_refs(image, anchor))?;
   let (func, _) = image
      .function_bounds(lea)
      .ok_or("settings section has no .pdata entry")?;
   if !prologue_at(image, func, FRAME_PROLOGUE, FRAME_PROLOGUE_LEN) {
      return Err("settings section prologue changed".to_owned());
   }
   let head = image
      .read(func, 0x160)
      .ok_or("settings section is unmapped")?;
   let page_offset = ACTIVE_PAGE_LOAD
      .find_in(head)
      .into_iter()
      .filter_map(|at| imm32(image, func + at + 3))
      .find(|disp| (0x100..=0x2000).contains(disp))
      .ok_or("settings section active page load not found")?;
   Ok(Section { func, page_offset })
}

/// Discord's own GPUI and Mana builders the settings page calls.
pub struct Gpui {
   /// The `Location` `div_new` takes, a static in `.rdata`.
   pub caller_loc:       usize,
   pub div_new:          usize,
   pub div_flex:         usize,
   pub div_flex_col:     usize,
   pub div_style_mut:    usize,
   pub div_child_div:    usize,
   pub div_child_button: usize,
   pub div_into_any:     usize,
   pub div_push_any:     Option<usize>,
   pub str_into_any:     Option<usize>,
   pub text_block:       usize,
   pub switch_new:       usize,
   pub switch_into_div:  usize,
   pub button_new:       usize,
   pub action_row:       usize,
   pub alloc_or_oom:     usize,
   /// Discord's `Switch` label, description and `on_change` setters, and a
   /// `Button::on_click`, which the layouts are read from.
   pub switch_label:     usize,
   pub switch_desc:      usize,
   pub switch_on_change: usize,
   pub button_on_click:  usize,
}

/// The builders, from the Developer Mode row of page 16 and the
/// `TesterUpdates` sign-in button, which call them in a fixed order.
pub fn gpui(image: &Image<'_>) -> Found<Gpui> {
   let dev = one(
      "set-devmode",
      &image.find_bytes(b"set-devmodeDeveloper Mode"),
   )?;
   let lea_dev = one("set-devmode load", &lea_refs(image, dev))?;
   let before_dev = calls_in(image, lea_dev - 0x70, 0x70);
   let after_dev = calls_in(image, lea_dev, 0x240);
   if before_dev.len() < 3 || after_dev.len() < 9 {
      return Err(format!(
         "page 16 call counts changed ({}, {})",
         before_dev.len(),
         after_dev.len()
      ));
   }
   let (div_new_call, div_new) = before_dev[0];
   let caller_loc = rdx_lea_before(image, div_new_call).ok_or("div_new Location not found")?;
   let switch_on_change = after_dev[7].1;
   let alloc_or_oom = calls_in(image, switch_on_change, 0x50)
      .first()
      .ok_or("alloc_or_oom not found in Switch::on_change")?
      .1;

   let btn = one(
      "tester-updates-sign-in",
      &image.find_bytes(b"tester-updates-sign-in\0"),
   )?;
   let lea_btn = one("tester-updates-sign-in load", &lea_refs(image, btn))?;
   let after_btn = calls_in(image, lea_btn, 0x1D0);
   if after_btn.len() < 6 {
      return Err(format!(
         "TesterUpdates call count changed ({})",
         after_btn.len()
      ));
   }
   let action_row = after_btn[4].1;
   let (ar_start, ar_end) = image
      .function_bounds(action_row)
      .ok_or("ActionRow has no .pdata entry")?;
   let ar_calls = calls_in(image, ar_start, ar_end - ar_start);
   if ar_calls.len() < 16 || ar_calls[0].1 != div_new || ar_calls[2].1 != before_dev[1].1 {
      return Err("ActionRow no longer calls div_new and flex first".to_owned());
   }
   let div_style_mut = calls_in(image, ar_calls[1].1, 0x40)
      .first()
      .ok_or("div_style_mut not found in div_py")?
      .1;
   let div_child_div = ar_calls[ar_calls.len() - 1].1;

   // `SmallVec::<[AnyElement; 2]>::push`, after `lea rcx, [rax + 0x40]`.
   let child_calls = calls_in(image, div_child_div, 0x60);
   let div_push_any = (child_calls.len() >= 2
      && image
         .read(div_child_div, 0x50)
         .is_some_and(|code| code.windows(4).any(|w| w == [0x48, 0x8D, 0x48, 0x40])))
   .then(|| child_calls[1].1);
   // `SharedString::into_any_element`, beside `div_child_div`, checks the
   // `SharedString` tag with `cmp cl, 0xff; ...; cmp cl, 0x19`.
   let str_into_any = calls_in(image, div_child_div, 0x400)
      .into_iter()
      .map(|(_, target)| target)
      .find(|&target| {
         image.read(target, 0x100).is_some_and(|body| {
            body
               .windows(8)
               .any(|w| w[0..3] == [0x80, 0xF9, 0xFF] && w[5..8] == [0x80, 0xF9, 0x19])
         })
      });

   Ok(Gpui {
      caller_loc,
      div_new,
      div_flex: before_dev[1].1,
      div_flex_col: before_dev[2].1,
      div_style_mut,
      div_child_div,
      div_child_button: ar_calls[14].1,
      div_into_any: after_btn[5].1,
      div_push_any,
      str_into_any,
      text_block: ar_calls[6].1,
      switch_new: after_dev[0].1,
      switch_into_div: after_dev[8].1,
      button_new: after_btn[0].1,
      action_row,
      alloc_or_oom,
      switch_label: after_dev[2].1,
      switch_desc: after_dev[5].1,
      switch_on_change,
      button_on_click: after_btn[3].1,
   })
}

/// The target of the last `lea rdx, [rip + disp32]` in the 32 bytes before
/// `call`.
fn rdx_lea_before(image: &Image<'_>, call: usize) -> Option<usize> {
   let start = call.checked_sub(0x20)?;
   let bytes = image.read(start, 0x20)?;
   (0..=bytes.len() - 7)
      .rev()
      .find(|&idx| bytes[idx..idx + 3] == [0x48, 0x8D, 0x15])
      .and_then(|idx| rel32_target(image, start + idx + 3))
}

/// `StyleRefinement` field offsets, read by name from its derived `Debug`.
/// Edges are `[top, right, bottom, left]`, corners `[top_left, top_right,
/// bottom_right, bottom_left]` and sizes `[width, height]`.
#[derive(Debug, Clone, Copy)]
pub struct StyleOffsets {
   /// `StyleRefinement` bytes, from `Interactivity::default`'s box.
   pub bytes:           usize,
   pub font_size:       usize,
   pub line_height:     usize,
   pub font_family:     usize,
   pub text_color:      usize,
   pub white_space:     usize,
   pub flex_shrink:     usize,
   pub background:      usize,
   pub border_color:    usize,
   pub overflow_x:      usize,
   pub position:        usize,
   pub flex_wrap:       usize,
   pub display:         usize,
   pub align_items:     usize,
   pub justify_content: usize,
   pub border_widths:   [usize; 4],
   pub corner_radii:    [usize; 4],
   pub padding:         [usize; 4],
   pub inset:           [usize; 4],
   pub gap:             [usize; 2],
   pub size:            [usize; 2],
   pub min_size:        [usize; 2],
}

/// `gpui::Div`: its size, `SmallVec<[AnyElement; 2]>` children, and boxed
/// `Interactivity`.
#[derive(Debug, Clone, Copy)]
pub struct DivLayout {
   pub size:               usize,
   pub children:           usize,
   pub interactivity:      usize,
   pub interactivity_size: usize,
}

/// `mana_gpui::components::switch::Switch`.
#[derive(Debug, Clone, Copy)]
pub struct SwitchLayout {
   pub size:        usize,
   pub label:       usize,
   pub description: usize,
   pub on_change:   usize,
}

/// `mana_gpui::components::button::Button`.
#[derive(Debug, Clone, Copy)]
pub struct ButtonLayout {
   pub size:     usize,
   pub on_click: usize,
   pub variant:  usize,
   /// The `ButtonSize` field.
   pub scale:    usize,
}

/// What the settings page and accessories build by hand.
#[derive(Debug, Clone, Copy)]
pub struct GpuiLayout {
   pub style:  StyleOffsets,
   pub div:    DivLayout,
   pub switch: SwitchLayout,
   pub button: ButtonLayout,
}

/// Field name to offset in a derived `Debug::fmt`, with nested structs as
/// `outer.inner`, following each field's `&dyn Debug` vtable `depth` deep.
#[cfg(target_arch = "x86_64")]
fn debug_offsets(image: &Image<'_>, func: usize, depth: usize) -> HashMap<String, usize> {
   let mut out = HashMap::new();
   let Some(code) = image
      .function_bounds(func)
      .and_then(|(start, end)| image.read(start, end - start))
   else {
      return out;
   };
   for field in phoenix_hook::x86::debug_fields(code, func) {
      let Some(name) = image
         .read(field.name, field.name_len)
         .and_then(|name| std::str::from_utf8(name).ok())
      else {
         continue;
      };
      if out.contains_key(name) {
         continue;
      }
      out.insert(name.to_owned(), field.offset);
      let fmt = field
         .vtable
         .and_then(|vtable| image.read_usize(vtable + 0x18));
      if depth > 0
         && let Some(fmt) = fmt.filter(|&fmt| image.is_code(fmt))
      {
         for (inner, offset) in debug_offsets(image, fmt, depth - 1) {
            out.entry(format!("{name}.{inner}"))
               .or_insert(field.offset + offset);
         }
      }
   }
   out
}

/// `<StyleRefinement as Debug>::fmt`, the one loader of a `StyleRefinement`
/// literal that prints `text`, `padding` and `gap`.
#[cfg(target_arch = "x86_64")]
#[expect(clippy::too_many_lines, reason = "one line per decoded field")]
fn style_offsets(image: &Image<'_>, bytes: usize) -> Found<StyleOffsets> {
   let leas = string_leas(image, &[b"StyleRefinement"]);
   let mut funcs: Vec<usize> = leas
      .values()
      .flatten()
      .filter_map(|&lea| Some(image.function_bounds(lea)?.0))
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let found: Vec<HashMap<String, usize>> = funcs
      .into_iter()
      .map(|func| debug_offsets(image, func, 1))
      .filter(|fields| {
         ["text", "padding", "gap"]
            .iter()
            .all(|field| fields.contains_key(*field))
      })
      .collect();
   let [fields] = &found[..] else {
      return Err(format!(
         "StyleRefinement Debug matched {} functions",
         found.len()
      ));
   };
   let get = |name: &str| -> Found<usize> {
      fields
         .get(name)
         .copied()
         .ok_or_else(|| format!("StyleRefinement Debug has no {name}"))
   };
   let edges = |name: &str| -> Found<[usize; 4]> {
      Ok([
         get(&format!("{name}.top"))?,
         get(&format!("{name}.right"))?,
         get(&format!("{name}.bottom"))?,
         get(&format!("{name}.left"))?,
      ])
   };
   let axes = |name: &str| -> Found<[usize; 2]> {
      Ok([
         get(&format!("{name}.width"))?,
         get(&format!("{name}.height"))?,
      ])
   };
   let style = StyleOffsets {
      bytes,
      font_size: get("text.font_size")?,
      line_height: get("text.line_height")?,
      font_family: get("text.font_family")?,
      text_color: get("text.color")?,
      white_space: get("text.white_space")?,
      flex_shrink: get("flex_shrink")?,
      background: get("background")?,
      border_color: get("border_color")?,
      overflow_x: get("overflow.x")?,
      position: get("position")?,
      flex_wrap: get("flex_wrap")?,
      display: get("display")?,
      align_items: get("align_items")?,
      justify_content: get("justify_content")?,
      border_widths: edges("border_widths")?,
      corner_radii: [
         get("corner_radii.top_left")?,
         get("corner_radii.top_right")?,
         get("corner_radii.bottom_right")?,
         get("corner_radii.bottom_left")?,
      ],
      padding: edges("padding")?,
      inset: edges("inset")?,
      gap: axes("gap")?,
      size: axes("size")?,
      min_size: axes("min_size")?,
   };
   // The widest value the runtime writes into each field: `Option<Hsla>` is
   // 20 bytes, `Option<Background>` 0x48, a `SharedString` 24, a length 8.
   let single = [
      (style.font_size, 8),
      (style.line_height, 8),
      (style.font_family, 24),
      (style.text_color, 20),
      (style.white_space, 1),
      (style.flex_shrink, 8),
      (style.background, 0x48),
      (style.border_color, 20),
      (style.overflow_x, 1),
      (style.position, 1),
      (style.flex_wrap, 1),
      (style.display, 1),
      (style.align_items, 1),
      (style.justify_content, 1),
   ];
   let lengths = [
      &style.border_widths[..],
      &style.corner_radii,
      &style.padding,
      &style.inset,
      &style.gap,
      &style.size,
      &style.min_size,
   ]
   .into_iter()
   .flatten()
   .map(|&at| (at, 8));
   if let Some(end) = single
      .into_iter()
      .chain(lengths)
      .map(|(at, len)| at + len)
      .max()
      && end > bytes
   {
      return Err(format!(
         "StyleRefinement is {bytes:#x} bytes, its fields reach {end:#x}"
      ));
   }
   Ok(style)
}

/// `Div::new` boxes `Interactivity` (`mov ecx, size; mov edx, 8; call`),
/// copies it in (`mov r8d, size; mov rcx, reg; call memcpy`) and stores the box
/// `reg` into the `Div` (`mov [sret + d8], reg`).
#[cfg(target_arch = "x86_64")]
fn div_interactivity(image: &Image<'_>, div_new: usize) -> Found<(usize, usize)> {
   let code = image.read(div_new, 0x180).ok_or("Div::new is unmapped")?;
   let size = code
      .windows(10)
      .find(|w| w[0] == 0xB9 && w[5..10] == [0xBA, 0x08, 0x00, 0x00, 0x00])
      .map(|w| u32::from_le_bytes([w[1], w[2], w[3], w[4]]) as usize)
      .filter(|size| (0x100..0x2000).contains(size))
      .ok_or("Div::new Interactivity allocation not found")?;
   let size_le = u32::try_from(size)
      .map_err(|_| "Div::new Interactivity size overflows")?
      .to_le_bytes();
   let copy = code
      .windows(6)
      .position(|w| w[..2] == [0x41, 0xB8] && w[2..] == size_le)
      .ok_or("Div::new Interactivity copy not found")?;
   // `mov rcx, reg` (`48 89 /r`, ModRM `11 reg 001`) right before the call.
   let call = copy
      + 6
      + code[copy + 6..]
         .windows(4)
         .take(0x20)
         .position(|w| w[0] == 0x48 && w[1] == 0x89 && w[2] & 0xC7 == 0xC1 && w[3] == 0xE8)
         .ok_or("Div::new memcpy destination not found")?;
   let reg = code[call + 2] & 0x38;
   let offset = code[call + 8..]
      .windows(4)
      .find(|w| w[0] == 0x48 && w[1] == 0x89 && w[2] & 0xF8 == 0x40 | reg && w[2] & 7 != 4)
      .map(|w| usize::from(w[3]))
      .ok_or("Div::new Interactivity store not found")?;
   Ok((offset, size))
}

/// The two in-struct fields Discord most often sets right after
/// `Button::new`, `variant` and then `size`, over every call site.
#[cfg(target_arch = "x86_64")]
fn button_style_fields(image: &Image<'_>, button_new: usize, size: usize) -> Found<(usize, usize)> {
   let ranked = post_ctor_byte_stores(image, button_new, size);
   // 189, 20 and 4 stores on 2026-11 builds.
   match ranked[..] {
      [(top, variant), (second, scale), ref rest @ ..]
         if top >= 3 * second && rest.first().is_none_or(|&(third, _)| second >= 3 * third) =>
      {
         Ok((variant, scale))
      },
      _ => {
         Err(format!(
            "Button variant and size stores are ambiguous ({:x?})",
            &ranked[..ranked.len().min(4)]
         ))
      },
   }
}

/// Decodes the [`GpuiLayout`] from the builders.
///
/// Each constructor must still set the fields the runtime overwrites and fit
/// its buffer. A layout that does not decode turns the settings page and
/// accessories off.
#[cfg(target_arch = "x86_64")]
#[expect(clippy::too_many_lines, reason = "one block per decoded struct")]
pub fn gpui_layouts(image: &Image<'_>, gpui: &Gpui) -> Found<GpuiLayout> {
   let check = |name: &str, func: usize, size: usize, fields: &[usize]| -> Found<()> {
      let end = image
         .function_bounds(func)
         .map_or(func + 0x800, |(_, end)| end);
      let code = image
         .read(func, end - func)
         .ok_or_else(|| format!("{name} is unmapped"))?;
      let stores = phoenix_hook::x86::sret_stores(code, func);
      let extent = stores.iter().map(|&(at, len)| at + len).max().unwrap_or(0);
      if extent == 0 || extent > size {
         return Err(format!("{name} fills {extent:#x} bytes of {size:#x}"));
      }
      for field in fields {
         if !stores.iter().any(|&(at, _)| at == *field) {
            return Err(format!("{name} no longer sets +{field:#x}"));
         }
      }
      Ok(())
   };

   // `div_child_div`: `lea rcx, [div + children]; ...; call push`, then the
   // return copy `mov r8d, size; call memcpy`.
   let child = image
      .read(gpui.div_child_div, 0x80)
      .ok_or("div_child_div is unmapped")?;
   let children = child
      .windows(4)
      .find(|w| w[..3] == [0x48, 0x8D, 0x48])
      .map(|w| usize::from(w[3]))
      .ok_or("Div children SmallVec offset not found")?;
   let div_size = fits(
      "Div",
      imm_after(child, &[0x41, 0xB8]).ok_or("Div size not found")?,
      layout::DIV_CAP,
   )?;
   let (interactivity, interactivity_size) = div_interactivity(image, gpui.div_new)?;
   if children + 0x48 > div_size || interactivity + 8 > div_size {
      return Err("Div children or Interactivity sit past the Div".to_owned());
   }
   check("Div::new", gpui.div_new, div_size, &[interactivity])?;
   let div = DivLayout {
      size: div_size,
      children,
      interactivity,
      interactivity_size,
   };

   let (on_change, switch_size) = rc_store(function_code(
      image,
      gpui.switch_on_change,
      "Switch::on_change",
   )?)
   .ok_or("Switch::on_change Rc store not found")?;
   let switch_size = fits(
      "Switch",
      switch_size.ok_or("Switch::on_change return copy not found")?,
      layout::SCRATCH_CAP,
   )?;
   let label = shared_string_setter(function_code(image, gpui.switch_label, "Switch::label")?)
      .ok_or("Switch label setter not found")?;
   let description = shared_string_setter(function_code(
      image,
      gpui.switch_desc,
      "Switch::description",
   )?)
   .ok_or("Switch description setter not found")?;
   if label.max(description) + 24 > switch_size || on_change + 16 > switch_size {
      return Err("Switch fields sit past the Switch".to_owned());
   }
   check("Switch::new", gpui.switch_new, switch_size, &[on_change])?;
   let switch = SwitchLayout {
      size: switch_size,
      label,
      description,
      on_change,
   };

   let (on_click, button_size) = rc_store(function_code(
      image,
      gpui.button_on_click,
      "Button::on_click",
   )?)
   .ok_or("Button::on_click Rc store not found")?;
   let button_size = fits(
      "Button",
      button_size.ok_or("Button::on_click return copy not found")?,
      layout::SCRATCH_CAP,
   )?;
   if on_click + 16 > button_size {
      return Err("Button on_click sits past the Button".to_owned());
   }
   let (variant, scale) = button_style_fields(image, gpui.button_new, button_size)?;
   check("Button::new", gpui.button_new, button_size, &[
      on_click, variant, scale,
   ])?;
   let button = ButtonLayout {
      size: button_size,
      on_click,
      variant,
      scale,
   };

   // `Interactivity::default` boxes `StyleRefinement` with `mov ecx, size; mov
   // edx, 8; call`.
   let box_alloc = |func: usize, limit: usize| -> Option<usize> {
      let code = image.read(func, limit)?;
      code
         .windows(10)
         .find(|w| w[0] == 0xB9 && w[5..10] == [0xBA, 0x08, 0x00, 0x00, 0x00])
         .map(|w| u32::from_le_bytes([w[1], w[2], w[3], w[4]]) as usize)
   };
   let (_, div_end) = image
      .function_bounds(gpui.div_new)
      .ok_or("Div::new has no .pdata entry")?;
   let style_bytes = callees(image, gpui.div_new, div_end)
      .into_iter()
      .filter_map(|callee| box_alloc(callee, 0xC0))
      .find(|&size| (0x200..=0x400).contains(&size))
      .ok_or("Interactivity::default StyleRefinement allocation not found")?;
   let style = style_offsets(image, style_bytes)?;

   Ok(GpuiLayout {
      style,
      div,
      switch,
      button,
   })
}

/// `MessageRow::render`, and where its message keeps the id.
pub struct MessageRow {
   pub func:          usize,
   pub msg_id_offset: usize,
}

/// The one function calling every helper that formats `message-content-{id}`.
pub fn message_row(image: &Image<'_>) -> Found<MessageRow> {
   /// `lea rax, [rcx + disp32]`, the id the template formats.
   const MSG_ID_LEA: Pattern = Pattern::new("48 8D 81 ?? ?? ?? ??");
   let anchor = one(
      "message-content template",
      &image.find_bytes(b"\x10message-content-\xc0\x00"),
   )?;
   let leas = lea_refs(image, anchor);
   if leas.is_empty() {
      return Err("nothing loads the message-content template".to_owned());
   }
   let msg_id_offset = leas
      .iter()
      .find_map(|&lea| {
         let start = lea.saturating_sub(0x60);
         let window = image.read(start, lea - start)?;
         MSG_ID_LEA
            .find_in(window)
            .into_iter()
            .rev()
            .filter_map(|at| imm32(image, start + at + 3))
            .find(|disp| (0x100..=0x2000).contains(disp))
      })
      .ok_or("message id load not found near the template")?;
   let helpers = leas
      .iter()
      .filter_map(|&lea| Some(image.function_bounds(lea)?.0))
      .collect::<HashSet<_>>();
   let common = helpers
      .iter()
      .map(|&helper| callers(image, helper))
      .filter(|parents| !parents.is_empty())
      .reduce(|common, parents| common.intersection(&parents).copied().collect())
      .unwrap_or_default();
   let func = one(
      "MessageRow::render",
      &common.into_iter().collect::<Vec<_>>(),
   )?;
   if !prologue_at(image, func, ROW_PROLOGUE, ROW_PROLOGUE_LEN) {
      return Err("MessageRow::render prologue changed".to_owned());
   }
   Ok(MessageRow {
      func,
      msg_id_offset,
   })
}

/// `build_message_body`, and where its message and row colors sit.
pub struct MessageBody {
   pub func:              usize,
   pub msg_id_offset:     usize,
   pub row_colors_offset: usize,
}

/// The function `MessageRow::render` calls that loads the `message-edited-`
/// template.
pub fn message_body(image: &Image<'_>, render_row: usize) -> Found<MessageBody> {
   /// `mov rcx, [rcx + disp32]`, loading the message id.
   const MSG_ID_MOV: Pattern = Pattern::new("48 8B 89 ?? ?? ?? ??");
   /// `mov eax, imm32; mov ecx, imm32; mov edx, imm32` with consecutive
   /// `RowColors` field offsets `+0xc`, `+0x8` and `+0x4`.
   const ROW_COLORS: Pattern = Pattern::new("B8 ?? ?? ?? ?? B9 ?? ?? ?? ?? BA ?? ?? ?? ??");
   let anchor = one(
      "message-edited template",
      &image.find_bytes(b"\x0fmessage-edited-\xc0\x00"),
   )?;
   let mut found = lea_refs(image, anchor)
      .into_iter()
      .filter_map(|lea| Some(image.function_bounds(lea)?.0))
      .filter(|&func| prologue_at(image, func, ROW_PROLOGUE, ROW_PROLOGUE_LEN))
      .filter(|&func| callers(image, func).contains(&render_row))
      .collect::<Vec<_>>();
   found.sort_unstable();
   found.dedup();
   let func = one("message body builder", &found)?;
   let head = image
      .read(func, 0x120)
      .ok_or("message body builder is unmapped")?;
   let msg_id_offset = MSG_ID_MOV
      .find_in(&head[..0x100])
      .into_iter()
      .filter_map(|at| imm32(image, func + at + 3))
      .find(|disp| (0x100..=0x800).contains(disp))
      .ok_or("message body id load not found")?;
   let row_colors_offset = ROW_COLORS
      .find_in(head)
      .into_iter()
      .find_map(|at| {
         let (a, c, d) = (
            imm32(image, func + at + 1)?,
            imm32(image, func + at + 6)?,
            imm32(image, func + at + 11)?,
         );
         (a == c + 4 && c == d + 4 && (0x100..=0x400).contains(&d)).then(|| d - 4)
      })
      .ok_or("message body row colors not found")?;
   Ok(MessageBody {
      func,
      msg_id_offset,
      row_colors_offset,
   })
}

/// `Store::delete_message`, the client's optimistic delete. It shares the map
/// and index deletes with the `MESSAGE_DELETE` handler and takes a lock.
pub fn optimistic_delete(image: &Image<'_>, message_delete: usize) -> Found<usize> {
   let (del_start, del_end) = image
      .function_bounds(message_delete)
      .ok_or("MESSAGE_DELETE handler has no .pdata entry")?;
   let del_callees = callees(image, del_start, del_end)
      .into_iter()
      .collect::<HashSet<_>>();
   let mut parents: HashMap<usize, HashSet<usize>> = HashMap::new();
   for (site, callee) in call_sites(image, |callee| del_callees.contains(&callee)) {
      if let Some((parent, _)) = image.function_bounds(site) {
         parents.entry(callee).or_default().insert(parent);
      }
   }
   let mut found = Vec::new();
   for parents in parents.values() {
      if parents.len() != 2 || !parents.contains(&del_start) {
         continue;
      }
      let Some(&other) = parents.iter().find(|&&parent| parent != del_start) else {
         continue;
      };
      if found.contains(&other) || !prologue_at(image, other, FRAME_PROLOGUE, FRAME_PROLOGUE_LEN) {
         continue;
      }
      // `lock cmpxchg`
      let locks = image
         .read(other, 0x80)
         .is_some_and(|head| head.windows(4).any(|w| w == [0xF0, 0x48, 0x0F, 0xB1]));
      let Some((start, end)) = image.function_bounds(other) else {
         continue;
      };
      // 6 shared callees on the builds seen, at most 2 for unrelated callers.
      let shared = callees(image, start, end)
         .into_iter()
         .filter(|callee| del_callees.contains(callee))
         .count();
      if locks && shared >= 4 {
         found.push(other);
      }
   }
   one("optimistic delete", &found)
}

/// `flag_override` in `StoreState`, from the inlined
/// `(user.flags | flag_override) & 1`:
///
/// ```text
/// cmp dword ptr [reg + tag], 1     ; 83 B8..BF <tag> 01
/// jne +0x0e
/// mov rax, qword ptr [reg + value] ; 48 8B 80..87 <value>
/// cmp reg2, 1; je +9; jmp +0xe; xor eax, eax; cmp reg2, 1; jne +7
/// or rax, qword ptr [reg + flags]  ; 48 0B 80..87 <flags>
/// ```
///
/// Reads off `&RwLock<StoreState>` sit 0x10 bytes higher, behind the lock, so
/// the lower pair is the one gateway handlers see.
pub fn staff(image: &Image<'_>) -> Found<StaffField> {
   const CHECK: Pattern = Pattern::new(
      "83 ?? ?? ?? ?? ?? 01 75 0E 48 8B ?? ?? ?? ?? ?? 83 ?? 01 74 09 EB 0E 31 C0 83 ?? 01 75 07 \
       48 0B ?? ?? ?? ?? ??",
   );
   const LOCK_HEADER: usize = 0x10;
   let disp32 = |modrm: u8, base: u8| (base..base + 8).contains(&modrm) && modrm != base + 4;
   let mut found = image
      .scan(CHECK)
      .into_iter()
      .filter_map(|at| {
         let bytes = image.read(at, 33)?;
         let (cmp, mov, or) = (bytes[1], bytes[11], bytes[32]);
         let same_base = cmp & 7 == mov & 7 && mov & 7 == or & 7;
         if !(disp32(cmp, 0xB8) && disp32(mov, 0x80) && disp32(or, 0x80) && same_base) {
            return None;
         }
         let (tag, value) = (imm32(image, at + 2)?, imm32(image, at + 12)?);
         (value == tag + 8).then_some(StaffField { tag, value })
      })
      .collect::<Vec<_>>();
   found.sort_unstable();
   found.dedup();
   match found[..] {
      [only] => Ok(only),
      [direct, locked]
         if locked.tag == direct.tag + LOCK_HEADER
            && locked.value == direct.value + LOCK_HEADER =>
      {
         Ok(direct)
      },
      _ => {
         Err(format!(
            "staff flag reads disagree ({} variants)",
            found.len()
         ))
      },
   }
}

/// `MessageContextMenu` builder and native `MenuItem` helpers for injecting
/// `phoenix.ui.addMessageAction` entries into right-click and `...` menus.
pub struct MessageContextMenu {
   pub build_fn:          usize,
   pub msg_ptr_offset:    usize,
   pub msg_id_offset:     usize,
   pub channel_id_offset: usize,
   pub props_new:         usize,
   pub with_id:           usize,
   pub with_icon:         usize,
   pub with_color:        usize,
   pub vec_push:          usize,
   pub layout:            MenuLayout,
}

/// `MenuItem` and its `Action` variant.
#[derive(Debug, Clone, Copy)]
pub struct MenuLayout {
   pub size:         usize,
   /// `MenuItemProps` inside `MenuItem::Action`.
   pub props:        usize,
   pub props_size:   usize,
   pub action_tag:   u64,
   /// The click handler `Rc<dyn Fn>`, and the word before it Discord zeroes.
   pub on_click:     usize,
   pub on_click_tag: usize,
}

/// Decodes `[reg + disp8]` or `[reg + disp32]` from a `48 8D` (`lea`) or
/// `48 8B` (`mov`) instruction where `modrm_base` is the `mod == 01` ModRM byte
/// (so `modrm_base + 0x40` is the matching `mod == 10` disp32 ModRM byte).
fn decode_disp(bytes: &[u8], opcode: u8, modrm_disp8: u8) -> Option<usize> {
   let modrm_disp32 = modrm_disp8.checked_add(0x40)?;
   for idx in 0..bytes.len().saturating_sub(3) {
      if bytes[idx] != 0x48 || bytes[idx + 1] != opcode {
         continue;
      }
      if bytes[idx + 2] == modrm_disp8 {
         return Some(usize::from(bytes[idx + 3]));
      }
      if bytes[idx + 2] == modrm_disp32 && idx + 7 <= bytes.len() {
         let disp = u32::from_le_bytes([
            bytes[idx + 3],
            bytes[idx + 4],
            bytes[idx + 5],
            bytes[idx + 6],
         ]);
         return Some(disp as usize);
      }
   }
   None
}

/// Locates `MessageContextMenu`'s item list builder (`build_fn`), the
/// `message_id` / `channel_id` offsets in its input, and the `MenuItem`
/// builders (`props_new`, `with_id`, `with_icon`, `with_color`, `vec_push`).
#[expect(
   clippy::too_many_lines,
   reason = "cross-checks both Copy Message ID and Delete Message menu blocks"
)]
pub fn message_context_menu(image: &Image<'_>) -> Found<MessageContextMenu> {
   let anchor = one(
      "MessageContextMenu string",
      &image.find_bytes(b"MessageContextMenu\0"),
   )?;
   let anchor_lea = one("MessageContextMenu xref", &lea_refs(image, anchor))?;
   let (build_fn, build_end) = image
      .function_bounds(anchor_lea)
      .ok_or("MessageContextMenu xref has no .pdata entry")?;
   if !prologue_at(image, build_fn, ROW_PROLOGUE, ROW_PROLOGUE_LEN) {
      return Err("MessageContextMenu prologue changed".to_owned());
   }

   // Decode `msg_ptr_offset` (`lea rcx, [rdx + disp]`, ModRM 0x4A/0x8A) and
   // `msg_id_offset` (`mov rdi, [rax + disp]`, ModRM 0x78/0xB8) from the head.
   let head = image
      .read(build_fn, 0x160)
      .ok_or("MessageContextMenu head is unmapped")?;
   let msg_ptr_offset =
      decode_disp(head, 0x8D, 0x4A).ok_or("MessageContextMenu message ptr offset not found")?;
   let msg_id_offset =
      decode_disp(head, 0x8B, 0x78).ok_or("MessageContextMenu message id offset not found")?;
   if !(0x20..=0x200).contains(&msg_ptr_offset) || !(0x20..=0x200).contains(&msg_id_offset) {
      return Err(format!(
         "MessageContextMenu offsets out of range (msg={msg_ptr_offset:#x}, id={msg_id_offset:#x})"
      ));
   }

   // Right after `MessageContextMenu`'s anchor call, the 4th call computes
   // message permissions (`r8 = &Message`) and reads `&message.channel_id` via
   // `lea rdx, [rdi + disp32]` (48 8D 97 disp32).
   let post_anchor_calls = calls_in(image, anchor_lea, 0x80);
   let channel_id_offset = post_anchor_calls
      .iter()
      .find_map(|&(_, callee)| {
         let body = image.read(callee, 0x80)?;
         let disp = decode_disp(body, 0x8D, 0x57)?;
         (0x100..=0x300).contains(&disp).then_some(disp)
      })
      .ok_or("MessageContextMenu channel_id offset not found")?;

   // Find the `Copy Message ID` and `Delete Message` `lea`s inside `build_fn`
   // directly (scanning only the ~19 KiB function instead of the whole `.text`).
   let build_bytes = image
      .read(build_fn, build_end - build_fn)
      .ok_or("MessageContextMenu body is unmapped")?;
   let find_in_build = |label: &[u8], name: &str| -> Found<usize> {
      let targets: HashSet<usize> = image.find_bytes(label).into_iter().collect();
      let mut hits = Vec::new();
      for idx in 0..build_bytes.len().saturating_sub(7) {
         if build_bytes[idx] & 0xF0 == 0x40
            && build_bytes[idx + 1] == 0x8D
            && build_bytes[idx + 2] & 0xC7 == 0x05
            && let Some(target) = rel32_target(image, build_fn + idx + 3)
            && targets.contains(&target)
         {
            hits.push(build_fn + idx);
         }
      }
      one(name, &hits)
   };
   let copy_id_lea = find_in_build(b"Copy Message ID", "Copy Message ID in MessageContextMenu")?;
   let delete_lea = find_in_build(b"Delete Message", "Delete Message in MessageContextMenu")?;

   let copy_calls = calls_in(image, copy_id_lea, 0x180);
   if copy_calls.len() < 5 {
      return Err(format!(
         "Copy Message ID block has only {} calls",
         copy_calls.len()
      ));
   }
   let copy_ctor = copy_calls[1].1;
   let with_id = copy_calls[2].1;
   let with_icon = copy_calls[3].1;
   let vec_push = copy_calls[4].1;

   let delete_calls = calls_in(image, delete_lea, 0x150);
   if delete_calls.len() < 6 {
      return Err(format!(
         "Delete Message block has only {} calls",
         delete_calls.len()
      ));
   }
   if delete_calls[2].1 != with_id
      || delete_calls[3].1 != with_icon
      || delete_calls[5].1 != vec_push
   {
      return Err("Copy Message ID and Delete Message helpers disagree".to_owned());
   }
   let with_color = delete_calls[4].1;

   // Walk `copy_ctor -> wrapper -> MenuItem::new_core -> MenuItemProps::new`
   // and verify the `MenuItemProps` size (`0x1C8`) and `Action` tag (`2`).
   let (_, ctor_end) = image
      .function_bounds(copy_ctor)
      .ok_or("copy-id ctor has no .pdata entry")?;
   let wrapper = calls_in(image, copy_ctor, ctor_end - copy_ctor)
      .last()
      .ok_or("copy-id ctor has no inner call")?
      .1;
   let (_, wrap_end) = image
      .function_bounds(wrapper)
      .ok_or("MenuItem ctor wrapper has no .pdata entry")?;
   let core_ctor = calls_in(image, wrapper, wrap_end - wrapper)
      .last()
      .ok_or("MenuItem ctor wrapper has no core call")?
      .1;
   let (_, core_end) = image
      .function_bounds(core_ctor)
      .ok_or("MenuItem core ctor has no .pdata entry")?;
   let core_bytes = image
      .read(core_ctor, core_end - core_ctor)
      .ok_or("MenuItem core ctor is unmapped")?;
   let props_new = calls_in(image, core_ctor, core_end - core_ctor)
      .first()
      .ok_or("MenuItem core ctor has no props_new call")?
      .1;
   let layout = menu_layout(image, core_bytes, vec_push)?;

   Ok(MessageContextMenu {
      build_fn,
      msg_ptr_offset,
      msg_id_offset,
      channel_id_offset,
      props_new,
      with_id,
      with_icon,
      with_color,
      vec_push,
      layout,
   })
}

/// Reads [`MenuLayout`] from the `MenuItem::Action` core constructor and
/// `Vec<MenuItem>::push`.
///
/// The constructor copies `MenuItemProps` in (`lea rdi, [sret + props];
/// lea rsi, ..; mov ecx, size; rep movsb`), clears the handler's tag (`and
/// qword [sret + d8], 0`), stores the `Rc<dyn Fn>` handler, and writes the
/// variant (`mov qword [sret], tag`). The push strides by the item size
/// (`imul reg, reg, size`).
fn menu_layout(image: &Image<'_>, core: &[u8], vec_push: usize) -> Found<MenuLayout> {
   let copy = core
      .windows(7)
      .position(|w| w[0] == 0xB9 && w[5..] == [0xF3, 0xA4])
      .ok_or("MenuItemProps copy not found")?;
   let props_size = imm_after(&core[copy..], &[0xB9]).ok_or("MenuItemProps size not found")?;
   let props = core[copy.saturating_sub(12)..copy]
      .windows(4)
      .find(|w| w[..2] == [0x48, 0x8D] && w[2] & 0xF8 == 0x78 && w[2] != 0x7C)
      .map(|w| usize::from(w[3]))
      .ok_or("MenuItemProps offset not found")?;
   let tail = &core[copy..];
   let (on_click, _) = rc_store(tail).ok_or("MenuItem handler store not found")?;
   let on_click_tag = tail
      .windows(5)
      .find(|w| w[..2] == [0x48, 0x83] && w[2] & 0xF8 == 0x60 && w[2] != 0x64 && w[4] == 0)
      .map(|w| usize::from(w[3]))
      .ok_or("MenuItem handler tag not found")?;
   let action_tag = tail
      .windows(7)
      .find(|w| w[..2] == [0x48, 0xC7] && w[2] < 8 && w[2] != 4 && w[2] != 5)
      .map(|w| u64::from(u32::from_le_bytes([w[3], w[4], w[5], w[6]])))
      .filter(|&tag| tag < 0x10)
      .ok_or("MenuItem::Action tag not found")?;
   let push = function_code(image, vec_push, "Vec<MenuItem>::push")?;
   let size = push
      .windows(7)
      .find(|w| w[0] & 0xF0 == 0x40 && w[1] == 0x69 && w[2] >= 0xC0)
      .map(|w| u32::from_le_bytes([w[3], w[4], w[5], w[6]]) as usize)
      .ok_or("Vec<MenuItem>::push stride not found")?;
   let size = fits("MenuItem", size, layout::SCRATCH_CAP)?;
   if props + props_size > size || on_click + 16 > size || on_click_tag + 8 > size {
      return Err("MenuItem fields sit past the MenuItem".to_owned());
   }
   Ok(MenuLayout {
      size,
      props,
      props_size,
      action_tag,
      on_click,
      on_click_tag,
   })
}

/// A `<T as IntoElement>::into_any_element` wrapper, the one small loader of
/// the type name `T` that copies `T` once (`mov r8d, size; call memcpy`), and
/// that size.
#[cfg(target_arch = "x86_64")]
fn find_into_any(
   image: &Image<'_>,
   leas: &HashMap<usize, Vec<usize>>,
   type_name: &[u8],
) -> Found<(usize, usize)> {
   let mut found = Vec::new();
   for addr in image.find_bytes(type_name) {
      for &lea in leas.get(&addr).into_iter().flatten() {
         let Some((start, end)) = image.function_bounds(lea) else {
            continue;
         };
         if end.saturating_sub(start) >= 0x100 || found.iter().any(|&(f, _)| f == start) {
            continue;
         }
         let Some(code) = image.read(start, end - start) else {
            continue;
         };
         let copies: Vec<usize> = code
            .windows(6)
            .filter(|w| w[..2] == [0x41, 0xB8])
            .map(|w| u32::from_le_bytes([w[2], w[3], w[4], w[5]]) as usize)
            .collect();
         if let [size] = copies[..] {
            found.push((start, size));
         }
      }
   }
   let label = std::str::from_utf8(type_name).unwrap_or("into_any");
   let (func, size) = one(label, &found)?;
   Ok((func, fits(label, size, layout::SCRATCH_CAP)?))
}

/// The loads of every copy of each of `needles`, from one pass over `.text`.
#[cfg(target_arch = "x86_64")]
fn string_leas(image: &Image<'_>, needles: &[&[u8]]) -> HashMap<usize, Vec<usize>> {
   let targets: Vec<usize> = needles
      .iter()
      .flat_map(|needle| image.find_bytes(needle))
      .collect();
   lea_refs_many(image, &targets)
}

/// What the native modal overlay builds and hooks.
///
/// `Workspace::render_overlays`, the `mana_html::hosted::modal::Modal`
/// builders, `Vec<AnyElement>::push`, `Div::id` (required for mouse-wheel
/// scrolling), GPUI's `WM_USER+5` force-redraw validation-number offset, and
/// the backdrop's click-outside wiring.
pub struct Modal {
   pub overlay_render:           usize,
   pub vec_push_any:             usize,
   pub modal_new:                usize,
   pub modal_into_any:           usize,
   pub modal_child_div:          usize,
   pub modal_footer_button:      usize,
   pub div_id:                   usize,
   pub window_validation_offset: usize,
   pub dismiss:                  Dismiss,
   pub layout:                   ModalLayout,
}

/// `mana_html::hosted::modal::Modal`.
#[derive(Debug, Clone, Copy)]
pub struct ModalLayout {
   pub size:     usize,
   /// `Vec<AnyElement>` of body children and of footer buttons.
   pub children: usize,
   pub footer:   usize,
   /// `Option<SharedString>`.
   pub subtitle: usize,
   /// `Option<Rc<dyn Fn(&mut Window, &mut App)>>`.
   pub on_close: usize,
   /// The `ModalSize` field.
   pub scale:    usize,
}

/// What Discord's overlay `backdrop` uses to close on a press outside the
/// dialog.
///
/// `Interactivity::on_mouse_down` pushes a `Box<dyn Fn(&MouseDownEvent,
/// DispatchPhase, &Hitbox, &mut Window, &mut App)>`, whose wrapper checks
/// `Hitbox::is_hovered` and calls `cx.stop_propagation()`.
#[derive(Debug, Clone, Copy)]
pub struct Dismiss {
   /// `Interactivity` offset of the mouse-down listener `Vec`.
   pub mouse_down_listeners: usize,
   /// `Vec<Box<dyn ..>>::push(vec, data, vtable)`.
   pub listener_push:        usize,
   /// `Hitbox::is_hovered(&Hitbox, &Window) -> bool`.
   pub hitbox_is_hovered:    usize,
   /// `App` offset of `propagate_event`.
   pub propagate_event:      usize,
}

/// Locates `Workspace::render_overlays` (`nav-overlay-popout-layer`), `Modal`
/// builders, `Vec<AnyElement>::push`, `Div::id`, and GPUI's
/// `WM_GPUI_FORCE_UPDATE_WINDOW` (`0x405`) validation offset.
#[expect(
   clippy::too_many_lines,
   reason = "one locator per modal builder, cross-checked"
)]
#[cfg(target_arch = "x86_64")]
pub fn modal(image: &Image<'_>, div_into_any: usize, div: &DivLayout) -> Found<Modal> {
   // `Modal::child::<Div>` and `Modal::footer_button` push an `AnyElement`
   // into one of the `Modal`'s `Vec`s (`lea rcx, [rax + vec]; lea rdx, [rbp +
   // d8]; call push`) and return it by copy (`mov ecx, size`). The child
   // pusher is the `Div` one whose `Vec` is not the footer's.
   const MODAL_PUSH: Pattern =
      Pattern::new("48 8D 48 ?? 48 8D 55 ?? E8 ?? ?? ?? ?? 90 B9 ?? ?? ?? ??");
   const MODAL_TYPE: &[u8] = b"mana_html::hosted::modal::Modal";
   const BUTTON_TYPE: &[u8] = b"mana_gpui::components::button::Button";
   let leas = string_leas(image, &[
      b"nav-overlay-popout-layer",
      b"mana-modal\0",
      MODAL_TYPE,
      BUTTON_TYPE,
      b"game-server-shop-carousel",
   ]);
   let leas_of = |addr: usize| leas.get(&addr).cloned().unwrap_or_default();

   let overlay_str = one(
      "nav-overlay-popout-layer",
      &image.find_bytes(b"nav-overlay-popout-layer"),
   )?;
   let overlay_lea = one("nav-overlay-popout-layer load", &leas_of(overlay_str))?;
   let (overlay_render, overlay_end) = image
      .function_bounds(overlay_lea)
      .ok_or("Workspace::render_overlays has no .pdata entry")?;
   if !prologue_at(image, overlay_render, FRAME_PROLOGUE, FRAME_PROLOGUE_LEN) {
      return Err("Workspace::render_overlays prologue changed".to_owned());
   }

   // Among copies of `mana-modal\0`, pick the one referenced by exactly two
   // functions: the large `Modal::render` (> 0x4000, FRAME_PROLOGUE) and the
   // small `Modal::new`.
   let mut modal_new = None;
   for addr in image.find_bytes(b"mana-modal\0") {
      let mut funcs: Vec<(usize, usize)> = leas_of(addr)
         .into_iter()
         .filter_map(|lea| image.function_bounds(lea))
         .collect();
      funcs.sort_unstable();
      funcs.dedup();
      if funcs.len() != 2 {
         continue;
      }
      let (small, large) = if funcs[0].1 - funcs[0].0 < funcs[1].1 - funcs[1].0 {
         (funcs[0], funcs[1])
      } else {
         (funcs[1], funcs[0])
      };
      if large.1 - large.0 > 0x4000
         && prologue_at(image, large.0, FRAME_PROLOGUE, FRAME_PROLOGUE_LEN)
      {
         modal_new = Some(small.0);
         break;
      }
   }
   let modal_new = modal_new.ok_or("Modal::new not found via mana-modal")?;

   let (modal_into_any, modal_size) = find_into_any(image, &leas, MODAL_TYPE)?;
   let (button_into_any, _) = find_into_any(image, &leas, BUTTON_TYPE)?;

   let mut pushers: Vec<(usize, usize, usize, usize)> = Vec::new();
   for at in image.scan(MODAL_PUSH) {
      let Some((start, end)) = image.function_bounds(at) else {
         continue;
      };
      if imm32(image, at + 15) != Some(modal_size) || pushers.iter().any(|p| p.0 == start) {
         continue;
      }
      let (Some(first), Some(push_fn), Some(vec)) = (
         calls_in(image, start, end - start)
            .first()
            .map(|&(_, callee)| callee),
         rel32_target(image, at + 9),
         image.read(at + 3, 1).map(|byte| usize::from(byte[0])),
      ) else {
         continue;
      };
      pushers.push((start, first, push_fn, vec));
   }
   let footers: Vec<(usize, usize, usize)> = pushers
      .iter()
      .filter(|p| p.1 == button_into_any)
      .map(|p| (p.0, p.2, p.3))
      .collect();
   let (modal_footer_button, push_b, footer) = one("Modal::footer_button", &footers)?;
   let childs: Vec<(usize, usize, usize)> = pushers
      .iter()
      .filter(|p| p.1 == div_into_any && p.3 != footer)
      .map(|p| (p.0, p.2, p.3))
      .collect();
   let (modal_child_div, vec_push_any, children) = one("Modal::child<Div>", &childs)?;
   if vec_push_any != push_b {
      return Err("Modal child/footer Vec<AnyElement>::push disagree".to_owned());
   }
   if !callees(image, overlay_render, overlay_end).contains(&vec_push_any) {
      return Err(
         "Workspace::render_overlays no longer pushes via Vec<AnyElement>::push".to_owned(),
      );
   }

   // `Div::id` is the first call after `lea` of `"game-server-shop-carousel"`,
   // checking `ElementId::None` in `Interactivity` (`cmp byte ptr [rcx +
   // disp32], 0xff`).
   let carousel = one(
      "game-server-shop-carousel",
      &image.find_bytes(b"game-server-shop-carousel"),
   )?;
   let carousel_lea = one("game-server-shop-carousel load", &leas_of(carousel))?;
   let div_id = calls_in(image, carousel_lea, 0x30)
      .first()
      .ok_or("Div::id call after carousel anchor not found")?
      .1;
   if !image.read(div_id, 0x200).is_some_and(|code| {
      code
         .windows(7)
         .any(|w| w[..2] == [0x80, 0xB9] && w[6] == 0xFF)
   }) {
      return Err("Div::id element_id offset check failed".to_owned());
   }

   // GPUI's `Zed::Window` WndProc checks `wParam == *(rc + disp32)` for
   // `WM_USER+5` (`0x405`, `WM_GPUI_FORCE_UPDATE_WINDOW`).
   const VALIDATION_PAT: Pattern = Pattern::new(
      "8D 83 FF FB FF FF 83 F8 13 0F 87 ?? ?? ?? ?? B9 ?? ?? ?? ?? 0F A3 C1 0F 83 ?? ?? ?? ?? 4C \
       3B B2",
   );
   let val_hit = one("GPUI window validation check", &image.scan(VALIDATION_PAT))?;
   let window_validation_offset =
      imm32(image, val_hit + 32).ok_or("GPUI window validation offset did not decode")?;
   if !(0x400..=0x4000).contains(&window_validation_offset) {
      return Err(format!(
         "GPUI window validation offset out of range ({window_validation_offset:#x})"
      ));
   }

   Ok(Modal {
      overlay_render,
      vec_push_any,
      modal_new,
      modal_into_any,
      modal_child_div,
      modal_footer_button,
      div_id,
      window_validation_offset,
      dismiss: backdrop_dismiss(image, overlay_render, overlay_end, div)?,
      layout: ModalLayout {
         size: modal_size,
         children,
         footer,
         subtitle: modal_subtitle(image, modal_size)?,
         on_close: modal_on_close(image, modal_size)?,
         scale: modal_scale(image, modal_new, modal_size)?,
      },
   })
}

/// The best of `ranked` (`(count, value)`, sorted down) if it beats the
/// runner-up `margin` times over.
#[cfg(target_arch = "x86_64")]
fn clear_winner(what: &str, ranked: &[(usize, usize)], margin: usize) -> Found<usize> {
   match ranked {
      [(top, value), rest @ ..] if rest.first().is_none_or(|&(next, _)| *top >= margin * next) => {
         Ok(*value)
      },
      _ => {
         Err(format!(
            "{what} is ambiguous ({:x?})",
            &ranked[..ranked.len().min(4)]
         ))
      },
   }
}

/// Small functions that return a `size`-byte struct by copy, `mov r8d, size;
/// call memcpy` or `mov ecx, size; rep movsb`.
#[cfg(target_arch = "x86_64")]
fn copy_returners(image: &Image<'_>, size: usize, max_len: usize) -> Vec<(usize, usize)> {
   let Ok(size) = u32::try_from(size) else {
      return Vec::new();
   };
   let le = size.to_le_bytes();
   let mut funcs: Vec<(usize, usize)> = [[0x41, 0xB8, le[0], le[1], le[2], le[3]].as_slice(), &[
      0xB9, le[0], le[1], le[2], le[3],
   ]]
   .into_iter()
   .flat_map(|needle| image.find_bytes(needle))
   .filter(|&at| image.is_code(at))
   .filter_map(|at| image.function_bounds(at))
   .filter(|&(start, end)| end - start <= max_len)
   .collect();
   funcs.sort_unstable();
   funcs.dedup();
   funcs
}

/// `Modal::subtitle`, the most called small `SharedString` setter on a
/// `Modal` (160 callers to 2 for the next on 2026-11 builds).
#[cfg(target_arch = "x86_64")]
fn modal_subtitle(image: &Image<'_>, size: usize) -> Found<usize> {
   let calls = call_index(image);
   let mut setters: Vec<(usize, usize)> = copy_returners(image, size, 0x180)
      .into_iter()
      .filter_map(|(start, end)| {
         let field = shared_string_setter(image.read(start, end - start)?)?;
         Some((calls.sites(start).count(), field))
      })
      .filter(|&(_, field)| field + 24 <= size)
      .collect();
   setters.sort_unstable_by(|a, b| b.cmp(a));
   clear_winner("Modal subtitle setter", &setters, 4)
}

/// `Modal::on_close`, the `Rc<dyn Fn>` most often stored into a `Modal` right
/// before it is returned by copy (344 stores, the only field on 2026-11
/// builds).
#[cfg(target_arch = "x86_64")]
fn modal_on_close(image: &Image<'_>, size: usize) -> Found<usize> {
   let mut votes: HashMap<usize, usize> = HashMap::new();
   let le = u32::try_from(size)
      .map_err(|_| "Modal size overflows")?
      .to_le_bytes();
   for at in image.find_bytes(&[0xB9, le[0], le[1], le[2], le[3]]) {
      if !image.is_code(at) {
         continue;
      }
      let Some(window) = image.read(at.saturating_sub(0x24), 0x24 + 0x10) else {
         continue;
      };
      if let Some((field, Some(copy))) = rc_store(window)
         && copy == size
         && field + 16 <= size
      {
         *votes.entry(field).or_default() += 1;
      }
   }
   let mut ranked: Vec<(usize, usize)> = votes.into_iter().map(|(at, n)| (n, at)).collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   clear_winner("Modal on_close store", &ranked, 4)
}

/// How often each byte of a `size`-byte struct is set right after `ctor`
/// returns it, over every call site, most set first.
#[cfg(target_arch = "x86_64")]
fn post_ctor_byte_stores(image: &Image<'_>, ctor: usize, size: usize) -> Vec<(usize, usize)> {
   let mut counts: HashMap<usize, usize> = HashMap::new();
   for site in call_index(image).sites(ctor) {
      let (Some(before), Some(after)) = (image.read(site - 0x30, 0x30), image.read(site + 5, 0x60))
      else {
         continue;
      };
      let mut seen = HashSet::new();
      for (at, _) in phoenix_hook::x86::sret_byte_stores(before, site - 0x30, after, site + 5) {
         if at < size && seen.insert(at) {
            *counts.entry(at).or_default() += 1;
         }
      }
   }
   let mut ranked: Vec<(usize, usize)> = counts.into_iter().map(|(at, n)| (n, at)).collect();
   ranked.sort_unstable_by(|a, b| b.cmp(a));
   ranked
}

/// `Modal::size`, the byte Discord most often sets right after `Modal::new`
/// (28 call sites to 3 on 2026-11 builds).
#[cfg(target_arch = "x86_64")]
fn modal_scale(image: &Image<'_>, modal_new: usize, size: usize) -> Found<usize> {
   clear_winner(
      "Modal size store",
      &post_ctor_byte_stores(image, modal_new, size),
      4,
   )
}

/// Follows the `"backdrop"` element in `Workspace::render_overlays` down to its
/// `on_mouse_down` call, a few small helpers deep.
#[cfg(target_arch = "x86_64")]
fn backdrop_dismiss(
   image: &Image<'_>,
   overlay: usize,
   overlay_end: usize,
   div: &DivLayout,
) -> Found<Dismiss> {
   let body = image
      .read(overlay, overlay_end - overlay)
      .ok_or("Workspace::render_overlays unreadable")?;
   let backdrop: Vec<usize> = body
      .windows(7)
      .enumerate()
      .filter(|(_, w)| w[1] == 0x8D && w[0] & 0xF0 == 0x40 && w[2] & 0xC7 == 0x05)
      .filter_map(|(offset, _)| {
         let lea = overlay + offset;
         let target = rel32_target(image, lea + 3)?;
         (image.read(target, 8)? == b"backdrop").then_some(lea)
      })
      .collect();
   let backdrop = one("backdrop load in render_overlays", &backdrop)?;

   // Breadth-first over small callees: `when(..)` -> `|div| ..` ->
   // `on_mouse_down`.
   let mut frontier: Vec<(usize, Option<usize>)> = calls_in(image, backdrop, 0x400)
      .into_iter()
      .map(|(_, callee)| (callee, None))
      .collect();
   let mut seen = HashSet::new();
   for _ in 0..3 {
      let mut next = Vec::new();
      for (func, parent) in frontier {
         let Some((start, end)) = image.function_bounds(func) else {
            continue;
         };
         if start != func || end - start > 0x400 || !seen.insert(func) {
            continue;
         }
         if let Some(found) = on_mouse_down_shape(image, start, end, div.interactivity_size) {
            let accessor_ok = parent.is_some_and(|parent| {
               image
                  .function_bounds(parent)
                  .is_some_and(|(p_start, p_end)| {
                     calls_in(image, p_start, p_end - p_start)
                        .iter()
                        .any(|&(_, callee)| interactivity_accessor(image, callee, div))
                  })
            });
            if !accessor_ok {
               return Err("on_mouse_down caller no longer reads Div's Interactivity".to_owned());
            }
            return Ok(found);
         }
         next.extend(
            calls_in(image, start, end - start)
               .into_iter()
               .map(|(_, callee)| (callee, Some(start))),
         );
      }
      frontier = next;
   }
   Err("backdrop on_mouse_down not found".to_owned())
}

/// `mov rax, [rcx + interactivity]` early in `Div`'s `interactivity()`.
#[cfg(target_arch = "x86_64")]
fn interactivity_accessor(image: &Image<'_>, func: usize, div: &DivLayout) -> bool {
   let Ok(disp) = u8::try_from(div.interactivity) else {
      return false;
   };
   let want = [0x48, 0x8B, 0x41, disp];
   image
      .read(func, 0x20)
      .is_some_and(|head| head.windows(4).any(|w| w == want))
}

/// `Interactivity::on_mouse_down`: `add reg, <vec offset>`, then
/// `lea r8, [vtable]; ...; jmp <push>`, where the vtable's `call` starts with
/// the `DispatchPhase::Bubble` test and calls `Hitbox::is_hovered`.
#[cfg(target_arch = "x86_64")]
fn on_mouse_down_shape(
   image: &Image<'_>,
   start: usize,
   end: usize,
   interactivity_size: usize,
) -> Option<Dismiss> {
   let body = image.read(start, end - start)?;
   let lea = body.windows(3).position(|w| w == [0x4C, 0x8D, 0x05])?;
   let vtable = rel32_target(image, start + lea + 3)?;
   let jmp = body[lea + 7..].iter().take(16).position(|&b| b == 0xE9)? + lea + 7;
   let listener_push = rel32_target(image, start + jmp + 1)?;
   let add = body[..lea]
      .windows(3)
      .rposition(|w| w[0] == 0x48 && w[1] == 0x81 && w[2] & 0xF8 == 0xC0)?;
   let mouse_down_listeners = imm32(image, start + add + 3)?;
   if mouse_down_listeners + 0x18 > interactivity_size || !image.is_code(listener_push) {
      return None;
   }

   let call = image.read_usize(vtable + 0x28)?;
   let wrapper = image.read(call, 0x100)?;
   if !wrapper[..0x10].windows(3).any(|w| w == [0x45, 0x84, 0xC0]) {
      return None;
   }
   // `mov rcx, r9; call is_hovered; test al, al; je ..; mov byte [reg + disp32],
   // 0`
   let at = wrapper.windows(19).position(|w| {
      matches!(w[..3], [0x4C, 0x89, 0xC9] | [0x49, 0x8B, 0xC9])
         && w[3] == 0xE8
         && w[8..11] == [0x84, 0xC0, 0x74]
         && w[12] == 0xC6
         && w[13] & 0xC0 == 0x80
         && w[13] & 0x07 != 0x04
         && w[18] == 0
   })?;
   // A button listener compares `event.button` first (`mov al, [rdx + disp8]`),
   // which rules out the scroll and move listener `Vec`s of the same shape.
   if !wrapper[..at]
      .windows(2)
      .any(|w| w == [0x8A, 0x42] || w == [0xB6, 0x42])
   {
      return None;
   }
   let hitbox_is_hovered = rel32_target(image, call + at + 4)?;
   let propagate_event = imm32(image, call + at + 14)?;
   (image.is_code(hitbox_is_hovered) && propagate_event < 0x10000).then_some(Dismiss {
      mouse_down_listeners,
      listener_push,
      hitbox_is_hovered,
      propagate_event,
   })
}

/// Native Mana/GPUI `FieldState`, `TextInput`, and `TextArea` bindings for
/// in-app URL entry and plugin code editing.
pub struct TextFields {
   pub cx_new_single:       usize,
   pub cx_new_multiline:    usize,
   pub entity_map_get:      usize,
   pub entity_downgrade:    usize,
   pub weak_drop:           usize,
   pub text_get:            usize,
   pub set_value:           usize,
   pub update_entity_dyn:   usize,
   pub arc_drop_slow:       usize,
   pub text_input_new:      usize,
   pub text_input_into_any: usize,
   pub text_area_new:       usize,
   pub text_area_into_any:  usize,
   pub layout:              FieldLayout,
}

/// Mana `FieldState`, the hosted `TextInput` and `TextArea` built on it, and
/// the GPUI handles they go through.
#[derive(Debug, Clone, Copy)]
pub struct FieldLayout {
   pub text_input_size:   usize,
   pub text_input_entity: usize,
   pub text_area_size:    usize,
   pub text_area_entity:  usize,
   /// `FieldState`'s `Option<SharedString>` slots: the two placeholders and
   /// the value `set_value` clears.
   pub placeholders:      [usize; 2],
   pub pending_value:     usize,
   /// `App`'s `EntityMap`.
   pub app_entities:      usize,
   /// The `Weak` inside a `WeakEntity`, what `weak_drop` takes a pointer to.
   pub weak_ptr:          usize,
}

/// Locates Mana `FieldState` creation, text read/write, and hosted
/// `TextInput`/`TextArea` element constructors.
#[cfg(target_arch = "x86_64")]
#[expect(
   clippy::too_many_lines,
   reason = "resolves and cross-checks FieldState, TextInput, and TextArea together"
)]
pub fn text_fields(image: &Image<'_>) -> Found<TextFields> {
   const READ_PANIC: &[u8] = b"readmana_gpui::components::rich_text::state::RichTextState";
   const TEXT_INPUT_TYPE: &[u8] = b"mana_html::hosted::text_input::TextInput";
   const TEXT_AREA_TYPE: &[u8] = b"mana_html::hosted::text_area::TextArea";
   const FIELD_NEW: Pattern = Pattern::new("56 48 83 EC 20 4D 89 C1 49 89 D0 48 89 CE 31 D2 E8");
   const FIELD_NEW_MULTI: Pattern =
      Pattern::new("56 48 83 EC 20 4D 89 C1 49 89 D0 48 89 CE B2 01 E8");
   /// Hosted `TextInput::new` and `TextArea::new` clone the entity into the
   /// struct first (`add rcx, entity; call entity_clone`).
   const HOSTED_NEW: Pattern = Pattern::new("56 48 83 EC 20 48 89 CE 48 81 C1 ?? ?? ?? ?? E8");

   let ctor_single = one("FieldState::new thunk", &image.scan(FIELD_NEW))?;
   let ctor_multi = one(
      "FieldState::new_multiline thunk",
      &image.scan(FIELD_NEW_MULTI),
   )?;
   let inner_a = rel32_target(image, ctor_single + 17).ok_or("FieldState::new call invalid")?;
   let inner_b =
      rel32_target(image, ctor_multi + 17).ok_or("FieldState::new_multiline call invalid")?;
   if inner_a != inner_b {
      return Err("FieldState::new and new_multiline target different inner functions".to_owned());
   }

   // Single pass over direct calls to find callers of both `FieldState` thunks.
   let mut single_callers = HashSet::new();
   let mut multi_callers = HashSet::new();
   for (site, callee) in call_sites(image, |t| t == ctor_single || t == ctor_multi) {
      if let Some((func, _)) = image.function_bounds(site) {
         if callee == ctor_single {
            single_callers.insert(func);
         } else {
            multi_callers.insert(func);
         }
      }
   }

   let pick_cx_new = |candidates: &HashSet<usize>,
                      ctor: usize,
                      name: &str|
    -> Found<(usize, usize, usize, usize)> {
      let mut matches: Vec<(usize, usize, usize, usize)> = candidates
         .iter()
         .filter_map(|&func| {
            let (start, end) = image.function_bounds(func)?;
            let body = image.read(start, end - start)?;
            if body.len() < 0x30 {
               return None;
            }
            let head = &body[..0x30];
            let has_inc = head.windows(3).any(|w| w == [0x48, 0xFF, 0x82]);
            let has_mov_rsi_r8 = head.windows(3).any(|w| w == [0x4C, 0x89, 0xC6]);
            if !has_inc || !has_mov_rsi_r8 {
               return None;
            }
            let calls: Vec<usize> = phoenix_hook::x86::disasm(body, start, 300)
               .into_iter()
               .filter(|line| line.text.starts_with("call "))
               .filter_map(|line| line.target)
               .collect();
            if body.len() > 0x260 || calls.len() != 10 || calls[4] != ctor {
               return None;
            }
            Some((start, calls[1], calls[3], calls[5]))
         })
         .collect();
      matches.sort_by_key(|(addr, ..)| *addr);
      matches
         .into_iter()
         .next()
         .ok_or_else(|| format!("{name} not found"))
   };

   let (cx_new_single, entity_clone, entity_downgrade, weak_drop) =
      pick_cx_new(&single_callers, ctor_single, "cx.new(FieldState::new)")?;
   let (cx_new_multiline, m_clone, m_down, m_weak) = pick_cx_new(
      &multi_callers,
      ctor_multi,
      "cx.new(FieldState::new_multiline)",
   )?;
   if entity_clone != m_clone || entity_downgrade != m_down || weak_drop != m_weak {
      return Err("cx.new single/multiline helpers disagree".to_owned());
   }

   // Set C: every function referencing any copy of the `RichTextState` "read"
   // panic string (may appear in multiple CGUs).
   let leas = string_leas(image, &[READ_PANIC, TEXT_INPUT_TYPE, TEXT_AREA_TYPE]);
   let read_panics = image.find_bytes(READ_PANIC);
   if read_panics.is_empty() {
      return Err("read RichTextState panic not found".to_owned());
   }
   let mut set_c: Vec<(usize, usize)> = read_panics
      .into_iter()
      .flat_map(|addr| leas.get(&addr).into_iter().flatten().copied())
      .filter_map(|lea| image.function_bounds(lea))
      .collect();
   set_c.sort_unstable();
   set_c.dedup();

   let c_calls: Vec<(usize, Vec<(usize, usize)>)> = set_c
      .iter()
      .map(|&(start, end)| {
         let calls = image
            .read(start, end - start)
            .map(|body| {
               phoenix_hook::x86::disasm(body, start, 600)
                  .into_iter()
                  .filter(|line| line.text.starts_with("call "))
                  .filter_map(|line| Some((line.addr, line.target?)))
                  .collect()
            })
            .unwrap_or_default();
         (start, calls)
      })
      .collect();
   let c_set: HashSet<usize> = set_c.iter().map(|&(start, _)| start).collect();

   // `text_get` is the member of C called by the most other members of C (>= 3).
   let mut caller_counts: HashMap<usize, usize> = HashMap::new();
   for (func, calls) in &c_calls {
      let mut seen = HashSet::new();
      for &(_, callee) in calls {
         if callee != *func && c_set.contains(&callee) && seen.insert(callee) {
            *caller_counts.entry(callee).or_default() += 1;
         }
      }
   }
   let text_get = caller_counts
      .into_iter()
      .filter(|&(_, count)| count >= 3)
      .max_by_key(|&(_, count)| count)
      .ok_or("FieldState text_get not found in RichTextState readers")?
      .0;

   let text_get_calls = &c_calls
      .iter()
      .find(|(f, _)| *f == text_get)
      .ok_or("text_get missing from set C")?
      .1;
   let entity_map_get = text_get_calls
      .first()
      .ok_or("text_get has no EntityMap::get call")?
      .1;

   // `set_value` is the member of C whose calls start `[SharedString::from,
   // text_get, ...]`, clears `pending_value` (`mov byte [reg + d8], 0xff`) and
   // updates the `RichTextState` entity by type name (`mov r9d, len`).
   let rts_len = u32::try_from(READ_PANIC.len() - b"read".len())
      .map_err(|_| "RichTextState name too long")?
      .to_le_bytes();
   let mov_r9d_len = [0x41, 0xB9, rts_len[0], rts_len[1], rts_len[2], rts_len[3]];
   let set_value_hits: Vec<(usize, &Vec<(usize, usize)>)> = c_calls
      .iter()
      .filter(|(func, calls)| {
         if calls.len() < 4 || calls[0].1 == entity_map_get || calls[1].1 != text_get {
            return false;
         }
         let Some((start, end)) = image.function_bounds(*func) else {
            return false;
         };
         let Some(body) = image.read(start, end - start) else {
            return false;
         };
         none_stores(body).len() == 1 && body.windows(6).any(|w| w == mov_r9d_len)
      })
      .map(|(f, calls)| (*f, calls))
      .collect();
   if set_value_hits.len() != 1 {
      return Err(format!(
         "FieldState::set_value matched {} functions",
         set_value_hits.len()
      ));
   }
   let (set_value, sv_calls) = set_value_hits[0];

   // `arc_drop_slow` is called right after `lock dec qword ptr [rax]` (`F0 48 FF
   // 08`).
   let arc_drop_slow = sv_calls
      .iter()
      .find_map(|&(site, callee)| {
         let before = image.read(site.saturating_sub(24), 24)?;
         before
            .windows(4)
            .any(|w| w == [0xF0, 0x48, 0xFF, 0x08])
            .then_some(callee)
      })
      .ok_or("Arc<str>::drop_slow not found in FieldState::set_value")?;

   // `update_entity_dyn` is called in `set_value` right after `mov r9d, len`
   // and begins with `inc qword ptr [rcx + disp32]`.
   let update_entity_dyn = sv_calls
      .iter()
      .find_map(|&(site, callee)| {
         let before = image.read(site.saturating_sub(32), 32)?;
         if !before.windows(6).any(|w| w == mov_r9d_len) {
            return None;
         }
         let head = image.read(callee, 0x40)?;
         head
            .windows(3)
            .any(|w| w == [0x48, 0xFF, 0x81])
            .then_some(callee)
      })
      .ok_or("App::update_entity_dyn not found in FieldState::set_value")?;

   // Several hosted elements share `HOSTED_NEW`, so each is the one that fills
   // its own size.
   let hosted: Vec<(usize, usize, usize)> = image
      .scan(HOSTED_NEW)
      .into_iter()
      .filter(|&at| rel32_target(image, at + 16) == Some(entity_clone))
      .filter_map(|at| {
         let entity = imm32(image, at + 11)?;
         let end = image.function_bounds(at).map_or(at + 0x800, |(_, end)| end);
         let extent = phoenix_hook::x86::sret_stores(image.read(at, end - at)?, at)
            .into_iter()
            .map(|(field, len)| field + len)
            .max()?;
         Some((at, entity, extent))
      })
      .collect();
   let hosted_new = |size: usize, name: &str| -> Found<(usize, usize)> {
      let hits: Vec<(usize, usize)> = hosted
         .iter()
         .filter(|&&(_, entity, extent)| {
            extent <= size && size - extent < 8 && entity + 0x20 <= size
         })
         .map(|&(at, entity, _)| (at, entity))
         .collect();
      one(name, &hits)
   };
   let (text_input_into_any, text_input_size) = find_into_any(image, &leas, TEXT_INPUT_TYPE)?;
   let (text_area_into_any, text_area_size) = find_into_any(image, &leas, TEXT_AREA_TYPE)?;
   let (text_input_new, text_input_entity) = hosted_new(text_input_size, "hosted TextInput::new")?;
   let (text_area_new, text_area_entity) = hosted_new(text_area_size, "hosted TextArea::new")?;

   // `FieldState::new` ends by writing `None` into its three
   // `Option<SharedString>`s; the one `set_value` clears is the pending value.
   let pending_value = none_stores(function_code(image, set_value, "FieldState::set_value")?)[0];
   let slots = none_stores(function_code(image, inner_a, "FieldState::new")?);
   let slots = slots
      .get(slots.len().saturating_sub(3)..)
      .filter(|slots| slots.len() == 3 && slots.contains(&pending_value))
      .ok_or("FieldState::new no longer clears three SharedString slots")?;
   let placeholders: Vec<usize> = slots
      .iter()
      .copied()
      .filter(|&slot| slot != pending_value)
      .collect();
   let [first, second] = placeholders[..] else {
      return Err("FieldState placeholder slots did not decode".to_owned());
   };

   // `text_get` opens with `lea rcx, [r8 + entities]` for `EntityMap::get`.
   let app_entities = image
      .read(text_get, 0x40)
      .and_then(|head| head.windows(4).find(|w| w[..3] == [0x49, 0x8D, 0x48]))
      .map(|w| usize::from(w[3]))
      .ok_or("App entity map offset not found in text_get")?;
   // `entity_downgrade` bumps the weak count: `mov rax, [rdx + weak]; cmp rax,
   // -1`.
   let weak_ptr = image
      .read(entity_downgrade, 0x20)
      .and_then(|head| {
         head
            .windows(8)
            .find(|w| w[..3] == [0x48, 0x8B, 0x42] && w[4..] == [0x48, 0x83, 0xF8, 0xFF])
      })
      .map(|w| usize::from(w[3]))
      .filter(|&weak| weak + 8 <= layout::ENTITY)
      .ok_or("WeakEntity Weak offset not found in entity_downgrade")?;

   Ok(TextFields {
      cx_new_single,
      cx_new_multiline,
      entity_map_get,
      entity_downgrade,
      weak_drop,
      text_get,
      set_value,
      update_entity_dyn,
      arc_drop_slow,
      text_input_new,
      text_input_into_any,
      text_area_new,
      text_area_into_any,
      layout: FieldLayout {
         text_input_size,
         text_input_entity,
         text_area_size,
         text_area_entity,
         placeholders: [first, second],
         pending_value,
         app_entities,
         weak_ptr,
      },
   })
}

/// Locates `libdiscord_media::fetch` via its `tracing::Metadata` callsite
/// static (`"media_fetch"` / `"libdiscord_media::fetch"`), so the desktop
/// runtime can unwrap custom badge icon URLs (`/badge-icons/phx_u_<hex>`).
pub fn media_fetch(image: &Image<'_>) -> Found<usize> {
   let name = one(
      "media_fetch tracing name",
      &image.find_bytes(b"media_fetchlibdiscord_media::fetch"),
   )?;
   let target = name + 11;
   let name_bytes = name.to_le_bytes();

   let mut callsites = Vec::new();
   for seg in image.segments.iter().filter(|s| !s.executable) {
      let mut pos = 0;
      while pos + 8 <= seg.bytes.len() {
         let Some(rel) = seg.bytes[pos..]
            .chunks_exact(8)
            .position(|chunk| chunk == name_bytes)
         else {
            break;
         };
         let off = pos + rel * 8;
         let slot = seg.addr + off;
         if image.read_usize(slot + 8) == Some(11)
            && image.read_usize(slot + 0x10) == Some(target)
            && image.read_usize(slot + 0x18) == Some(23)
            && let Some(callsite) = image.read_usize(slot + 0x30)
         {
            callsites.push(callsite);
         }
         pos = off + 8;
      }
   }
   let callsite = one("media_fetch callsite", &callsites)?;

   let mut funcs: Vec<usize> = image
      .rip_refs(callsite..callsite + 1)
      .into_iter()
      .filter_map(|(field, _)| Some(image.function_bounds(field)?.0))
      .collect();
   funcs.sort_unstable();
   funcs.dedup();
   let func = one("libdiscord_media::fetch", &funcs)?;
   if !prologue_at(image, func, MEDIA_FETCH_PROLOGUE, MEDIA_FETCH_PROLOGUE_LEN) {
      return Err("libdiscord_media::fetch prologue changed".to_owned());
   }
   Ok(func)
}
