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
use phoenix_sigs::desktop::{
   self,
   layout,
};

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
struct RawDiv([u8; layout::DIV]);

#[repr(C, align(8))]
pub struct RawAnyElement([u8; layout::ANY_ELEMENT]);

#[repr(C, align(8))]
struct RawSwitch([u8; layout::SWITCH]);

#[repr(C, align(8))]
struct RawButton([u8; layout::BUTTON]);

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
type DivPushAnyFn = unsafe extern "C" fn(*mut c_void, *mut RawAnyElement);
type StrIntoAnyFn =
   unsafe extern "C" fn(*mut RawAnyElement, *mut SharedString) -> *mut RawAnyElement;
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
   div_push_any:     Option<DivPushAnyFn>,
   str_into_any:     Option<StrIntoAnyFn>,
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

/// Resolves Discord's native GPUI / Mana widget builders from unique string
/// anchors. Safe to call from `DllMain` (`prepare()`).
pub fn locate(image: &Image<'static>) -> bool {
   if SYMBOLS.get().is_some() {
      return true;
   }

   let found = desktop::gpui(image).and_then(|gpui| {
      desktop::gpui_layouts(image, &gpui)?;
      Ok(gpui)
   });
   let desktop::Gpui {
      caller_loc,
      div_new,
      div_flex,
      div_flex_col,
      div_style_mut,
      div_child_div,
      div_child_button,
      div_into_any,
      div_push_any,
      str_into_any,
      text_block,
      switch_new,
      switch_into_div,
      button_new,
      action_row,
      alloc_or_oom,
   } = match found {
      Ok(gpui) => gpui,
      Err(err) => {
         log::warn!("desktop gpui: {err}, leaving the settings page and accessories off");
         return false;
      },
   };

   // SAFETY: the builders were found by their call order and cross-checked,
   // their layouts match `layout`, and they have these Rust-ABI signatures.
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
         div_push_any:     div_push_any
            .map(|addr| std::mem::transmute::<usize, DivPushAnyFn>(addr)),
         str_into_any:     str_into_any
            .map(|addr| std::mem::transmute::<usize, StrIntoAnyFn>(addr)),
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
       switch={switch_new:#x}, button={button_new:#x}, row={action_row:#x}, \
       push_any={div_push_any:#x?}, str_any={str_into_any:#x?})"
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
         base.add(layout::BUTTON_VARIANT).write(variant as u8);
         base.add(layout::BUTTON_SIZE).write(size as u8);
         let rc_ptr = self.alloc_rc_action(action);
         base.add(layout::BUTTON_ON_CLICK).cast::<*mut RcInner>().write(rc_ptr);
         base.add(layout::BUTTON_ON_CLICK + 8)
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
         base.add(layout::SWITCH_ON_CHANGE).cast::<*mut RcInner>().write(rc_ptr);
         base.add(layout::SWITCH_ON_CHANGE + 8)
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

   fn push_any(&self, parent: &mut RawDiv, child: &mut RawAnyElement) -> bool {
      let Some(push) = self.sym.div_push_any else {
         return false;
      };
      // SAFETY: `RawDiv + 0x40` is `SmallVec<[AnyElement; 2]>` (verified inside `div_child_div`).
      unsafe {
         let children = parent.0.as_mut_ptr().add(0x40).cast::<c_void>();
         push(children, child);
      }
      true
   }

   fn text_line(&self, text: &str, color_hsla: [f32; 4], font_px: f32) -> Option<RawDiv> {
      let str_into_any = self.sym.str_into_any?;
      let mut line_div = self.raw_div();
      // SAFETY: verified StyleRefinement offsets:
      // +0x58 = text.font_size (AbsoluteLength::Pixels),
      // +0x88 = text.color (Option<Hsla>: u32 tag=1, [f32; 4] at +0x8c).
      self.style_div(&mut line_div, |s| unsafe {
         s.add(0x58).cast::<u64>().write(gpui_px(font_px));
         s.add(0x88).cast::<u32>().write(1);
         s.add(0x8c).cast::<[f32; 4]>().write(color_hsla);
      });
      let mut ss = SharedString::from_str(text);
      // SAFETY: converts static `SharedString` into a 32-byte `AnyElement` and pushes it.
      let mut any = unsafe {
         let mut out = MaybeUninit::<RawAnyElement>::uninit();
         str_into_any(out.as_mut_ptr(), &raw mut ss);
         out.assume_init()
      };
      if !self.push_any(&mut line_div, &mut any) {
         return None;
      }
      Some(line_div)
   }
}

/// Converts 32-bit ARGB (`0xAARRGGBB`) into GPUI's `[h, s, l, a]` (`0.0..=1.0`).
fn argb_to_hsla(argb: u32) -> [f32; 4] {
   let raw_a = ((argb >> 24) & 0xFF) as f32 / 255.0;
   let a = if raw_a == 0.0 && argb != 0 { 1.0 } else { raw_a };
   let r = ((argb >> 16) & 0xFF) as f32 / 255.0;
   let g = ((argb >> 8) & 0xFF) as f32 / 255.0;
   let b = (argb & 0xFF) as f32 / 255.0;

   let max = r.max(g).max(b);
   let min = r.min(g).min(b);
   let delta = max - min;
   let l = f32::midpoint(max, min);

   let (h, s) = if delta == 0.0 {
      (0.0, 0.0)
   } else {
      let s = delta / (1.0 - (2.0 * l - 1.0).abs());
      let mut h = if (max - r).abs() < f32::EPSILON {
         ((g - b) / delta) % 6.0
      } else if (max - g).abs() < f32::EPSILON {
         (b - r) / delta + 2.0
      } else {
         (r - g) / delta + 4.0
      };
      h /= 6.0;
      if h < 0.0 {
         h += 1.0;
      }
      (h, s)
   };
   [h, s, l, a]
}

/// Whether `div_push_any` and `str_into_any` resolved for native message accessories.
#[must_use]
pub fn supports_accessories() -> bool {
   SYMBOLS
      .get()
      .is_some_and(|s| s.div_push_any.is_some() && s.str_into_any.is_some())
}

/// Wraps a rendered message row `RawAnyElement` in place with native GPUI
/// accessory lines underneath and an optional tinted left accent bar.
pub fn wrap_with_accessories(
   sret: *mut RawAnyElement,
   lines: &[(String, Option<u32>)],
   tint: Option<u32>,
) -> bool {
   let Some(sym) = SYMBOLS.get() else {
      return false;
   };
   if sym.div_push_any.is_none() || sym.str_into_any.is_none() {
      return false;
   }
   let b = Builder { sym };

   // SAFETY: takes ownership of the original rendered `AnyElement` out of `sret`;
   // we write the replacement `AnyElement` back into `sret` before returning.
   let mut orig = unsafe { ptr::read(sret) };

   let mut outer = b.v_stack(2.0);
   if let Some(argb) = tint {
      let hsla = argb_to_hsla(argb);
      // SAFETY: verified StyleRefinement offsets:
      // +0x88 = text.color (Option<Hsla>, inherited by child text runs),
      // +0x128 = border_left, +0x250 = border_color.
      b.style_div(&mut outer, |s| unsafe {
         s.add(0x88).cast::<u32>().write(1);
         s.add(0x8c).cast::<[f32; 4]>().write(hsla);
         s.add(0x128).cast::<u64>().write(gpui_px(3.0));
         s.add(0x250).cast::<u32>().write(1);
         s.add(0x254).cast::<[f32; 4]>().write(hsla);
      });
   }
   if !b.push_any(&mut outer, &mut orig) {
      // SAFETY: restore original element if push failed.
      unsafe { ptr::write(sret, orig) };
      return false;
   }

   if !lines.is_empty() {
      let mut acc_box = b.v_stack(2.0);
      // SAFETY: indent accessories under the message text column (left=72px, bottom=4px, right=16px).
      b.style_div(&mut acc_box, |s| unsafe {
         s.add(0x1a0).cast::<u64>().write(gpui_px(16.0)); // padding_right
         s.add(0x1a8).cast::<u64>().write(gpui_px(4.0)); // padding_bottom
         s.add(0x1b0).cast::<u64>().write(gpui_px(72.0)); // padding_left
      });
      const MUTED: [f32; 4] = [0.60, 0.08, 0.62, 0.85];
      for (text, color) in lines {
         let hsla = color.map_or(MUTED, argb_to_hsla);
         for subline in text.lines() {
            if subline.is_empty() {
               continue;
            }
            if let Some(line_div) = b.text_line(subline, hsla, 12.0) {
               acc_box = b.push_div(acc_box, line_div);
            }
         }
      }
      outer = b.push_div(outer, acc_box);
   }

   b.into_any(outer, sret);
   true
}

// ---------------------------------------------------------------------------
// Sunbird Categorized Native GPUI Settings Page
// ---------------------------------------------------------------------------

const DESKTOP_FULL: &[&str] = &[
   "silent-typing",
   "nsfw-gate-bypass",
   "message-latency",
   "message-logger",
   "no-profile-themes",
   "spoiler-mode",
   "dearrow",
   "translate",
   "clear-urls",
   "fix-embeds",
];

const DESKTOP_PARTIAL: &[&str] = &[
   "global-badges",
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
