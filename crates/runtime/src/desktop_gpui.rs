//! Native GPUI / Mana widget bindings for Desktop Phoenix User Settings.
//!
//! Resolves `Discord.exe`'s own compiled GPUI (`Div`, `AnyElement`,
//! `SharedString`) and Mana (`Switch`, `Button`, `TextBlock`, `ActionRow`)
//! builders from unique string anchors during [`crate::prepare`] — no
//! hardcoded addresses and no webview overlay. All controls render natively
//! inside Discord's GPUI render tree with Discord's theme, fonts, spacing, and
//! scroll container.

use std::{
   collections::HashSet,
   ffi::c_void,
   mem::MaybeUninit,
   ptr,
   sync::{
      LazyLock,
      Mutex,
      OnceLock,
      PoisonError,
   },
};

use phoenix_hook::image::Image;

use crate::{
   Chosen,
   Control,
   update::{
      self,
      Catalog,
   },
};

/// `gpui::SharedString` (24 bytes): tag `0x18` is `Static(&'static str)`,
/// requiring no allocator or `Arc::drop`.
#[repr(C)]
#[derive(Clone, Copy)]
struct SharedString {
   tag:  u8,
   _pad: [u8; 7],
   ptr:  *const u8,
   len:  usize,
}

impl SharedString {
   fn from_static(s: &'static str) -> Self {
      Self {
         tag:  0x18,
         _pad: [0; 7],
         ptr:  s.as_ptr(),
         len:  s.len(),
      }
   }

   fn from_str(s: &str) -> Self {
      Self::from_static(intern(s))
   }
}

/// Process-lifetime string interner so dynamic plugin names, descriptions, and
/// GPUI element IDs can be passed as `SharedString::Static(&'static str)` with
/// zero allocator mismatch and bounded memory.
static INTERNED: LazyLock<Mutex<HashSet<&'static str>>> =
   LazyLock::new(|| Mutex::new(HashSet::new()));

fn intern(s: &str) -> &'static str {
   let mut set = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
   if let Some(&existing) = set.get(s) {
      return existing;
   }
   let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
   set.insert(leaked);
   leaked
}

#[repr(C, align(8))]
struct RawDiv([u8; 0x88]);

#[repr(C, align(8))]
pub struct RawAnyElement([u8; 0x20]);

#[repr(C, align(8))]
struct RawSwitch([u8; 0x248]);

#[repr(C, align(8))]
struct RawButton([u8; 0x328]);

#[repr(C)]
struct RcInner {
   strong:    usize,
   weak:      usize,
   action_id: u64,
}

#[repr(C)]
struct RcFnVtbl {
   drop_in_place: *const c_void,
   size:          usize,
   align:         usize,
   call_once:     *const c_void,
   call_mut:      *const c_void,
   call:          *const c_void,
   extra:         *const c_void,
}

// SAFETY: vtables contain only function pointers and plain integers.
unsafe impl Sync for RcFnVtbl {}

static SWITCH_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     switch_callback as *const c_void,
   call_mut:      switch_callback as *const c_void,
   call:          switch_callback as *const c_void,
   extra:         ptr::null(),
};

static BUTTON_FN_VTBL: RcFnVtbl = RcFnVtbl {
   drop_in_place: ptr::null(),
   size:          size_of::<u64>(),
   align:         align_of::<u64>(),
   call_once:     button_callback as *const c_void,
   call_mut:      button_callback as *const c_void,
   call:          button_callback as *const c_void,
   extra:         ptr::null(),
};

type DivNewFn = unsafe extern "C" fn(*mut RawDiv, *const c_void) -> *mut RawDiv;
type DivMutFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivGapFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, f32) -> *mut RawDiv;
type DivChildDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivIntoAnyFn = unsafe extern "C" fn(*mut RawAnyElement, *mut RawDiv) -> *mut RawAnyElement;
type TextBlockFn =
   unsafe extern "C" fn(*mut RawDiv, *const SharedString, *const SharedString) -> *mut RawDiv;
