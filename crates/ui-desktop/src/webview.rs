//! Embedded WebView2 host and Discord main-window subclass for Desktop Phoenix.
//!
//! Responsibilities:
//! 1. Subclass Discord's main GPUI `HWND` to handle `Ctrl+Shift+I` (opens the
//!    Sunbird DevTools / Console window without registering a global hotkey
//!    that would steal `Ctrl+Shift+I` from browsers or editors) and `Escape` /
//!    top-right close clicks while Sunbird Settings is open.
//! 2. Lazily host Microsoft Edge WebView2 (`EmbeddedBrowserWebView.dll`) as a
//!    child `HWND` positioned over Discord's User Settings content pane when
//!    the user selects the injected **Sunbird** tab in Discord Settings.
//! 3. Gracefully fall back to the standalone Edge `--app` dashboard window if
//!    the WebView2 Runtime is unavailable on the machine.

use std::{
   cell::RefCell,
   ffi::c_void,
   path::{
      Path,
      PathBuf,
   },
   ptr,
   sync::atomic::{
      AtomicBool,
      AtomicU32,
      AtomicUsize,
      Ordering,
   },
   thread,
   time::Duration,
};

use crate::windows;

const WM_MOVE: u32 = 0x0003;
const WM_SIZE: u32 = 0x0005;
const WM_SHOWWINDOW: u32 = 0x0018;
const WM_WINDOWPOSCHANGED: u32 = 0x0047;
const WM_NCDESTROY: u32 = 0x0082;
const WM_KEYDOWN: u32 = 0x0100;
const WM_KEYUP: u32 = 0x0101;
const WM_SYSKEYDOWN: u32 = 0x0104;
const WM_LBUTTONUP: u32 = 0x0202;
const WM_APP: u32 = 0x8000;
const WM_APP_SYNC_SETTINGS: u32 = WM_APP + 0x42;
const WM_APP_CLOSE_SETTINGS: u32 = WM_APP + 0x43;

const SIZE_MINIMIZED: usize = 1;
const VK_ESCAPE: usize = 0x1B;
const VK_CONTROL: i32 = 0x11;
const VK_SHIFT: i32 = 0x10;
const VK_MENU: i32 = 0x12;

const WS_POPUP: u32 = 0x8000_0000;
const WS_CLIPCHILDREN: u32 = 0x0200_0000;
const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
const SW_HIDE: i32 = 0;
const SW_SHOWNOACTIVATE: i32 = 4;
const SWP_NOACTIVATE: u32 = 0x0010;
const SWP_NOZORDER: u32 = 0x0004;

const SUBCLASS_ID: usize = 0x5342_5756; // "SBWV"
const E_NOINTERFACE: i32 = -2_147_467_262; // 0x80004002
const S_OK: i32 = 0;

static WATCHER_STARTED: AtomicBool = AtomicBool::new(false);
static DISCORD_HWND: AtomicUsize = AtomicUsize::new(0);
static DESIRED_VISIBLE: AtomicBool = AtomicBool::new(false);
static FALLBACK_OPENED: AtomicBool = AtomicBool::new(false);

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
   data1: u32,
   data2: u16,
   data3: u16,
   data4: [u8; 8],
}

/// `ICoreWebView2Controller2` (`c979903e-d4ca-4228-92eb-47ee3fa96eab`).
static IID_CONTROLLER2: Guid = Guid {
   data1: 0xc979_903e,
   data2: 0xd4ca,
   data3: 0x4228,
   data4: [0x92, 0xeb, 0x47, 0xee, 0x3f, 0xa9, 0x6e, 0xab],
};

