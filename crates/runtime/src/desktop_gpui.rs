//! Native GPUI / Mana widget bindings for Desktop Phoenix User Settings.
//!
//! Resolves `Discord.exe`'s own compiled GPUI (`Div`, `StyleRefinement`,
//! `AnyElement`, `SharedString`) and Mana (`Switch`, `Button`, `TextBlock`,
//! `ActionRow`) builders from unique string anchors during [`crate::prepare`] —
//! no hardcoded addresses and no webview overlay. All controls render natively
//! inside Discord's GPUI render tree with category tabs, rounded bordered
//! cards, collapsible plugin settings dropdowns, and native select dropdowns.

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
      Listed,
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

#[derive(Clone, Copy)]
#[repr(u8)]
enum BtnVariant {
   Primary   = 0,
   Secondary = 1,
   Critical  = 3,
}

#[derive(Clone, Copy)]
#[repr(u8)]
enum BtnSize {
   Small  = 1,
   Medium = 2,
}

/// Encodes `Some(AbsoluteLength::Pixels(px))` in GPUI's `StyleRefinement`.
fn gpui_px(px: f32) -> u64 {
   u64::from(px.to_bits()) << 32
}

type DivNewFn = unsafe extern "C" fn(*mut RawDiv, *const c_void) -> *mut RawDiv;
type DivMutFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivStyleMutFn = unsafe extern "C" fn(*mut RawDiv) -> *mut u8;
type DivChildDivFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawDiv) -> *mut RawDiv;
type DivChildBtnFn = unsafe extern "C" fn(*mut RawDiv, *mut RawDiv, *mut RawButton) -> *mut RawDiv;
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
   caller_loc:       *const c_void,
   div_new:          DivNewFn,
   div_flex:         DivMutFn,
   div_flex_col:     DivMutFn,
   div_style_mut:    DivStyleMutFn,
   div_child_div:    DivChildDivFn,
   div_child_button: DivChildBtnFn,
   div_into_any:     DivIntoAnyFn,
   text_block:       TextBlockFn,
   switch_new:       SwitchNewFn,
   switch_into_div:  SwitchIntoDivFn,
   button_new:       ButtonNewFn,
   action_row:       ActionRowFn,
   alloc_or_oom:     AllocOrOomFn,
}

// SAFETY: function pointers and static `.rdata` `Location` pointer in `Discord.exe`.
unsafe impl Send for GpuiSymbols {}
unsafe impl Sync for GpuiSymbols {}

static SYMBOLS: OnceLock<GpuiSymbols> = OnceLock::new();

// ---------------------------------------------------------------------------
// Interactive UI state (tabs, filters, collapsible dropdowns, open selects)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
   Plugins,
   Catalog,
   General,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PluginFilter {
   All,
   Enabled,
   Disabled,
}

struct UiState {
   tab:                  Tab,
   filter:               PluginFilter,
   expanded_plugins:     HashSet<String>,
   open_select:          Option<(String, String)>,
   cat_desktop_open:     bool,
   cat_partial_open:     bool,
   cat_mobile_open:      bool,
}

impl Default for UiState {
   fn default() -> Self {
      Self {
         tab:              Tab::Plugins,
         filter:           PluginFilter::All,
         expanded_plugins: HashSet::new(),
         open_select:      None,
         cat_desktop_open: true,
         cat_partial_open: false,
         cat_mobile_open:  false,
      }
   }
}

static UI_STATE: LazyLock<Mutex<UiState>> = LazyLock::new(|| Mutex::new(UiState::default()));

