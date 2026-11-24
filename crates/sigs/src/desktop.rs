//! Where Desktop Phoenix (`Discord.exe`) keeps what the runtime patches.
//!
//! Found from strings, call graphs and instruction shapes, never fixed
//! addresses. The runtime installs from these, and `sigscan check` runs them on
//! a new build before anyone ships against it.

use std::collections::{
   HashMap,
   HashSet,
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
/// `host::section` and `Store::delete_message` prologue: 8 pushes and
/// `sub rsp, imm32`.
pub const FRAME_PROLOGUE: Pattern =
   Pattern::new("55 41 57 41 56 41 55 41 54 56 57 53 48 81 EC ?? ?? ?? ??");
pub const FRAME_PROLOGUE_LEN: usize = 19;

/// The 22-byte `.rdata` string naming settings page 24, relabeled in place.
pub const SETTINGS_LABEL: &[u8] = b"PhoenixDESKTOP_PHOENIX";

/// GPUI and Mana struct layouts the settings page and message accessories
/// build by hand. [`gpui_layouts`] checks them against the constructors.
pub mod layout {
   pub const DIV: usize = 0x88;
   pub const ANY_ELEMENT: usize = 0x20;
   pub const SWITCH: usize = 0x248;
   pub const SWITCH_ON_CHANGE: usize = 0x230;
   pub const BUTTON: usize = 0x328;
   pub const BUTTON_ON_CLICK: usize = 0x298;
   pub const BUTTON_SIZE: usize = 0x31D;
   pub const BUTTON_VARIANT: usize = 0x323;
   /// One past the last `StyleRefinement` byte the runtime writes.
   pub const STYLE_END: usize = 0x27D;
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
   image
      .rip_refs(target..target + 1)
      .into_iter()
      .filter_map(|(field, _)| {
         let start = field.checked_sub(3)?;
         let head = image.read(start, 3)?;
         (head[0] & 0xF0 == 0x40 && head[1] == 0x8D && head[2] & 0xC7 == 0x05).then_some(start)
      })
      .collect()
}

/// `(site, callee)` for every `call rel32` whose callee passes `keep`.
fn call_sites(image: &Image<'_>, keep: impl Fn(usize) -> bool) -> Vec<(usize, usize)> {
   let mut sites = Vec::new();
   for segment in image.segments.iter().filter(|segment| segment.executable) {
      for (offset, window) in segment.bytes.windows(5).enumerate() {
         if window[0] != 0xE8 {
            continue;
         }
         let rel = i32::from_le_bytes([window[1], window[2], window[3], window[4]]);
         let site = segment.addr + offset;
         if let Some(callee) = (site + 5).checked_add_signed(rel as isize)
            && keep(callee)
         {
            sites.push((site, callee));
         }
      }
   }
   sites
}

/// The functions that call `callee`.
fn callers(image: &Image<'_>, callee: usize) -> HashSet<usize> {
   call_sites(image, |target| target == callee)
      .into_iter()
      .filter_map(|(site, _)| Some(image.function_bounds(site)?.0))
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

/// Checks the [`layout`]s the runtime builds against this build's constructors.
///
/// Each struct must still fit, and its constructor must still set the fields
/// the runtime overwrites. A layout that moved would make the runtime
/// write into the wrong field or past the struct, so it turns the settings
/// page and accessories off instead.
#[cfg(target_arch = "x86_64")]
pub fn gpui_layouts(image: &Image<'_>, gpui: &Gpui) -> Found<()> {
   let stores = |name: &str, func: usize| -> Found<Vec<(usize, usize)>> {
      let end = image
         .function_bounds(func)
         .map_or(func + 0x800, |(_, end)| end);
      let code = image
         .read(func, end - func)
         .ok_or_else(|| format!("{name} is unmapped"))?;
      Ok(phoenix_hook::x86::sret_stores(code, func))
   };
   let check = |name: &str, func: usize, size: usize, fields: &[usize]| -> Found<()> {
      let stores = stores(name, func)?;
      let extent = stores.iter().map(|&(at, len)| at + len).max().unwrap_or(0);
      if extent == 0 || extent > size {
         return Err(format!(
            "{name} fills {extent:#x} bytes, the runtime expects {size:#x}"
         ));
      }
      for field in fields {
         if !stores.iter().any(|&(at, _)| at == *field) {
            return Err(format!("{name} no longer sets +{field:#x}"));
         }
      }
      Ok(())
   };
   check("Div::new", gpui.div_new, layout::DIV, &[])?;
   check("Switch::new", gpui.switch_new, layout::SWITCH, &[
      layout::SWITCH_ON_CHANGE,
   ])?;
   check("Button::new", gpui.button_new, layout::BUTTON, &[
      layout::BUTTON_ON_CLICK,
      layout::BUTTON_SIZE,
      layout::BUTTON_VARIANT,
   ])?;
   // `Div::new` boxes its `StyleRefinement`: `mov ecx, size; mov edx, 8; call`.
   let code = image
      .read(gpui.div_new, 0x100)
      .ok_or("Div::new is unmapped")?;
   let style = code
      .windows(10)
      .find(|w| w[0] == 0xB9 && w[5..10] == [0xBA, 0x08, 0x00, 0x00, 0x00])
      .map(|w| u32::from_le_bytes([w[1], w[2], w[3], w[4]]) as usize)
      .ok_or("Div::new style allocation not found")?;
   if style < layout::STYLE_END {
      return Err(format!(
         "StyleRefinement is {style:#x} bytes, the runtime writes up to {:#x}",
         layout::STYLE_END
      ));
   }
   Ok(())
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