#[repr(C)]
#[derive(Clone, Copy)]
struct Point {
   x: i32,
   y: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
   left:   i32,
   top:    i32,
   right:  i32,
   bottom: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Color {
   a: u8,
   r: u8,
   g: u8,
   b: u8,
}

#[repr(C)]
struct WndClassExW {
   cb_size:    u32,
   style:      u32,
   wnd_proc:   unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
   cls_extra:  i32,
   wnd_extra:  i32,
   instance:   *mut c_void,
   icon:       *mut c_void,
   cursor:     *mut c_void,
   background: *mut c_void,
   menu_name:  *const u16,
   class_name: *const u16,
   icon_sm:    *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn EnumWindows(
      cb: unsafe extern "system" fn(*mut c_void, isize) -> i32,
      lparam: isize,
   ) -> i32;
   fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
   fn IsWindow(hwnd: *mut c_void) -> i32;
   fn IsWindowVisible(hwnd: *mut c_void) -> i32;
   fn GetClassNameW(hwnd: *mut c_void, buf: *mut u16, max: i32) -> i32;
   fn GetClientRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
   fn ClientToScreen(hwnd: *mut c_void, pt: *mut Point) -> i32;
   fn IsIconic(hwnd: *mut c_void) -> i32;
   fn GetKeyState(vk: i32) -> i16;
   fn PostMessageW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
   fn RegisterClassExW(cls: *const WndClassExW) -> u16;
   fn CreateWindowExW(
      ex_style: u32,
      class_name: *const u16,
      window_name: *const u16,
      style: u32,
      x: i32,
      y: i32,
      width: i32,
      height: i32,
      parent: *mut c_void,
      menu: *mut c_void,
      instance: *mut c_void,
      param: *mut c_void,
   ) -> *mut c_void;
   fn DestroyWindow(hwnd: *mut c_void) -> i32;
   fn ShowWindow(hwnd: *mut c_void, cmd: i32) -> i32;
   fn SetWindowPos(
      hwnd: *mut c_void,
      insert_after: *mut c_void,
      x: i32,
      y: i32,
      cx: i32,
      cy: i32,
      flags: u32,
   ) -> i32;
   fn SetFocus(hwnd: *mut c_void) -> *mut c_void;
   fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
   fn GetDpiForWindow(hwnd: *mut c_void) -> u32;
}

#[link(name = "comctl32")]
unsafe extern "system" {
   fn SetWindowSubclass(
      hwnd: *mut c_void,
      proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize,
      id: usize,
      ref_data: usize,
   ) -> i32;
   fn RemoveWindowSubclass(
      hwnd: *mut c_void,
      proc: unsafe extern "system" fn(*mut c_void, u32, usize, isize, usize, usize) -> isize,
      id: usize,
   ) -> i32;
   fn DefSubclassProc(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GetCurrentProcessId() -> u32;
   fn GetModuleHandleW(name: *const u16) -> *mut c_void;
   fn LoadLibraryW(path: *const u16) -> *mut c_void;
   fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

#[link(name = "gdi32")]
unsafe extern "system" {
   fn CreateSolidBrush(color: u32) -> *mut c_void;
}

// ---------------------------------------------------------------------------
// Minimal WebView2 COM vtables (verified against webview2-com-sys 0.39.1)
// ---------------------------------------------------------------------------

type CreateWebViewEnvInternalFn = unsafe extern "system" fn(
   check_thread: u8,
   runtime_type: i32,
   user_data_folder: *const u16,
   environment_options: *mut c_void,
   handler: *mut c_void,
) -> i32;

#[repr(C)]
struct IUnknownVtbl {
   query_interface: unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32,
   add_ref:         unsafe extern "system" fn(*mut c_void) -> u32,
   release:         unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct EnvVtbl {
   base:               IUnknownVtbl,
   create_controller:  unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
}

#[repr(C)]
struct ControllerVtbl {
   base:                               IUnknownVtbl,
   get_is_visible:                     unsafe extern "system" fn(*mut c_void, *mut i32) -> i32,
   put_is_visible:                     unsafe extern "system" fn(*mut c_void, i32) -> i32,
   get_bounds:                         unsafe extern "system" fn(*mut c_void, *mut Rect) -> i32,
   put_bounds:                         unsafe extern "system" fn(*mut c_void, Rect) -> i32,
   _get_zoom_factor:                   *const c_void,
   _put_zoom_factor:                   *const c_void,
   _add_zoom_factor_changed:           *const c_void,
   _remove_zoom_factor_changed:        *const c_void,
   _set_bounds_and_zoom_factor:        *const c_void,
   _move_focus:                        *const c_void,
   _add_move_focus_requested:          *const c_void,
   _remove_move_focus_requested:       *const c_void,
   _add_got_focus:                     *const c_void,
   _remove_got_focus:                  *const c_void,
   _add_lost_focus:                    *const c_void,
   _remove_lost_focus:                 *const c_void,
   _add_accelerator_key_pressed:       *const c_void,
   _remove_accelerator_key_pressed:    *const c_void,
   _get_parent_window:                 *const c_void,
   _put_parent_window:                 *const c_void,
   notify_parent_window_pos_changed:   unsafe extern "system" fn(*mut c_void) -> i32,
   close:                              unsafe extern "system" fn(*mut c_void) -> i32,
   get_core_webview2:                  unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> i32,
}

#[repr(C)]
struct Controller2Vtbl {
   base:                      ControllerVtbl,
   _get_default_bg_color:     *const c_void,
   put_default_bg_color:      unsafe extern "system" fn(*mut c_void, Color) -> i32,
}

#[repr(C)]
struct WebViewVtbl {
   base:         IUnknownVtbl,
   _get_settings: *const c_void,
   _get_source:   *const c_void,
   navigate:      unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
}

#[repr(C)]
struct CompletedHandlerVtbl {
   base:   IUnknownVtbl,
   invoke: unsafe extern "system" fn(*mut c_void, i32, *mut c_void) -> i32,
}

#[repr(C)]
struct ComCallback {
   vtbl:      *const CompletedHandlerVtbl,
   ref_count: AtomicU32,
}

unsafe fn com_release(obj: *mut c_void) {
   if obj.is_null() {
      return;
   }
   // SAFETY: valid COM pointer with IUnknown vtable at offset 0.
   unsafe {
      let vtbl = *(obj.cast::<*const IUnknownVtbl>());
      ((*vtbl).release)(obj);
   }
}

unsafe fn com_add_ref(obj: *mut c_void) {
   if obj.is_null() {
      return;
   }
   // SAFETY: valid COM pointer with IUnknown vtable at offset 0.
   unsafe {
      let vtbl = *(obj.cast::<*const IUnknownVtbl>());
      ((*vtbl).add_ref)(obj);
   }
}

unsafe extern "system" fn cb_query_interface(
   this: *mut c_void,
   _riid: *const Guid,
   out: *mut *mut c_void,
) -> i32 {
   if out.is_null() {
      return E_NOINTERFACE;
   }
   // SAFETY: `this` is a heap-allocated `ComCallback`.
   unsafe {
      *out = this;
      cb_add_ref(this);
   }
   S_OK
}

unsafe extern "system" fn cb_add_ref(this: *mut c_void) -> u32 {
   // SAFETY: `this` is a heap-allocated `ComCallback`.
   unsafe { (*this.cast::<ComCallback>()).ref_count.fetch_add(1, Ordering::Relaxed) + 1 }
}

unsafe extern "system" fn cb_release(this: *mut c_void) -> u32 {
   // SAFETY: `this` is a heap-allocated `ComCallback` created by `Box::into_raw`.
   unsafe {
      let prev = (*this.cast::<ComCallback>()).ref_count.fetch_sub(1, Ordering::AcqRel);
      if prev == 1 {
         drop(Box::from_raw(this.cast::<ComCallback>()));
         0
      } else {
         prev - 1
      }
   }
}

static ENV_HANDLER_VTBL: CompletedHandlerVtbl = CompletedHandlerVtbl {
   base:   IUnknownVtbl {
      query_interface: cb_query_interface,
      add_ref:         cb_add_ref,
      release:         cb_release,
   },
   invoke: on_env_created,
};

static CTRL_HANDLER_VTBL: CompletedHandlerVtbl = CompletedHandlerVtbl {
   base:   IUnknownVtbl {
      query_interface: cb_query_interface,
      add_ref:         cb_add_ref,
      release:         cb_release,
   },
   invoke: on_controller_created,
};

fn new_callback(vtbl: &'static CompletedHandlerVtbl) -> *mut c_void {
   Box::into_raw(Box::new(ComCallback {
      vtbl:      ptr::from_ref(vtbl),
      ref_count: AtomicU32::new(1),
   }))
   .cast()
}

// ---------------------------------------------------------------------------
// Thread-local state (lives exclusively on Discord's UI thread)
// ---------------------------------------------------------------------------

struct EmbeddedHost {
   host_hwnd:    *mut c_void,
   env:          *mut c_void,
   controller:   *mut c_void,
   webview:      *mut c_void,
   initializing: bool,
   failed:       bool,
}

impl EmbeddedHost {
   const fn new() -> Self {
      Self {
         host_hwnd:    ptr::null_mut(),
         env:          ptr::null_mut(),
         controller:   ptr::null_mut(),
         webview:      ptr::null_mut(),
         initializing: false,
         failed:       false,
      }
   }
}

thread_local! {
   static HOST: RefCell<EmbeddedHost> = const { RefCell::new(EmbeddedHost::new()) };
}

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Computes the embedded Settings content-pane rectangle in **client**
/// coordinates of `discord_hwnd`.
unsafe fn compute_client_pane_rect(discord_hwnd: *mut c_void) -> (Rect, f64) {
   let mut client = Rect {
      left:   0,
      top:    0,
      right:  0,
      bottom: 0,
   };
   // SAFETY: `discord_hwnd` is a valid HWND.
   unsafe { GetClientRect(discord_hwnd, &raw mut client) };
   let dpi = unsafe { GetDpiForWindow(discord_hwnd) };
   let scale = if dpi == 0 { 1.0 } else { f64::from(dpi) / 96.0 };

   let total_w = f64::from(client.right - client.left) / scale;
   let total_h = f64::from(client.bottom - client.top) / scale;

   // Discord centers a ~1060px container (sidebar ~240 + content 740 + close ~80)
   // on wide windows, and pins the sidebar at ~240px on narrower windows.
   let extra = ((total_w - 1060.0) * 0.5).max(0.0);
   let left_dip = (extra + 244.0).clamp(236.0, (total_w * 0.42).max(236.0));
   let right_margin_dip = (extra + 78.0).clamp(78.0, (total_w * 0.28).max(78.0));
   let top_dip = 38.0;
   let bottom_dip = (total_h - 14.0).max(top_dip + 120.0);
   let right_dip = (total_w - right_margin_dip).max(left_dip + 320.0);

   #[expect(clippy::cast_possible_truncation, reason = "screen pixel coordinates fit i32")]
   let px = |dip: f64| (dip * scale).round() as i32;
   let local = Rect {
      left:   px(left_dip),
      top:    px(top_dip),
      right:  px(right_dip).min(client.right),
      bottom: px(bottom_dip).min(client.bottom),
   };
   (local, scale)
}

/// Computes the embedded Settings content-pane rectangle in **screen**
/// coordinates, aligned inside Discord's client area so Discord's native
/// titlebar (top), Settings sidebar (left), and `ESC` close circle (top-right)
/// stay visible and clickable. Using an owned `WS_POPUP` in screen coordinates
/// ensures WebView2 renders above GPUI's DirectComposition swapchain.
unsafe fn compute_screen_pane_rect(discord_hwnd: *mut c_void) -> Rect {
   // SAFETY: `discord_hwnd` is a valid HWND.
   let (local, _) = unsafe { compute_client_pane_rect(discord_hwnd) };
   let mut origin = Point {
      x: local.left,
      y: local.top,
   };
   // SAFETY: `discord_hwnd` is a valid HWND.
   unsafe { ClientToScreen(discord_hwnd, &raw mut origin) };
   let w = (local.right - local.left).max(1);
   let h = (local.bottom - local.top).max(1);
   Rect {
      left:   origin.x,
      top:    origin.y,
      right:  origin.x + w,
      bottom: origin.y + h,
   }
}

fn find_webview2_dll() -> Option<PathBuf> {
   let mut roots = vec![
      PathBuf::from(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application"),
      PathBuf::from(r"C:\Program Files\Microsoft\EdgeWebView\Application"),
   ];
   if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
      roots.push(
         Path::new(&local_app)
            .join("Microsoft")
            .join("EdgeWebView")
            .join("Application"),
      );
   }
   let mut candidates: Vec<(Vec<u32>, PathBuf)> = Vec::new();
   for root in roots {
      let Ok(entries) = std::fs::read_dir(&root) else {
         continue;
      };
      for entry in entries.flatten() {
         let name = entry.file_name();
         let name = name.to_string_lossy();
         if !name.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
            continue;
         }
         let dll = entry
            .path()
            .join("EBWebView")
            .join("x64")
            .join("EmbeddedBrowserWebView.dll");
         if dll.is_file() {
            let ver = name
               .split('.')
               .map(|part| part.parse::<u32>().unwrap_or(0))
               .collect::<Vec<_>>();
            candidates.push((ver, dll));
         }
      }
   }
   candidates.sort_by(|a, b| b.0.cmp(&a.0));
   candidates.into_iter().next().map(|(_, path)| path)
}

unsafe fn load_create_env_fn() -> Option<CreateWebViewEnvInternalFn> {
   let dll_path = find_webview2_dll()?;
   log::info!("webview2: loading {}", dll_path.display());
   let wide_path = wide(&dll_path.to_string_lossy());
   // SAFETY: NUL-terminated DLL path and ASCII export name.
   unsafe {
      let module = LoadLibraryW(wide_path.as_ptr());
      if module.is_null() {
         log::warn!(
            "webview2: LoadLibraryW failed: {}",
            std::io::Error::last_os_error()
         );
         return None;
      }
      let proc = GetProcAddress(
         module,
         b"CreateWebViewEnvironmentWithOptionsInternal\0".as_ptr(),
      );
      if proc.is_null() {
         log::warn!("webview2: CreateWebViewEnvironmentWithOptionsInternal export missing");
         return None;
      }
      Some(std::mem::transmute::<*mut c_void, CreateWebViewEnvInternalFn>(proc))
   }
}

unsafe extern "system" fn host_wnd_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
) -> isize {
   // SAFETY: standard DefWindowProcW delegation.
   unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe fn ensure_host_hwnd(discord_hwnd: *mut c_void) -> *mut c_void {
   let existing = HOST.with(|h| h.borrow().host_hwnd);
   if !existing.is_null() {
      return existing;
   }
   let class_name = wide("SunbirdSettingsHost");
   let title = wide("Sunbird Settings");
   // SAFETY: Win32 child window registration and creation on Discord's UI thread.
   unsafe {
      let instance = GetModuleHandleW(ptr::null());
      // Dark Discord settings surface (#1a1a1e -> COLORREF 0x001E1A1A).
      let bg_brush = CreateSolidBrush(0x001E_1A1A);
      #[expect(clippy::cast_possible_truncation, reason = "WNDCLASSEXW is 80 bytes")]
      let cb_size = size_of::<WndClassExW>() as u32;
      let cls = WndClassExW {
         cb_size,
         style: 0,
         wnd_proc: host_wnd_proc,
         cls_extra: 0,
         wnd_extra: 0,
         instance,
         icon: ptr::null_mut(),
         cursor: ptr::null_mut(),
         background: bg_brush,
         menu_name: ptr::null(),
         class_name: class_name.as_ptr(),
         icon_sm: ptr::null_mut(),
      };
      RegisterClassExW(&raw const cls);

      let rect = compute_screen_pane_rect(discord_hwnd);
      // Owned WS_POPUP with WS_EX_TOOLWINDOW composites cleanly above GPUI's
      // DirectComposition swapchain while staying bound to Discord's Z-order
      // (never topmost over other apps, never shown in Alt+Tab or taskbar).
      let hwnd = CreateWindowExW(
         WS_EX_TOOLWINDOW,
         class_name.as_ptr(),
         title.as_ptr(),
         WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
         rect.left,
         rect.top,
         (rect.right - rect.left).max(1),
         (rect.bottom - rect.top).max(1),
         discord_hwnd,
         ptr::null_mut(),
         instance,
         ptr::null_mut(),
      );
      log::info!(
         "webview2: created host popup {hwnd:p} at ({}, {}) {}x{}",
         rect.left,
         rect.top,
         rect.right - rect.left,
         rect.bottom - rect.top
      );
      HOST.with(|h| h.borrow_mut().host_hwnd = hwnd);
      hwnd
   }
}

unsafe fn update_host_bounds(discord_hwnd: *mut c_void) {
   let (host_hwnd, controller) = HOST.with(|h| {
      let h = h.borrow();
      (h.host_hwnd, h.controller)
   });
   if host_hwnd.is_null() {
      return;
   }
   // SAFETY: called on Discord's UI thread with live HWNDs and COM controller.
   unsafe {
      let rect = compute_screen_pane_rect(discord_hwnd);
      let w = (rect.right - rect.left).max(1);
      let h = (rect.bottom - rect.top).max(1);
      SetWindowPos(
         host_hwnd,
         ptr::null_mut(),
         rect.left,
         rect.top,
         w,
         h,
         SWP_NOACTIVATE | SWP_NOZORDER,
      );
      if !controller.is_null() {
         let vtbl = *(controller.cast::<*const ControllerVtbl>());
         let hr = ((*vtbl).put_bounds)(
            controller,
            Rect {
               left:   0,
               top:    0,
               right:  w,
               bottom: h,
            },
         );
         if hr < 0 {
            log::warn!("webview2: put_bounds failed: {hr:#010x}");
         }
      }
   }
}

unsafe fn start_webview2_init(discord_hwnd: *mut c_void) {
   let should_start = HOST.with(|h| {
      let mut h = h.borrow_mut();
      if h.initializing || !h.controller.is_null() || h.failed {
         return false;
      }
      h.initializing = true;
      true
   });
   if !should_start {
      return;
   }
   // SAFETY: runs on Discord's UI thread; callbacks execute on the same STA thread.
   unsafe {
      let host_hwnd = ensure_host_hwnd(discord_hwnd);
      if host_hwnd.is_null() {
         mark_failed();
         return;
      }
      let Some(create_env) = load_create_env_fn() else {
         mark_failed();
         return;
      };
      let data_dir = windows::webview_data_dir();
      let _ = std::fs::create_dir_all(&data_dir);
      let wide_dir = wide(&data_dir.to_string_lossy());
      let handler = new_callback(&ENV_HANDLER_VTBL);
      let hr = create_env(1, 0, wide_dir.as_ptr(), ptr::null_mut(), handler);
      com_release(handler);
      if hr < 0 {
         log::warn!("webview2: CreateWebViewEnvironmentWithOptionsInternal failed: {hr:#010x}");
         mark_failed();
      }
   }
}

fn mark_failed() {
   HOST.with(|h| {
      let mut h = h.borrow_mut();
      h.initializing = false;
      h.failed = true;
      if !h.host_hwnd.is_null() {
         // SAFETY: hides the empty child host before falling back.
         unsafe { ShowWindow(h.host_hwnd, SW_HIDE) };
      }
   });
   if DESIRED_VISIBLE.load(Ordering::Acquire) && !FALLBACK_OPENED.swap(true, Ordering::AcqRel) {
      windows::open_dashboard();
   }
}

unsafe extern "system" fn on_env_created(
   _this: *mut c_void,
   hr: i32,
   env: *mut c_void,
) -> i32 {
   if hr < 0 || env.is_null() {
      log::warn!("webview2: environment creation failed: {hr:#010x}");
      mark_failed();
      return S_OK;
   }
   let host_hwnd = HOST.with(|h| h.borrow().host_hwnd);
   if host_hwnd.is_null() {
      mark_failed();
      return S_OK;
   }
   // SAFETY: `env` is a valid `ICoreWebView2Environment` on Discord's UI thread.
   unsafe {
      com_add_ref(env);
      HOST.with(|h| h.borrow_mut().env = env);
      let vtbl = *(env.cast::<*const EnvVtbl>());
      let handler = new_callback(&CTRL_HANDLER_VTBL);
      let hr = ((*vtbl).create_controller)(env, host_hwnd, handler);
      com_release(handler);
      if hr < 0 {
         log::warn!("webview2: CreateCoreWebView2Controller failed: {hr:#010x}");
         mark_failed();
      }
   }
   S_OK
}

unsafe extern "system" fn on_controller_created(
   _this: *mut c_void,
   hr: i32,
   controller: *mut c_void,
) -> i32 {
   if hr < 0 || controller.is_null() {
      log::warn!("webview2: controller creation failed: {hr:#010x}");
      mark_failed();
      return S_OK;
   }
   // SAFETY: `controller` is a valid `ICoreWebView2Controller` on Discord's UI thread.
   unsafe {
      com_add_ref(controller);
      // Match Discord's dark Settings background (#1a1a1e) to avoid white flashes.
      let mut ctrl2: *mut c_void = ptr::null_mut();
      let base_vtbl = *(controller.cast::<*const ControllerVtbl>());
      if ((*base_vtbl).base.query_interface)(controller, &raw const IID_CONTROLLER2, &raw mut ctrl2)
         == S_OK
         && !ctrl2.is_null()
      {
         let vtbl2 = *(ctrl2.cast::<*const Controller2Vtbl>());
         ((*vtbl2).put_default_bg_color)(
            ctrl2,
            Color {
               a: 255,
               r: 0x1a,
               g: 0x1a,
               b: 0x1e,
            },
         );
         com_release(ctrl2);
      }

      let mut webview: *mut c_void = ptr::null_mut();
      if ((*base_vtbl).get_core_webview2)(controller, &raw mut webview) == S_OK && !webview.is_null()
      {
         if let Some(url) = windows::dashboard_url() {
            let embedded = format!("{url}?embedded=1");
            let wide_url = wide(&embedded);
            let wv_vtbl = *(webview.cast::<*const WebViewVtbl>());
            let nav_hr = ((*wv_vtbl).navigate)(webview, wide_url.as_ptr());
            log::info!("webview2: navigate {embedded} -> {nav_hr:#010x}");
         }
      }

      HOST.with(|h| {
         let mut h = h.borrow_mut();
         h.controller = controller;
         h.webview = webview;
         h.initializing = false;
      });

      let discord_hwnd = DISCORD_HWND.load(Ordering::Acquire) as *mut c_void;
      if !discord_hwnd.is_null() {
         apply_visibility(discord_hwnd, DESIRED_VISIBLE.load(Ordering::Acquire));
      }
      log::info!("webview2: embedded Sunbird Settings ready");
   }
   S_OK
}

unsafe fn apply_visibility(discord_hwnd: *mut c_void, visible: bool) {
   if visible {
      let failed = HOST.with(|h| h.borrow().failed);
      if failed {
         if !FALLBACK_OPENED.swap(true, Ordering::AcqRel) {
            windows::open_dashboard();
         }
         return;
      }
      // SAFETY: called on Discord's UI thread.
      unsafe {
         if IsWindowVisible(discord_hwnd) == 0 || IsIconic(discord_hwnd) != 0 {
            return;
         }
         start_webview2_init(discord_hwnd);
         update_host_bounds(discord_hwnd);
         HOST.with(|h| {
            let h = h.borrow();
            if !h.host_hwnd.is_null() {
               ShowWindow(h.host_hwnd, SW_SHOWNOACTIVATE);
            }
            if !h.controller.is_null() {
               let vtbl = *(h.controller.cast::<*const ControllerVtbl>());
               let hr = ((*vtbl).put_is_visible)(h.controller, 1);
               if hr < 0 {
                  log::warn!("webview2: put_is_visible(1) failed: {hr:#010x}");
               }
            }
         });
      }
   } else {
      FALLBACK_OPENED.store(false, Ordering::Release);
      // SAFETY: called on Discord's UI thread.
      unsafe {
         HOST.with(|h| {
            let h = h.borrow();
            if !h.controller.is_null() {
               let vtbl = *(h.controller.cast::<*const ControllerVtbl>());
               ((*vtbl).put_is_visible)(h.controller, 0);
            }
            if !h.host_hwnd.is_null() {
               ShowWindow(h.host_hwnd, SW_HIDE);
            }
         });
      }
   }
}

unsafe fn cleanup_host() {
   HOST.with(|h| {
      let mut h = h.borrow_mut();
      // SAFETY: releases COM objects and destroys child host on UI thread teardown.
      unsafe {
         if !h.controller.is_null() {
            let vtbl = *(h.controller.cast::<*const ControllerVtbl>());
            ((*vtbl).close)(h.controller);
            com_release(h.controller);
            h.controller = ptr::null_mut();
         }
         if !h.webview.is_null() {
            com_release(h.webview);
            h.webview = ptr::null_mut();
         }
         if !h.env.is_null() {
            com_release(h.env);
            h.env = ptr::null_mut();
         }
         if !h.host_hwnd.is_null() {
            DestroyWindow(h.host_hwnd);
            h.host_hwnd = ptr::null_mut();
         }
      }
      h.initializing = false;
   });
}

// ---------------------------------------------------------------------------
// Discord main HWND subclass
// ---------------------------------------------------------------------------

unsafe extern "system" fn discord_subclass_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
   _uid: usize,
   _ref_data: usize,
) -> isize {
   // SAFETY: Win32 subclass callback executing on Discord's UI thread.
   unsafe {
      match msg {
         WM_APP_SYNC_SETTINGS => {
            apply_visibility(hwnd, wparam != 0);
            return 0;
         },
         WM_APP_CLOSE_SETTINGS => {
            DESIRED_VISIBLE.store(false, Ordering::Release);
            apply_visibility(hwnd, false);
            SetFocus(hwnd);
            PostMessageW(hwnd, WM_KEYDOWN, VK_ESCAPE, 0);
            PostMessageW(hwnd, WM_KEYUP, VK_ESCAPE, 0);
            return 0;
         },
         WM_KEYDOWN | WM_SYSKEYDOWN => {
            let is_repeat = (lparam & 0x4000_0000) != 0;
            if wparam == usize::from(b'I') && !is_repeat {
               let ctrl = GetKeyState(VK_CONTROL) < 0;
               let shift = GetKeyState(VK_SHIFT) < 0;
               let alt = GetKeyState(VK_MENU) < 0;
               if ctrl && shift && !alt {
                  windows::open_devtools();
                  return 0;
               }
            } else if wparam == VK_ESCAPE && DESIRED_VISIBLE.swap(false, Ordering::AcqRel) {
               apply_visibility(hwnd, false);
            }
         },
         WM_LBUTTONUP if DESIRED_VISIBLE.load(Ordering::Acquire) => {
            let (pane, scale) = compute_client_pane_rect(hwnd);
            #[expect(
               clippy::cast_possible_truncation,
               reason = "GET_X_LPARAM / GET_Y_LPARAM are signed 16-bit coordinates"
            )]
            let x = i32::from((lparam & 0xffff) as i16);
            #[expect(
               clippy::cast_possible_truncation,
               reason = "GET_X_LPARAM / GET_Y_LPARAM are signed 16-bit coordinates"
            )]
            let y = i32::from(((lparam >> 16) & 0xffff) as i16);
            #[expect(clippy::cast_possible_truncation, reason = "scaled close margin fits i32")]
            let max_y = (160.0 * scale).round() as i32;
            // Discord's native `ESC` close circle sits immediately to the right of the content pane.
            if x >= pane.right && y <= max_y {
               DESIRED_VISIBLE.store(false, Ordering::Release);
               apply_visibility(hwnd, false);
            }
         },
         WM_SIZE => {
            let res = DefSubclassProc(hwnd, msg, wparam, lparam);
            if wparam == SIZE_MINIMIZED {
               apply_visibility(hwnd, false);
            } else if DESIRED_VISIBLE.load(Ordering::Acquire) {
               apply_visibility(hwnd, true);
            } else {
               update_host_bounds(hwnd);
            }
            return res;
         },
         WM_SHOWWINDOW => {
            let res = DefSubclassProc(hwnd, msg, wparam, lparam);
            if wparam == 0 {
               apply_visibility(hwnd, false);
            } else if DESIRED_VISIBLE.load(Ordering::Acquire) {
               apply_visibility(hwnd, true);
            }
            return res;
         },
         WM_MOVE | WM_WINDOWPOSCHANGED => {
            let res = DefSubclassProc(hwnd, msg, wparam, lparam);
            update_host_bounds(hwnd);
            HOST.with(|h| {
               let controller = h.borrow().controller;
               if !controller.is_null() {
                  let vtbl = *(controller.cast::<*const ControllerVtbl>());
                  ((*vtbl).notify_parent_window_pos_changed)(controller);
               }
            });
            return res;
         },
         WM_NCDESTROY => {
            cleanup_host();
            DISCORD_HWND.store(0, Ordering::Release);
            RemoveWindowSubclass(hwnd, discord_subclass_proc, SUBCLASS_ID);
         },
         _ => {},
      }
      DefSubclassProc(hwnd, msg, wparam, lparam)
   }
}