#[derive(Clone)]
enum Action {
   SetTab(Tab),
   SetFilter(PluginFilter),
   ToggleExpandPlugin(String),
   ToggleSelectDropdown {
      plugin: String,
      key:    String,
   },
   PickSelectOption {
      plugin: String,
      key:    String,
      value:  String,
   },
   ToggleCatalogGroup(u8),
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
         Action::SetTab(tab) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            ui.tab = tab;
            ui.open_select = None;
         },
         Action::SetFilter(filter) => {
            UI_STATE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .filter = filter;
         },
         Action::ToggleExpandPlugin(name) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            if !ui.expanded_plugins.remove(&name) {
               ui.expanded_plugins.insert(name);
            }
            ui.open_select = None;
         },
         Action::ToggleSelectDropdown { plugin, key } => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            let target = (plugin, key);
            if ui.open_select.as_ref() == Some(&target) {
               ui.open_select = None;
            } else {
               ui.open_select = Some(target);
            }
         },
         Action::PickSelectOption { plugin, key, value } => {
            crate::set_setting(&plugin, &key, Chosen::Text(value));
            UI_STATE
               .lock()
               .unwrap_or_else(PoisonError::into_inner)
               .open_select = None;
         },
         Action::ToggleCatalogGroup(group) => {
            let mut ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
            match group {
               0 => ui.cat_desktop_open = !ui.cat_desktop_open,
               1 => ui.cat_partial_open = !ui.cat_partial_open,
               _ => ui.cat_mobile_open = !ui.cat_mobile_open,
            }
         },
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
/// entry in `.text`.
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
/// anchors. Safe to call from `DllMain` (`prepare()`).
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

   // Inside `action_row`, cross-check `div_new` and `div_flex`, then extract
   // `div_style_mut`, `text_block`, `div_child_button`, and `div_child_div`.
   let Some((ar_start, ar_end)) = image.function_bounds(action_row) else {
      log::warn!("desktop gpui: no .pdata bounds for ActionRow {action_row:#x}");
      return false;
   };
   let ar_calls = calls_in(image, ar_start, ar_end - ar_start);
   if ar_calls.len() < 16 || ar_calls[0].1 != div_new || ar_calls[2].1 != div_flex {
      log::warn!(
         "desktop gpui: ActionRow cross-check failed (len={}, div_match={}, flex_match={})",
         ar_calls.len(),
         ar_calls.first().is_some_and(|c| c.1 == div_new),
         ar_calls.get(2).is_some_and(|c| c.1 == div_flex),
      );
      return false;
   }
   let div_py = ar_calls[1].1;
   let py_calls = calls_in(image, div_py, 0x40);
   let Some(&(_, div_style_mut)) = py_calls.first() else {
      log::warn!("desktop gpui: could not resolve div_style_mut inside div_py");
      return false;
   };
   let text_block = ar_calls[6].1;
   let div_child_button = ar_calls[14].1;
   let div_child_div = ar_calls.last().expect("checked len >= 16").1;

   // SAFETY: every resolved address was verified via `is_function` / cross-check
   // and matches the exact Rust-ABI signatures verified in Discord.exe.
   let symbols = unsafe {
      GpuiSymbols {
         caller_loc:       caller_loc as *const c_void,
         div_new:          std::mem::transmute::<usize, DivNewFn>(div_new),
         div_flex:         std::mem::transmute::<usize, DivMutFn>(div_flex),
         div_flex_col:     std::mem::transmute::<usize, DivMutFn>(div_flex_col),
         div_style_mut:    std::mem::transmute::<usize, DivStyleMutFn>(div_style_mut),
         div_child_div:    std::mem::transmute::<usize, DivChildDivFn>(div_child_div),
         div_child_button: std::mem::transmute::<usize, DivChildBtnFn>(div_child_button),
         div_into_any:     std::mem::transmute::<usize, DivIntoAnyFn>(div_into_any),
         text_block:       std::mem::transmute::<usize, TextBlockFn>(text_block),
         switch_new:       std::mem::transmute::<usize, SwitchNewFn>(switch_new),
         switch_into_div:  std::mem::transmute::<usize, SwitchIntoDivFn>(switch_into_div),
         button_new:       std::mem::transmute::<usize, ButtonNewFn>(button_new),
         action_row:       std::mem::transmute::<usize, ActionRowFn>(action_row),
         alloc_or_oom:     std::mem::transmute::<usize, AllocOrOomFn>(alloc_or_oom),
      }
   };
   let _ = SYMBOLS.set(symbols);
   log::info!(
      "desktop gpui: resolved native Mana builders (div={div_new:#x}, style={div_style_mut:#x}, \
       switch={switch_new:#x}, button={button_new:#x}, row={action_row:#x})"
   );
   true
}