type SwitchNewFn = unsafe extern "C" fn(*mut RawSwitch, *const u8, usize, u8) -> *mut RawSwitch;
type SwitchIntoDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawSwitch) -> *mut RawDiv;
type ButtonNewFn =
   unsafe extern "C" fn(*mut RawButton, *const u8, usize, *const SharedString) -> *mut RawButton;
type ActionRowFn = unsafe extern "C" fn(
   *mut RawDiv,
   *const SharedString,
   *const SharedString,
   *mut RawButton,
) -> *mut RawDiv;
/// Discord's `alloc_or_oom(align: usize, size: usize) -> *mut u8`.
type AllocOrOomFn = unsafe extern "C" fn(usize, usize) -> *mut u8;

struct GpuiSymbols {
   caller_loc:      *const c_void,
   div_new:         DivNewFn,
   div_flex:        DivMutFn,
   div_flex_col:    DivMutFn,
   div_gap:         DivGapFn,
   div_child_div:   DivChildDivFn,
   div_into_any:    DivIntoAnyFn,
   text_block:      TextBlockFn,
   switch_new:      SwitchNewFn,
   switch_into_div: SwitchIntoDivFn,
   button_new:      ButtonNewFn,
   action_row:      ActionRowFn,
   alloc_or_oom:    AllocOrOomFn,
}

// SAFETY: function pointers and static `.rdata` `Location` pointer in `Discord.exe`.
unsafe impl Send for GpuiSymbols {}
unsafe impl Sync for GpuiSymbols {}

static SYMBOLS: OnceLock<GpuiSymbols> = OnceLock::new();

#[derive(Clone)]
enum Action {
   ToggleStaff,
   ReloadAll,
   OpenDevTools,
   CheckUpdates,
   ApplyUpdates,
   RefreshCatalog,
   TogglePlugin(String),
   ReloadPlugin(String),
   UninstallPlugin(String),
   ToggleSetting {
      plugin: String,
      key:    String,
   },
   CycleSelect {
      plugin: String,
      key:    String,
      next:   String,
   },
   PressButton {
      plugin: String,
      key:    String,
   },
   InstallCatalog(String),
}

static ACTIONS: LazyLock<Mutex<Vec<Action>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn register_action(action: Action) -> u64 {
   let mut list = ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   let id = list.len() as u64;
   list.push(action);
   id
}

fn lookup_action(id: u64) -> Option<Action> {
   let list = ACTIONS.lock().unwrap_or_else(PoisonError::into_inner);
   usize::try_from(id).ok().and_then(|idx| list.get(idx).cloned())
}