struct FindCtx {
   pid:  u32,
   hwnd: *mut c_void,
}

unsafe extern "system" fn enum_discord_windows(hwnd: *mut c_void, lparam: isize) -> i32 {
   // SAFETY: `lparam` points to `FindCtx` on `find_discord_hwnd`'s stack.
   unsafe {
      let ctx = &mut *(lparam as *mut FindCtx);
      let mut pid = 0u32;
      GetWindowThreadProcessId(hwnd, &raw mut pid);
      if pid != ctx.pid || IsWindowVisible(hwnd) == 0 {
         return 1;
      }
      let mut cls = [0u16; 128];
      let len = GetClassNameW(hwnd, cls.as_mut_ptr(), 128);
      if len > 0 {
         #[expect(clippy::cast_sign_loss, reason = "GetClassNameW length checked > 0")]
         let name = String::from_utf16_lossy(&cls[..len as usize]);
         if name == "SunbirdTrayWindow" || name == "SunbirdSettingsHost" {
            return 1;
         }
      }
      let mut rect = Rect {
         left:   0,
         top:    0,
         right:  0,
         bottom: 0,
      };
      GetClientRect(hwnd, &raw mut rect);
      if rect.right - rect.left >= 300 && rect.bottom - rect.top >= 200 {
         ctx.hwnd = hwnd;
         return 0;
      }
      1
   }
}