// ---------------------------------------------------------------------------
// Native GPUI / Mana Layout & Styling Helpers
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

   fn raw_div(&self) -> RawDiv {
      // SAFETY: constructs an empty GPUI `Div`.
      unsafe {
         let mut d = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_new)(d.as_mut_ptr(), self.sym.caller_loc);
         d.assume_init()
      }
   }

   fn style_div(&self, div: &mut RawDiv, f: impl FnOnce(*mut u8)) {
      // SAFETY: `div_style_mut` returns a valid mutable pointer to `Div`'s
      // `StyleRefinement` (verified offsets from `Discord.exe`).
      let style = unsafe { (self.sym.div_style_mut)(&raw mut *div) };
      if !style.is_null() {
         f(style);
      }
   }

   fn v_stack(&self, gap_px: f32) -> RawDiv {
      // SAFETY: `div().flex().flex_col()` + true `gap` at StyleRefinement +0x1b8/+0x1c0.
      unsafe {
         let mut d0 = self.raw_div();
         let mut d1 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_flex)(d1.as_mut_ptr(), &raw mut d0);
         let mut d1 = d1.assume_init();

         let mut d2 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_flex_col)(d2.as_mut_ptr(), &raw mut d1);
         let mut d2 = d2.assume_init();

         self.style_div(&mut d2, |s| {
            let g = gpui_px(gap_px);
            s.add(0x1b8).cast::<u64>().write(g);
            s.add(0x1c0).cast::<u64>().write(g);
         });
         d2
      }
   }

   fn h_row(&self, gap_px: f32, space_between: bool) -> RawDiv {
      // SAFETY: `div().flex().items_center().gap(px)` (+ optional `justify_between`).
      unsafe {
         let mut d0 = self.raw_div();
         let mut d1 = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_flex)(d1.as_mut_ptr(), &raw mut d0);
         let mut d1 = d1.assume_init();

         self.style_div(&mut d1, |s| {
            let g = gpui_px(gap_px);
            s.add(0x1b8).cast::<u64>().write(g);
            s.add(0x1c0).cast::<u64>().write(g);
            s.add(0x279).write(4); // align_items = Center
            if space_between {
               s.add(0x27c).write(6); // justify_content = SpaceBetween
            }
         });
         d1
      }
   }

   /// Rounded bordered card container matching Discord's native Settings cards.
   fn card(&self, gap_px: f32, pad_px: f32, radius_px: f32, border_alpha: f32) -> RawDiv {
      let mut d = self.v_stack(gap_px);
      // SAFETY: verified StyleRefinement offsets:
      // +0x110..+0x130 = border_widths, +0x130..+0x150 = corner_radii,
      // +0x198..+0x1b8 = padding, +0x250 = Option<Hsla> border_color.
      self.style_div(&mut d, |s| unsafe {
         let bw = gpui_px(1.0);
         for off in [0x110usize, 0x118, 0x120, 0x128] {
            s.add(off).cast::<u64>().write(bw);
         }
         let r = gpui_px(radius_px);
         for off in [0x130usize, 0x138, 0x140, 0x148] {
            s.add(off).cast::<u64>().write(r);
         }
         let p = gpui_px(pad_px);
         for off in [0x198usize, 0x1a0, 0x1a8, 0x1b0] {
            s.add(off).cast::<u64>().write(p);
         }
         // Neutral translucent border that adapts across dark, ash, midnight, and light themes.
         s.add(0x250).cast::<u32>().write(1);
         s.add(0x254).cast::<[f32; 4]>().write([0.62, 0.05, 0.55, border_alpha]);
      });
      d
   }

   /// Subtle 1px horizontal divider line between sections or rows.
   fn divider(&self) -> RawDiv {
      let mut d = self.raw_div();
      // SAFETY: 1px bottom border with translucent neutral color and vertical margin.
      self.style_div(&mut d, |s| unsafe {
         s.add(0x120).cast::<u64>().write(gpui_px(1.0)); // border_bottom = 1px
         s.add(0x1f0).cast::<u64>().write(gpui_px(1.0)); // height = 1px
         s.add(0x250).cast::<u32>().write(1);
         s.add(0x254).cast::<[f32; 4]>().write([0.62, 0.05, 0.55, 0.16]);
      });
      d
   }

   fn push_div(&self, mut parent: RawDiv, mut child: RawDiv) -> RawDiv {
      // SAFETY: moves both `RawDiv`s into `div_child_div`.
      unsafe {
         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_child_div)(out.as_mut_ptr(), &raw mut parent, &raw mut child);
         out.assume_init()
      }
   }

   fn build_button(
      &self,
      id: &str,
      label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawButton {
      let id_static = intern(id);
      let label_ss = SharedString::from_str(label);
      // SAFETY: creates a native Mana `Button`, sets variant (+0x323), size
      // (+0x31d), and attaches our `Rc<dyn Fn>` click callback at +0x298..+0x2a8.
      unsafe {
         let mut btn = MaybeUninit::<RawButton>::uninit();
         (self.sym.button_new)(
            btn.as_mut_ptr(),
            id_static.as_ptr(),
            id_static.len(),
            &raw const label_ss,
         );
         let mut btn = btn.assume_init();
         let base = btn.0.as_mut_ptr();
         base.add(0x323).write(variant as u8);
         base.add(0x31d).write(size as u8);
         let rc_ptr = self.alloc_rc_action(action);
         base.add(0x298).cast::<*mut RcInner>().write(rc_ptr);
         base.add(0x2a0)
            .cast::<*const RcFnVtbl>()
            .write(&raw const BUTTON_FN_VTBL);
         btn
      }
   }

   fn push_button(
      &self,
      mut parent: RawDiv,
      id: &str,
      label: &str,
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawDiv {
      let mut btn = self.build_button(id, label, variant, size, action);
      // SAFETY: appends native Mana `Button` directly into `parent`.
      unsafe {
         let mut out = MaybeUninit::<RawDiv>::uninit();
         (self.sym.div_child_button)(out.as_mut_ptr(), &raw mut parent, &raw mut btn);
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
      // SAFETY: creates a native Mana `Switch`, sets label (+0x98), optional
      // description (+0xb0), and `on_change` `Rc<dyn Fn>` (+0x230..+0x240).
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
      variant: BtnVariant,
      size: BtnSize,
      action: Action,
   ) -> RawDiv {
      let title_ss = SharedString::from_str(title);
      let sub_ss = SharedString::from_str(subtitle);
      let mut btn = self.build_button(id, button_label, variant, size, action);
      // SAFETY: builds Discord's native `ActionRow(title, subtitle, button)`.
      unsafe {
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
// Sunbird Categorized Native GPUI Settings Page
// ---------------------------------------------------------------------------

const DESKTOP_FULL: &[&str] = &[
   "silent-typing",
   "nsfw-gate-bypass",
   "message-latency",
   "no-profile-themes",
   "dearrow",
   "translate",
   "clear-urls",
   "fix-embeds",
];

const DESKTOP_PARTIAL: &[&str] = &[
   "global-badges",
   "message-logger",
   "spoiler-mode",
   "fakenitro",
];

/// Renders the categorized native Sunbird Settings page into `sret`.
pub fn render_sunbird_page(sret: *mut RawAnyElement) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   let b = Builder { sym };

   ACTIONS
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .clear();

   let (tab, filter, expanded_plugins, open_select, cat_desktop, cat_partial, cat_mobile) = {
      let ui = UI_STATE.lock().unwrap_or_else(PoisonError::into_inner);
      (
         ui.tab,
         ui.filter,
         ui.expanded_plugins.clone(),
         ui.open_select.clone(),
         ui.cat_desktop_open,
         ui.cat_partial_open,
         ui.cat_mobile_open,
      )
   };

   let plugins = crate::plugins();
   let enabled_count = plugins.iter().filter(|p| p.enabled).count();
   let disabled_count = plugins.len().saturating_sub(enabled_count);

   let mut root = b.v_stack(16.0);

   // 1. Header + Category Tab Bar
   let summary = format!(
      "v{} · {enabled_count}/{} plugins active · Ctrl+Shift+I for DevTools",
      update::VERSION,
      plugins.len()
   );
   root = b.push_div(root, b.text_block("Sunbird", &summary));

   let tab_var = |t: Tab| {
      if tab == t {
         BtnVariant::Primary
      } else {
         BtnVariant::Secondary
      }
   };
   let mut tab_bar = b.h_row(8.0, false);
   tab_bar = b.push_button(
      tab_bar,
      "sunbird-tab-plugins",
      &format!("Plugins ({})", plugins.len()),
      tab_var(Tab::Plugins),
      BtnSize::Small,
      Action::SetTab(Tab::Plugins),
   );
   tab_bar = b.push_button(
      tab_bar,
      "sunbird-tab-catalog",
      "Plugin Catalog",
      tab_var(Tab::Catalog),
      BtnSize::Small,
      Action::SetTab(Tab::Catalog),
   );
   tab_bar = b.push_button(
      tab_bar,
      "sunbird-tab-general",
      "General & Updates",
      tab_var(Tab::General),
      BtnSize::Small,
      Action::SetTab(Tab::General),
   );
   root = b.push_div(root, tab_bar);
   root = b.push_div(root, b.divider());

   // 2. Active Category Content
   match tab {
      Tab::Plugins => {
         root = render_plugins_tab(
            &b,
            root,
            &plugins,
            enabled_count,
            disabled_count,
            filter,
            &expanded_plugins,
            open_select.as_ref(),
         );
      },
      Tab::Catalog => {
         let installed: HashSet<String> = plugins.iter().map(|p| p.name.clone()).collect();
         root = render_catalog_tab(&b, root, &installed, cat_desktop, cat_partial, cat_mobile);
      },
      Tab::General => {
         root = render_general_tab(&b, root);
      },
   }

   b.into_any(root, sret);
   true
}

#[allow(clippy::too_many_arguments)]
fn render_plugins_tab(
   b: &Builder,
   mut root: RawDiv,
   plugins: &[crate::PluginInfo],
   enabled_count: usize,
   disabled_count: usize,
   filter: PluginFilter,
   expanded: &HashSet<String>,
   open_select: Option<&(String, String)>,
) -> RawDiv {
   // Filter pills + Reload All toolbar
   let filt_var = |f: PluginFilter| {
      if filter == f {
         BtnVariant::Primary
      } else {
         BtnVariant::Secondary
      }
   };
   let mut left_pills = b.h_row(6.0, false);
   left_pills = b.push_button(
      left_pills,
      "sunbird-filter-all",
      &format!("All ({})", plugins.len()),
      filt_var(PluginFilter::All),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::All),
   );
   left_pills = b.push_button(
      left_pills,
      "sunbird-filter-enabled",
      &format!("Enabled ({enabled_count})"),
      filt_var(PluginFilter::Enabled),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::Enabled),
   );
   left_pills = b.push_button(
      left_pills,
      "sunbird-filter-disabled",
      &format!("Disabled ({disabled_count})"),
      filt_var(PluginFilter::Disabled),
      BtnSize::Small,
      Action::SetFilter(PluginFilter::Disabled),
   );

   let mut toolbar = b.h_row(8.0, true);
   toolbar = b.push_div(toolbar, left_pills);
   toolbar = b.push_button(
      toolbar,
      "sunbird-toolbar-reload-all",
      "Reload All",
      BtnVariant::Secondary,
      BtnSize::Small,
      Action::ReloadAll,
   );
   root = b.push_div(root, toolbar);

   let filtered: Vec<&crate::PluginInfo> = plugins
      .iter()
      .filter(|p| match filter {
         PluginFilter::All => true,
         PluginFilter::Enabled => p.enabled,
         PluginFilter::Disabled => !p.enabled,
      })
      .collect();

   if filtered.is_empty() {
      let msg = if plugins.is_empty() {
         "Open the Plugin Catalog tab to install plugins."
      } else {
         "No plugins match the current filter."
      };
      let mut empty = b.card(8.0, 16.0, 8.0, 0.18);
      empty = b.push_div(empty, b.text_block("Nothing here", msg));
      return b.push_div(root, empty);
   }

   let mut list = b.v_stack(10.0);
   for plugin in filtered {
      let is_open = expanded.contains(&plugin.name);
      let mut card = b.card(10.0, 14.0, 8.0, if plugin.enabled { 0.22 } else { 0.14 });

      let title = match &plugin.header.version {
         Some(ver) => format!("{}  v{ver}", plugin.name),
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

      card = b.push_div(
         card,
         b.switch_row(
            &format!("sunbird-plugin-{}", plugin.name),
            &title,
            (!desc.is_empty()).then_some(desc.as_str()),
            plugin.enabled,
            Action::TogglePlugin(plugin.name.clone()),
         ),
      );

      let has_settings = plugin.enabled && !plugin.settings.is_empty();
      if has_settings || plugin.installed {
         let mut actions = b.h_row(8.0, false);
         if has_settings {
            let lbl = if is_open {
               format!("▾ Hide Settings ({})", plugin.settings.len())
            } else {
               format!("▸ Settings ({})", plugin.settings.len())
            };
            actions = b.push_button(
               actions,
               &format!("sunbird-expand-{}", plugin.name),
               &lbl,
               if is_open {
                  BtnVariant::Primary
               } else {
                  BtnVariant::Secondary
               },
               BtnSize::Small,
               Action::ToggleExpandPlugin(plugin.name.clone()),
            );
         }
         if plugin.enabled && plugin.installed {
            actions = b.push_button(
               actions,
               &format!("sunbird-reload-{}", plugin.name),
               "Reload",
               BtnVariant::Secondary,
               BtnSize::Small,
               Action::ReloadPlugin(plugin.name.clone()),
            );
         }
         if !plugin.enabled && plugin.installed {
            actions = b.push_button(
               actions,
               &format!("sunbird-remove-{}", plugin.name),
               "Remove",
               BtnVariant::Critical,
               BtnSize::Small,
               Action::UninstallPlugin(plugin.name.clone()),
            );
         }
         card = b.push_div(card, actions);
      }

      // Collapsible per-plugin settings dropdown drawer
      if has_settings && is_open {
         card = b.push_div(card, b.divider());
         let mut drawer = b.card(10.0, 12.0, 6.0, 0.14);
         for setting in &plugin.settings {
            let sid = format!("sunbird-set-{}-{}", plugin.name, setting.key);
            let sdesc = setting.description.as_deref().unwrap_or("");
            match &setting.control {
               Control::Switch(val) => {
                  drawer = b.push_div(
                     drawer,
                     b.switch_row(
                        &sid,
                        &setting.title,
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
                  let cur_label = options
                     .iter()
                     .find(|o| o.value == *chosen)
                     .map_or(chosen.as_str(), |o| o.label.as_str());
                  let dropdown_open = open_select
                     .is_some_and(|(p, k)| p == &plugin.name && k == &setting.key);
                  let trigger_lbl = if dropdown_open {
                     format!("{cur_label} ▴")
                  } else {
                     format!("{cur_label} ▾")
                  };
                  drawer = b.push_div(
                     drawer,
                     b.action_row(
                        &sid,
                        &setting.title,
                        sdesc,
                        &trigger_lbl,
                        if dropdown_open {
                           BtnVariant::Primary
                        } else {
                           BtnVariant::Secondary
                        },
                        BtnSize::Small,
                        Action::ToggleSelectDropdown {
                           plugin: plugin.name.clone(),
                           key:    setting.key.clone(),
                        },
                     ),
                  );
                  if dropdown_open && !options.is_empty() {
                     let mut menu = b.card(6.0, 8.0, 6.0, 0.24);
                     for opt in options {
                        let selected = opt.value == *chosen;
                        let opt_lbl = if selected {
                           format!("✓  {}", opt.label)
                        } else {
                           opt.label.clone()
                        };
                        menu = b.push_button(
                           menu,
                           &format!("{sid}-opt-{}", opt.value),
                           &opt_lbl,
                           if selected {
                              BtnVariant::Primary
                           } else {
                              BtnVariant::Secondary
                           },
                           BtnSize::Small,
                           Action::PickSelectOption {
                              plugin: plugin.name.clone(),
                              key:    setting.key.clone(),
                              value:  opt.value.clone(),
                           },
                        );
                     }
                     drawer = b.push_div(drawer, menu);
                  }
               },
               Control::Button => {
                  drawer = b.push_div(
                     drawer,
                     b.action_row(
                        &sid,
                        &setting.title,
                        sdesc,
                        &setting.title,
                        BtnVariant::Secondary,
                        BtnSize::Small,
                        Action::PressButton {
                           plugin: plugin.name.clone(),
                           key:    setting.key.clone(),
                        },
                     ),
                  );
               },
               Control::Text(val) => {
                  let sub = if sdesc.is_empty() {
                     format!("Value: {val}")
                  } else {
                     format!("{sdesc} · Value: {val}")
                  };
                  drawer = b.push_div(drawer, b.text_block(&setting.title, &sub));
               },
               Control::Secret(val) => {
                  let state = if val.is_empty() { "not set" } else { "configured" };
                  let sub = if sdesc.is_empty() {
                     format!("Secret ({state})")
                  } else {
                     format!("{sdesc} ({state})")
                  };
                  drawer = b.push_div(drawer, b.text_block(&setting.title, &sub));
               },
               Control::Number(val) => {
                  let sub = if sdesc.is_empty() {
                     format!("Value: {val}")
                  } else {
                     format!("{sdesc} · Value: {val}")
                  };
                  drawer = b.push_div(drawer, b.text_block(&setting.title, &sub));
               },
            }
         }
         card = b.push_div(card, drawer);
      }

      list = b.push_div(list, card);
   }

   b.push_div(root, list)
}

fn render_catalog_group(
   b: &Builder,
   mut root: RawDiv,
   group_id: u8,
   title: &str,
   subtitle: &str,
   items: &[&Listed],
   installed: &HashSet<String>,
   open: bool,
) -> RawDiv {
   if items.is_empty() {
      return root;
   }
   let btn_lbl = if open {
      format!("▾ Hide ({})", items.len())
   } else {
      format!("▸ Show ({})", items.len())
   };
   let mut section = b.card(10.0, 14.0, 8.0, 0.18);
   section = b.push_div(
      section,
      b.action_row(
         &format!("sunbird-cat-group-{group_id}"),
         title,
         subtitle,
         &btn_lbl,
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::ToggleCatalogGroup(group_id),
      ),
   );
   if open {
      section = b.push_div(section, b.divider());
      let mut list = b.v_stack(8.0);
      for item in items {
         let is_installed = installed.contains(&item.name);
         let sub = match &item.warning {
            Some(w) if !w.is_empty() => format!("{} · Note: {w}", item.description),
            _ => item.description.clone(),
         };
         let mut row_card = b.card(6.0, 12.0, 6.0, 0.14);
         row_card = b.push_div(
            row_card,
            b.action_row(
               &format!("sunbird-cat-{}", item.name),
               &item.name,
               &sub,
               if is_installed { "Reinstall" } else { "Install" },
               if is_installed {
                  BtnVariant::Secondary
               } else {
                  BtnVariant::Primary
               },
               BtnSize::Small,
               Action::InstallCatalog(item.name.clone()),
            ),
         );
         list = b.push_div(list, row_card);
      }
      section = b.push_div(section, list);
   }
   root = b.push_div(root, section);
   root
}

fn render_catalog_tab(
   b: &Builder,
   mut root: RawDiv,
   installed: &HashSet<String>,
   desktop_open: bool,
   partial_open: bool,
   mobile_open: bool,
) -> RawDiv {
   root = b.push_div(
      root,
      b.action_row(
         "sunbird-catalog-refresh",
         "Official Plugin Catalog",
         "Install plugins directly from assets.sunbird.tools.",
         "Refresh",
         BtnVariant::Secondary,
         BtnSize::Small,
         Action::RefreshCatalog,
      ),
   );

   let catalog = update::catalog();
   match catalog {
      Catalog::Unfetched => {
         update::fetch_catalog();
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.18);
         box_div = b.push_div(
            box_div,
            b.text_block(
               "Fetching catalog...",
               "Loading official plugins from assets.sunbird.tools.",
            ),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Loading => {
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.18);
         box_div = b.push_div(
            box_div,
            b.text_block(
               "Fetching catalog...",
               "Loading official plugins from assets.sunbird.tools.",
            ),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Failed(err) => {
         let mut box_div = b.card(8.0, 14.0, 8.0, 0.22);
         box_div = b.push_div(
            box_div,
            b.action_row(
               "sunbird-catalog-retry",
               "Could not reach catalog",
               &err,
               "Retry",
               BtnVariant::Primary,
               BtnSize::Small,
               Action::RefreshCatalog,
            ),
         );
         root = b.push_div(root, box_div);
      },
      Catalog::Loaded(items) => {
         let desktop: Vec<&Listed> = items
            .iter()
            .filter(|i| DESKTOP_FULL.contains(&i.name.as_str()))
            .collect();
         let partial: Vec<&Listed> = items
            .iter()
            .filter(|i| DESKTOP_PARTIAL.contains(&i.name.as_str()))
            .collect();
         let mobile: Vec<&Listed> = items
            .iter()
            .filter(|i| {
               !DESKTOP_FULL.contains(&i.name.as_str())
                  && !DESKTOP_PARTIAL.contains(&i.name.as_str())
            })
            .collect();

         root = render_catalog_group(
            b,
            root,
            0,
            "Desktop Ready",
            "Fully supported on Desktop Phoenix.",
            &desktop,
            installed,
            desktop_open,
         );
         root = render_catalog_group(
            b,
            root,
            1,
            "Partial Desktop Support",
            "Core hooks work on desktop; some mobile UI badges/accessories are pending.",
            &partial,
            installed,
            partial_open,
         );
         root = render_catalog_group(
            b,
            root,
            2,
            "Mobile UI Plugins",
            "Primarily target Android Phoenix's UI tree.",
            &mobile,
            installed,
            mobile_open,
         );
      },
   }
   root
}

fn render_general_tab(b: &Builder, mut root: RawDiv) -> RawDiv {
   // Card 1: Discord & Developer controls
   let mut dev_card = b.card(12.0, 16.0, 8.0, 0.20);
   dev_card = b.push_div(
      dev_card,
      b.switch_row(
         "sunbird-staff-unlock",
         "Staff Unlock",
         Some("Unlock Discord staff-only settings, experiments, and internal developer menus."),
         crate::staff_enabled(),
         Action::ToggleStaff,
      ),
   );
   dev_card = b.push_div(dev_card, b.divider());
   dev_card = b.push_div(
      dev_card,
      b.action_row(
         "sunbird-open-devtools",
         "Runtime Console & DevTools",
         "Evaluate JavaScript in QuickJS and view live runtime logs (Ctrl+Shift+I).",
         "Open DevTools",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::OpenDevTools,
      ),
   );
   dev_card = b.push_div(dev_card, b.divider());
   dev_card = b.push_div(
      dev_card,
      b.action_row(
         "sunbird-reload-all",
         "Reload All Plugins",
         "Re-evaluate every currently enabled plugin without restarting Discord.",
         "Reload All",
         BtnVariant::Secondary,
         BtnSize::Medium,
         Action::ReloadAll,
      ),
   );
   root = b.push_div(root, dev_card);

   // Card 2: Updates
   let upd_status = update::status();
   let upd_found = update::found();
   let (upd_desc, upd_btn, upd_var, upd_act) = if update::busy() {
      (
         upd_status.unwrap_or_else(|| "Checking for updates...".to_owned()),
         "Working...",
         BtnVariant::Secondary,
         Action::CheckUpdates,
      )
   } else if let Some(found) = upd_found
      && !found.plugins.is_empty()
   {
      (
         format!("Plugin updates ready: {}", found.plugins.join(", ")),
         "Apply Updates",
         BtnVariant::Primary,
         Action::ApplyUpdates,
      )
   } else {
      (
         upd_status.unwrap_or_else(|| format!("Sunbird {} is up to date.", update::VERSION)),
         "Check for Updates",
         BtnVariant::Secondary,
         Action::CheckUpdates,
      )
   };

   let mut upd_card = b.card(10.0, 16.0, 8.0, 0.20);
   upd_card = b.push_div(
      upd_card,
      b.action_row(
         "sunbird-updates",
         "Sunbird Updates",
         &upd_desc,
         upd_btn,
         upd_var,
         BtnSize::Medium,
         upd_act,
      ),
   );
   b.push_div(root, upd_card)
}