unsafe extern "C" fn switch_callback(
   self_ptr: *const u64,
   checked: u8,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id` inside our `Rc<dyn Fn>`.
      let id = unsafe { *self_ptr };
      let on = checked != 0;
      let Some(action) = lookup_action(id) else {
         return;
      };
      match action {
         Action::ToggleStaff => crate::set_staff_enabled(on),
         Action::TogglePlugin(name) => crate::set_enabled(&name, on),
         Action::ToggleSetting { plugin, key } => {
            crate::set_setting(&plugin, &key, Chosen::Switch(on));
         },
         _ => {},
      }
   });
}

unsafe extern "C" fn button_callback(
   self_ptr: *const u64,
   _event: *const c_void,
   _window: *mut c_void,
   _app: *mut c_void,
) {
   if self_ptr.is_null() {
      return;
   }
   let _ = std::panic::catch_unwind(|| {
      // SAFETY: `self_ptr` points to `RcInner.action_id` inside our `Rc<dyn Fn>`.
      let id = unsafe { *self_ptr };
      let Some(action) = lookup_action(id) else {
         return;
      };
      match action {
         Action::ReloadAll => crate::reload_all(),
         Action::OpenDevTools => phoenix_ui_desktop::open_devtools(),
         Action::CheckUpdates => update::check(),
         Action::ApplyUpdates => update::apply(),
         Action::RefreshCatalog => update::fetch_catalog(),
         Action::ReloadPlugin(name) => crate::reload(&name),
         Action::UninstallPlugin(name) => {
            if let Err(err) = crate::uninstall(&name) {
               log::warn!("sunbird settings: uninstall {name}: {err}");
            }
         },
         Action::CycleSelect { plugin, key, next } => {
            crate::set_setting(&plugin, &key, Chosen::Text(next));
         },
         Action::PressButton { plugin, key } => crate::press(&plugin, &key),
         Action::InstallCatalog(name) => {
            std::thread::spawn(move || {
               match update::install_listed(&name) {
                  Ok(installed) => {
                     log::info!("sunbird settings: installed {installed}");
                     phoenix_ui_desktop::toast(&format!("Installed {installed}"));
                  },
                  Err(err) => {
                     log::warn!("sunbird settings: install {name}: {err}");
                     phoenix_ui_desktop::toast(&format!("Install failed: {err}"));
                  },
               }
            });
         },
         _ => {},
      }
   });
}

// ---------------------------------------------------------------------------
// Build-independent symbol resolution from unique string anchors
// ---------------------------------------------------------------------------

fn find_lea_xrefs(image: &Image<'_>, target: usize) -> Vec<usize> {
   let mut hits = Vec::new();
   for seg in image.segments.iter().filter(|s| s.executable) {
      let bytes = seg.bytes.as_ref();
      if bytes.len() < 7 {
         continue;
      }
      for idx in 1..=bytes.len() - 6 {
         if bytes[idx] != 0x8D {
            continue;
         }
         if (bytes[idx - 1] & 0xF0) != 0x40 || (bytes[idx + 1] & 0xC7) != 0x05 {
            continue;
         }
         let disp = i32::from_le_bytes([
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
            bytes[idx + 5],
         ]);
         let next_rip = seg.addr + idx + 6;
         if next_rip.wrapping_add_signed(disp as isize) == target {
            hits.push(seg.addr + idx - 1);
         }
      }
   }
   hits
}

/// Scans `image` in `[start, start + len)` for `E8 rel32` direct calls whose
/// destination is a `.pdata` function start or a valid leaf/thunk function
/// entry in `.text` (e.g. `Button::new` and `Div::into_any_element`).
fn calls_in(image: &Image<'_>, start: usize, len: usize) -> Vec<(usize, usize)> {
   let Some(bytes) = image.read(start, len) else {
      return Vec::new();
   };
   let mut out = Vec::new();
   let mut idx = 0;
   while idx + 5 <= bytes.len() {
      if bytes[idx] == 0xE8 {
         let disp = i32::from_le_bytes([
            bytes[idx + 1],
            bytes[idx + 2],
            bytes[idx + 3],
            bytes[idx + 4],
         ]);
         let call_addr = start + idx;
         let target = (call_addr + 5).wrapping_add_signed(disp as isize);
         let is_target = if image.is_function(target) {
            true
         } else if image.is_code(target) {
            image.read(target, 1).is_some_and(|first| {
               matches!(
                  first[0],
                  0xE9 | 0x48 | 0x4C | 0x40 | 0x41 | 0x55 | 0x53 | 0x56 | 0x57 | 0x89 | 0x8B | 0xB8
                     | 0x31 | 0x33
               )
            })
         } else {
            false
         };
         if is_target {
            out.push((call_addr, target));
            idx += 5;
            continue;
         }
      }
      idx += 1;
   }
   out
}

/// Decodes the last `lea rdx, [rip + disp32]` (`48 8D 15 <i32>`) in
/// `[call_addr - 0x20, call_addr)`.
fn find_rdx_lea_before(image: &Image<'_>, call_addr: usize) -> Option<usize> {
   let start = call_addr.checked_sub(0x20)?;
   let bytes = image.read(start, 0x20)?;
   for idx in (0..=bytes.len().saturating_sub(7)).rev() {
      if bytes[idx] == 0x48 && bytes[idx + 1] == 0x8D && bytes[idx + 2] == 0x15 {
         let disp = i32::from_le_bytes([
            bytes[idx + 3],
            bytes[idx + 4],
            bytes[idx + 5],
            bytes[idx + 6],
         ]);
         let next_rip = start + idx + 7;
         return Some(next_rip.wrapping_add_signed(disp as isize));
      }
   }
   None
}

/// Resolves Discord's native GPUI / Mana widget builders from unique string
/// anchors. Safe to call from `DllMain` (`prepare()`): performs only read-only
/// memory scans.
pub fn locate(image: &Image<'static>) -> bool {
   if SYMBOLS.get().is_some() {
      return true;
   }

   // Anchor A: Page 16 ("Developer Mode") inside `host::section`.
   let dev_strs = image.find_bytes(b"set-devmodeDeveloper Mode");
   let [dev_str] = dev_strs[..] else {
      log::warn!("desktop gpui: set-devmode matched {} times", dev_strs.len());
      return false;
   };
   let dev_leas = find_lea_xrefs(image, dev_str);
   let [lea_dev] = dev_leas[..] else {
      log::warn!("desktop gpui: set-devmode xrefs matched {} times", dev_leas.len());
      return false;
   };

   let before_dev = calls_in(image, lea_dev - 0x70, 0x70);
   let after_dev = calls_in(image, lea_dev, 0x240);
   if before_dev.len() < 3 || after_dev.len() < 9 {
      log::warn!(
         "desktop gpui: unexpected Page 16 call counts ({}, {})",
         before_dev.len(),
         after_dev.len()
      );
      return false;
   }
   let (div_new_call, div_new) = before_dev[0];
   let div_flex = before_dev[1].1;
   let div_flex_col = before_dev[2].1;
   let Some(caller_loc) = find_rdx_lea_before(image, div_new_call) else {
      log::warn!("desktop gpui: could not decode Location for div_new");
      return false;
   };
   let switch_new = after_dev[0].1;
   let switch_on_change = after_dev[7].1;
   let switch_into_div = after_dev[8].1;

   let on_change_calls = calls_in(image, switch_on_change, 0x50);
   let Some(&(_, alloc_or_oom)) = on_change_calls.first() else {
      log::warn!("desktop gpui: could not resolve alloc_or_oom inside Switch::on_change");
      return false;
   };

   // Anchor B: `TesterUpdates` Sign In button + ActionRow.
   let btn_strs = image.find_bytes(b"tester-updates-sign-in\0");
   let [btn_str] = btn_strs[..] else {
      log::warn!("desktop gpui: tester-updates-sign-in matched {} times", btn_strs.len());
      return false;
   };
   let btn_leas = find_lea_xrefs(image, btn_str);
   let [lea_btn] = btn_leas[..] else {
      log::warn!("desktop gpui: tester-updates-sign-in xrefs matched {} times", btn_leas.len());
      return false;
   };
   let after_btn = calls_in(image, lea_btn, 0x1d0);
   if after_btn.len() < 6 {
      log::warn!("desktop gpui: unexpected TesterUpdates call count ({})", after_btn.len());
      return false;
   }
   let button_new = after_btn[0].1;
   let action_row = after_btn[4].1;
   let div_into_any = after_btn[5].1;

   // Inside `action_row`, decode `div_gap`, `text_block`, and `div_child_div`,
   // and cross-check `div_new` and `div_flex` against Anchor A.
   let Some((ar_start, ar_end)) = image.function_bounds(action_row) else {
      log::warn!("desktop gpui: no .pdata bounds for ActionRow {action_row:#x}");
      return false;
   };
   let ar_calls = calls_in(image, ar_start, ar_end - ar_start);
   if ar_calls.len() < 12 || ar_calls[0].1 != div_new || ar_calls[2].1 != div_flex {
      log::warn!(
         "desktop gpui: ActionRow cross-check failed (len={}, div_match={}, flex_match={})",
         ar_calls.len(),
         ar_calls.first().is_some_and(|c| c.1 == div_new),
         ar_calls.get(2).is_some_and(|c| c.1 == div_flex),
      );
      return false;
   }
   let div_gap = ar_calls[1].1;
   let text_block = ar_calls[6].1;
   let div_child_div = ar_calls.last().expect("checked len >= 12").1;

   // SAFETY: every resolved address was verified via `Image::is_function` and
   // matches the exact Rust-ABI signatures verified in Discord.exe.
   let symbols = unsafe {
      GpuiSymbols {
         caller_loc:      caller_loc as *const c_void,
         div_new:         std::mem::transmute::<usize, DivNewFn>(div_new),
         div_flex:        std::mem::transmute::<usize, DivMutFn>(div_flex),
         div_flex_col:    std::mem::transmute::<usize, DivMutFn>(div_flex_col),
         div_gap:         std::mem::transmute::<usize, DivGapFn>(div_gap),
         div_child_div:   std::mem::transmute::<usize, DivChildDivFn>(div_child_div),
         div_into_any:    std::mem::transmute::<usize, DivIntoAnyFn>(div_into_any),
         text_block:      std::mem::transmute::<usize, TextBlockFn>(text_block),
         switch_new:      std::mem::transmute::<usize, SwitchNewFn>(switch_new),
         switch_into_div: std::mem::transmute::<usize, SwitchIntoDivFn>(switch_into_div),
         button_new:      std::mem::transmute::<usize, ButtonNewFn>(button_new),
         action_row:      std::mem::transmute::<usize, ActionRowFn>(action_row),
         alloc_or_oom:    std::mem::transmute::<usize, AllocOrOomFn>(alloc_or_oom),
      }
   };
   let _ = SYMBOLS.set(symbols);
   log::info!(
      "desktop gpui: resolved native Mana builders (div={div_new:#x}, switch={switch_new:#x}, \
       button={button_new:#x}, row={action_row:#x})"
   );
   true
}

// ---------------------------------------------------------------------------
// Safe builder helpers over Discord's native GPUI / Mana widgets
// ---------------------------------------------------------------------------

struct Builder {
   sym: &'static GpuiSymbols,
}

impl Builder {
   fn alloc_rc_action(&self, action: Action) -> *mut RcInner {
      let id = register_action(action);
      // SAFETY: calls Discord's own `alloc_or_oom(align=8, size=24)` so
      // Discord's `Rc::drop` frees it with the matching allocator.
      unsafe {
         let raw = (self.sym.alloc_or_oom)(align_of::<RcInner>(), size_of::<RcInner>());
         let inner = raw.cast::<RcInner>();
         inner.write(RcInner {
            strong:    1,
            weak:      1,
            action_id: id,
         });
         inner
      }
   }

   fn v_stack(&self, gap_px: f32) -> RawDiv {
      // SAFETY: sequentially constructs `div().flex().flex_col().gap(px)` using
      // Discord's native GPUI builders.
      unsafe {
         let mut d0 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_new)(d0.as_mut_ptr(), self.sym.caller_loc);
         let mut d0 = d0.assume_init();

         let mut d1 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_flex)(d1.as_mut_ptr(), &raw mut d0);
         let mut d1 = d1.assume_init();

         let mut d2 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_flex_col)(d2.as_mut_ptr(), &raw mut d1);
         let mut d2 = d2.assume_init();

         let mut d3 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_gap)(d3.as_mut_ptr(), &raw mut d2, gap_px);
         d3.assume_init()
      }
   }

   fn push_div(&self, mut parent: RawDiv, mut child: RawDiv) -> RawDiv {
      // SAFETY: transfers ownership of both `RawDiv`s into `div_child_div`.
      unsafe {
         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_child_div)(out.as_mut_ptr(), &raw mut parent, &raw mut child);
         out.assume_init()
      }
   }

   fn text_block(&self, title: &str, subtitle: &str) -> RawDiv {
      let title = SharedString::from_str(title);
      let subtitle = SharedString::from_str(subtitle);
      // SAFETY: `SharedString::Static` pointers live for `'static`.
      unsafe {
         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.text_block)(out.as_mut_ptr(), &raw const title, &raw const subtitle);
         out.assume_init()
      }
   }

   fn switch_row(
      &self,
      id: &str,
      label: &str,
      description: Option<&str>,
      checked: bool,
      action: Action,
   ) -> RawDiv {
      let id_static = intern(id);
      let label_ss = SharedString::from_str(label);
      // SAFETY: builds a native Mana `Switch`, populates its `label` (+0x98),
      // optional `description` (+0xb0), and `on_change` `Rc<dyn Fn>` (+0x230),
      // then wraps it in a `Div` via `switch_into_div`.
      unsafe {
         let mut sw = MaybeUninit::<RawSwitch>::uninit();
         (self.sym.switch_new)(
            sw.as_mut_ptr(),
            id_static.as_ptr(),
            id_static.len(),
            u8::from(checked),
         );
         let mut sw = sw.assume_init();
         let base = sw.0.as_mut_ptr();
         base.add(0x98).cast::<SharedString>().write(label_ss);
         if let Some(desc) = description
            && !desc.is_empty()
         {
            let desc_ss = SharedString::from_str(desc);
            base.add(0xb0).cast::<SharedString>().write(desc_ss);
         }
         let rc_ptr = self.alloc_rc_action(action);
         base.add(0x230).cast::<*mut RcInner>().write(rc_ptr);
         base.add(0x238)
            .cast::<*const RcFnVtbl>()
            .write(&raw const SWITCH_FN_VTBL);

         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.switch_into_div)(out.as_mut_ptr(), &raw mut sw);
         out.assume_init()
      }
   }

   fn action_row(
      &self,
      id: &str,
      title: &str,
      subtitle: &str,
      button_label: &str,
      action: Action,
   ) -> RawDiv {
      let id_static = intern(id);
      let title_ss = SharedString::from_str(title);
      let sub_ss = SharedString::from_str(subtitle);
      let btn_ss = SharedString::from_str(button_label);
      // SAFETY: builds a native Mana `Button`, attaches our `on_click`
      // `Rc<dyn Fn>` at +0x298..0x2a8, and passes it into Discord's native
      // `ActionRow(title, subtitle, button)` builder.
      unsafe {
         let mut btn = MaybeUninit::<RawButton>::uninit();
         (self.sym.button_new)(
            btn.as_mut_ptr(),
            id_static.as_ptr(),
            id_static.len(),
            &raw const btn_ss,
         );
         let mut btn = btn.assume_init();
         let rc_ptr = self.alloc_rc_action(action);
         let base = btn.0.as_mut_ptr();
         base.add(0x298).cast::<*mut RcInner>().write(rc_ptr);
         base.add(0x2a0)
            .cast::<*const RcFnVtbl>()
            .write(&raw const BUTTON_FN_VTBL);

         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.action_row)(
            out.as_mut_ptr(),
            &raw const title_ss,
            &raw const sub_ss,
            &raw mut btn,
         );
         out.assume_init()
      }
   }

   fn into_any(&self, mut div: RawDiv, sret: *mut RawAnyElement) {
      // SAFETY: writes the 32-byte `gpui::AnyElement` directly to `sret`.
      unsafe {
         (self.sym.div_into_any)(sret, &raw mut div);
      }
   }
}