fn find_discord_hwnd() -> *mut c_void {
   // SAFETY: Win32 process ID and EnumWindows with stack-owned `FindCtx`.
   unsafe {
      let mut ctx = FindCtx {
         pid:  GetCurrentProcessId(),
         hwnd: ptr::null_mut(),
      };
      EnumWindows(enum_discord_windows, (&raw mut ctx) as isize);
      ctx.hwnd
   }
}

fn ensure_subclassed() -> *mut c_void {
   let current = DISCORD_HWND.load(Ordering::Acquire) as *mut c_void;
   // SAFETY: `IsWindow` safely validates any HWND handle.
   if !current.is_null() && unsafe { IsWindow(current) } != 0 {
      return current;
   }
   let found = find_discord_hwnd();
   if found.is_null() {
      return ptr::null_mut();
   }
   // SAFETY: attaches subclass to Discord's live top-level HWND.
   if unsafe { SetWindowSubclass(found, discord_subclass_proc, SUBCLASS_ID, 0) } != 0 {
      DISCORD_HWND.store(found as usize, Ordering::Release);
      log::info!("webview2: attached to Discord window ({found:p}); Ctrl+Shift+I enabled");
      found
   } else {
      ptr::null_mut()
   }
}

/// Starts a lightweight background watcher that attaches our subclass to
/// Discord's main window as soon as GPUI opens it, so `Ctrl+Shift+I` works
/// immediately.
pub fn start_window_hook() {
   if WATCHER_STARTED.swap(true, Ordering::AcqRel) {
      return;
   }
   let _ = thread::Builder::new()
      .name("sunbird-hwnd".to_owned())
      .spawn(|| {
         for _ in 0..120 {
            if !ensure_subclassed().is_null() {
               return;
            }
            thread::sleep(Duration::from_millis(250));
         }
      });
}

/// Shows or hides the embedded Sunbird Settings WebView2 pane inside Discord's
/// User Settings modal. Safe to call from any thread (including GPUI render
/// detours) — cheap atomic no-op when visibility is unchanged.
pub fn set_embedded_settings_visible(visible: bool) {
   let prev = DESIRED_VISIBLE.swap(visible, Ordering::AcqRel);
   if prev == visible {
      return;
   }
   let hwnd = ensure_subclassed();
   if hwnd.is_null() {
      if visible {
         windows::open_dashboard();
      }
      return;
   }
   // SAFETY: `PostMessageW` is thread-safe and marshals execution onto Discord's UI thread.
   unsafe {
      PostMessageW(hwnd, WM_APP_SYNC_SETTINGS, usize::from(visible), 0);
   }
}

/// Hides the embedded Sunbird Settings pane and forwards `Escape` to Discord so
/// its User Settings modal closes too.
pub fn close_embedded_settings() {
   let hwnd = DISCORD_HWND.load(Ordering::Acquire) as *mut c_void;
   if hwnd.is_null() {
      return;
   }
   // SAFETY: thread-safe Win32 message post to Discord's UI thread.
   unsafe {
      PostMessageW(hwnd, WM_APP_CLOSE_SETTINGS, 0, 0);
   }
}