// ---------------------------------------------------------------------------
// Sunbird Native GPUI Settings Page
// ---------------------------------------------------------------------------

/// Renders the entire native Sunbird Settings page into `sret` (`AnyElement`).
/// Returns `false` if native GPUI symbols were not located (caller falls back
/// to Discord's original page 24).
pub fn render_sunbird_page(sret: *mut RawAnyElement) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let b = Builder { sym };

   // Clear actions from the previous render pass before building this frame.
   ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();

   let plugins = crate::plugins();
   let enabled_count = plugins.iter().filter(|p| p.enabled).count();
   let installed_names: HashSet<String> = plugins.iter().map(|p| p.name.clone()).collect();

   let mut root = b.v_stack(20.0);

   // 1. Header
   let summary = format!(
      "v{} · {enabled_count}/{} plugins active · Press Ctrl+Shift+I for Runtime DevTools",
      update::VERSION,
      plugins.len()
   );
   root = b.push_div(root, b.text_block("Sunbird", &summary));

   // 2. Core controls
   root = b.push_div(
      root,
      b.switch_row(
         "sunbird-staff-unlock",
         "Staff Unlock",
         Some("Unlock Discord staff-only settings, experiments, and internal menus."),
         crate::staff_enabled(),
         Action::ToggleStaff,
      ),
   );
   root = b.push_div(
      root,
      b.action_row(
         "sunbird-reload-all",
         "Reload All Plugins",
         "Re-evaluate every enabled plugin without restarting Discord.",
         "Reload All",
         Action::ReloadAll,
      ),
   );
   root = b.push_div(
      root,
      b.action_row(
         "sunbird-open-devtools",
         "Runtime Console & DevTools",
         "Evaluate JavaScript in QuickJS and view live runtime logs (Ctrl+Shift+I).",
         "Open DevTools",
         Action::OpenDevTools,
      ),
   );

   // 3. Updates row
   let upd_status = update::status();
   let upd_found = update::found();
   let (upd_desc, upd_btn, upd_act) = if update::busy() {
      (
         upd_status.unwrap_or_else(|| "Checking for updates...".to_owned()),
         "Working...",
         Action::CheckUpdates,
      )
   } else if let Some(found) = upd_found
      && !found.plugins.is_empty()
   {
      (
         format!("Plugin updates available: {}", found.plugins.join(", ")),
         "Apply Updates",
         Action::ApplyUpdates,
      )
   } else {
      (
         upd_status.unwrap_or_else(|| format!("Sunbird {} is up to date.", update::VERSION)),
         "Check Updates",
         Action::CheckUpdates,
      )
   };
   root = b.push_div(
      root,
      b.action_row("sunbird-updates", "Updates", &upd_desc, upd_btn, upd_act),
   );

   // 4. Installed Plugins
   root = b.push_div(
      root,
      b.text_block(
         "Installed Plugins",
         "Toggle plugins on or off and configure plugin settings.",
      ),
   );
   if plugins.is_empty() {
      root = b.push_div(
         root,
         b.text_block(
            "No plugins installed",
            "Install a plugin from the Plugin Catalog below or drop a .js bundle into the plugins folder.",
         ),
      );
   } else {
      for plugin in &plugins {
         let title = match &plugin.header.version {
            Some(ver) => format!("{}  (v{ver})", plugin.name),
            None => plugin.name.clone(),
         };
         let mut desc_parts = Vec::new();
         if let Some(d) = &plugin.header.description {
            desc_parts.push(d.clone());
         }
         if let Some(a) = &plugin.header.author {
            desc_parts.push(format!("by {a}"));
         }
         if let Some(err) = &plugin.error {
            desc_parts.push(format!("Error: {err}"));
         }
         let desc = desc_parts.join(" · ");
         root = b.push_div(
            root,
            b.switch_row(
               &format!("sunbird-plugin-{}", plugin.name),
               &title,
               (!desc.is_empty()).then_some(desc.as_str()),
               plugin.enabled,
               Action::TogglePlugin(plugin.name.clone()),
            ),
         );

         if plugin.enabled {
            for setting in &plugin.settings {
               let sid = format!("sunbird-set-{}-{}", plugin.name, setting.key);
               let label = format!("    {}", setting.title);
               let sdesc = setting.description.as_deref().unwrap_or("");
               match &setting.control {
                  Control::Switch(val) => {
                     root = b.push_div(
                        root,
                        b.switch_row(
                           &sid,
                           &label,
                           setting.description.as_deref(),
                           *val,
                           Action::ToggleSetting {
                              plugin: plugin.name.clone(),
                              key:    setting.key.clone(),
                           },
                        ),
                     );
                  },
                  Control::Select { options, chosen } => {
                     let cur_idx = options
                        .iter()
                        .position(|o| o.value == *chosen)
                        .unwrap_or(0);
                     let cur_label = options
                        .get(cur_idx)
                        .map_or(chosen.as_str(), |o| o.label.as_str());
                     let next_val = if options.is_empty() {
                        chosen.clone()
                     } else {
                        options[(cur_idx + 1) % options.len()].value.clone()
                     };
                     root = b.push_div(
                        root,
                        b.action_row(
                           &sid,
                           &label,
                           sdesc,
                           cur_label,
                           Action::CycleSelect {
                              plugin: plugin.name.clone(),
                              key:    setting.key.clone(),
                              next:   next_val,
                           },
                        ),
                     );
                  },
                  Control::Button => {
                     root = b.push_div(
                        root,
                        b.action_row(
                           &sid,
                           &label,
                           sdesc,
                           &setting.title,
                           Action::PressButton {
                              plugin: plugin.name.clone(),
                              key:    setting.key.clone(),
                           },
                        ),
                     );
                  },
                  Control::Text(val) => {
                     let sub = if sdesc.is_empty() {
                        format!("Current value: {val}")
                     } else {
                        format!("{sdesc} (Current: {val})")
                     };
                     root = b.push_div(root, b.text_block(&label, &sub));
                  },
                  Control::Secret(val) => {
                     let state = if val.is_empty() { "not set" } else { "configured" };
                     let sub = if sdesc.is_empty() {
                        format!("Secret ({state})")
                     } else {
                        format!("{sdesc} ({state})")
                     };
                     root = b.push_div(root, b.text_block(&label, &sub));
                  },
                  Control::Number(val) => {
                     let sub = if sdesc.is_empty() {
                        format!("Value: {val}")
                     } else {
                        format!("{sdesc} (Value: {val})")
                     };
                     root = b.push_div(root, b.text_block(&label, &sub));
                  },
               }
            }
            if plugin.installed {
               root = b.push_div(
                  root,
                  b.action_row(
                     &format!("sunbird-reload-{}", plugin.name),
                     &format!("    Reload {}", plugin.name),
                     "Reload this plugin's bundle live.",
                     "Reload",
                     Action::ReloadPlugin(plugin.name.clone()),
                  ),
               );
            }
         } else if plugin.installed {
            root = b.push_div(
               root,
               b.action_row(
                  &format!("sunbird-remove-{}", plugin.name),
                  &format!("    Remove {}", plugin.name),
                  "Delete this installed plugin from disk.",
                  "Remove",
                  Action::UninstallPlugin(plugin.name.clone()),
               ),
            );
         }
      }
   }

   // 5. Plugin Catalog
   root = b.push_div(
      root,
      b.text_block(
         "Plugin Catalog",
         "Official plugins from assets.sunbird.tools.",
      ),
   );
   let catalog = update::catalog();
   match catalog {
      Catalog::Unfetched => {
         update::fetch_catalog();
         root = b.push_div(
            root,
            b.action_row(
               "sunbird-catalog-loading",
               "Loading catalog...",
               "Fetching official plugins from assets.sunbird.tools.",
               "Refresh",
               Action::RefreshCatalog,
            ),
         );
      },
      Catalog::Loading => {
         root = b.push_div(
            root,
            b.action_row(
               "sunbird-catalog-loading",
               "Loading catalog...",
               "Fetching official plugins from assets.sunbird.tools.",
               "Refresh",
               Action::RefreshCatalog,
            ),
         );
      },
      Catalog::Failed(err) => {
         root = b.push_div(
            root,
            b.action_row(
               "sunbird-catalog-retry",
               "Catalog unavailable",
               &err,
               "Retry",
               Action::RefreshCatalog,
            ),
         );
      },
      Catalog::Loaded(items) => {
         for item in items {
            let is_installed = installed_names.contains(&item.name);
            let sub = match &item.warning {
               Some(w) if !w.is_empty() => format!("{} · Note: {w}", item.description),
               _ => item.description.clone(),
            };
            root = b.push_div(
               root,
               b.action_row(
                  &format!("sunbird-cat-{}", item.name),
                  &item.name,
                  &sub,
                  if is_installed { "Reinstall" } else { "Install" },
                  Action::InstallCatalog(item.name.clone()),
               ),
            );
         }
      },
   }

   b.into_any(root, sret);
   true
}
