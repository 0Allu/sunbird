use std::{
   ffi::c_void,
   ptr,
   sync::{
      OnceLock,
      atomic::{
         AtomicBool,
         AtomicUsize,
         Ordering,
      },
   },
   thread,
};

use crate::Error;

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;
const SW_SHOWNORMAL: i32 = 1;

const WM_USER: u32 = 0x0400;
const WM_TRAY: u32 = WM_USER + 1;
const WM_LBUTTONUP: usize = 0x0202;
const WM_LBUTTONDBLCLK: usize = 0x0203;
const WM_RBUTTONUP: usize = 0x0205;
const WM_COMMAND: u32 = 0x0111;
const WM_DESTROY: u32 = 0x0002;

const NIM_ADD: u32 = 0x0000_0000;
const NIM_MODIFY: u32 = 0x0000_0001;
const NIM_DELETE: u32 = 0x0000_0002;
const NIF_MESSAGE: u32 = 0x0000_0001;
const NIF_ICON: u32 = 0x0000_0002;
const NIF_TIP: u32 = 0x0000_0004;
const NIF_INFO: u32 = 0x0000_0010;
const NIIF_INFO: u32 = 0x0000_0001;

const MF_STRING: u32 = 0x0000_0000;
const TPM_RIGHTBUTTON: u32 = 0x0002;
const TPM_NONOTIFY: u32 = 0x0080;
const TPM_RETURNCMD: u32 = 0x0100;

const ID_OPEN_SETTINGS: usize = 1001;
const IDI_APPLICATION: usize = 32512;
const TRAY_UID: u32 = 0x5342_5244; // "SBRD"

static TRAY_STARTED: AtomicBool = AtomicBool::new(false);
static TRAY_HWND: AtomicUsize = AtomicUsize::new(0);
static DASHBOARD_URL: OnceLock<String> = OnceLock::new();

#[repr(C)]
struct WndClassExW {
   cb_size:       u32,
   style:         u32,
   wnd_proc:      unsafe extern "system" fn(*mut c_void, u32, usize, isize) -> isize,
   cls_extra:     i32,
   wnd_extra:     i32,
   instance:      *mut c_void,
   icon:          *mut c_void,
   cursor:        *mut c_void,
   background:    *mut c_void,
   menu_name:     *const u16,
   class_name:    *const u16,
   icon_sm:       *mut c_void,
}

#[repr(C)]
struct Point {
   x: i32,
   y: i32,
}

#[repr(C)]
struct Msg {
   hwnd:    *mut c_void,
   message: u32,
   wparam:  usize,
   lparam:  isize,
   time:    u32,
   pt:      Point,
}

#[repr(C)]
struct NotifyIconDataW {
   cb_size:              u32,
   hwnd:                 *mut c_void,
   u_id:                 u32,
   u_flags:              u32,
   u_callback_message:   u32,
   h_icon:               *mut c_void,
   sz_tip:               [u16; 128],
   dw_state:             u32,
   dw_state_mask:        u32,
   sz_info:              [u16; 256],
   u_timeout_or_version: u32,
   sz_info_title:        [u16; 64],
   dw_info_flags:        u32,
   guid_item:            [u8; 16],
   h_balloon_icon:       *mut c_void,
}

#[link(name = "user32")]
unsafe extern "system" {
   fn OpenClipboard(hwnd: *mut c_void) -> i32;
   fn CloseClipboard() -> i32;
   fn EmptyClipboard() -> i32;
   fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
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
   fn DefWindowProcW(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> isize;
   fn GetMessageW(msg: *mut Msg, hwnd: *mut c_void, min: u32, max: u32) -> i32;
   fn TranslateMessage(msg: *const Msg) -> i32;
   fn DispatchMessageW(msg: *const Msg) -> isize;
   fn LoadIconW(instance: *mut c_void, name: *const u16) -> *mut c_void;
   fn CreatePopupMenu() -> *mut c_void;
   fn AppendMenuW(menu: *mut c_void, flags: u32, id: usize, item: *const u16) -> i32;
   fn TrackPopupMenu(
      menu: *mut c_void,
      flags: u32,
      x: i32,
      y: i32,
      reserved: i32,
      hwnd: *mut c_void,
      rect: *const c_void,
   ) -> i32;
   fn DestroyMenu(menu: *mut c_void) -> i32;
   fn GetCursorPos(pt: *mut Point) -> i32;
   fn SetForegroundWindow(hwnd: *mut c_void) -> i32;
   fn PostQuitMessage(code: i32);
}

#[link(name = "kernel32")]
unsafe extern "system" {
   fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
   fn GlobalLock(mem: *mut c_void) -> *mut c_void;
   fn GlobalUnlock(mem: *mut c_void) -> i32;
   fn GlobalFree(mem: *mut c_void) -> *mut c_void;
   fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

#[link(name = "shell32")]
unsafe extern "system" {
   fn ShellExecuteW(
      hwnd: *mut c_void,
      operation: *const u16,
      file: *const u16,
      parameters: *const u16,
      directory: *const u16,
      show_cmd: i32,
   ) -> isize;
   fn Shell_NotifyIconW(message: u32, data: *mut NotifyIconDataW) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
   text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fill_wide<const N: usize>(dst: &mut [u16; N], text: &str) {
   dst.fill(0);
   for (slot, unit) in dst.iter_mut().take(N.saturating_sub(1)).zip(text.encode_utf16()) {
      *slot = unit;
   }
}

fn os_err(context: &str) -> Error {
   Error::Platform {
      message: format!("{context}: {}", std::io::Error::last_os_error()),
   }
}

fn open_dashboard() {
   if let Some(url) = DASHBOARD_URL.get()
      && let Err(err) = crate::open_url(url)
   {
      log::warn!("tray: could not open dashboard: {err}");
   }
}

fn empty_nid(hwnd: *mut c_void) -> NotifyIconDataW {
   #[expect(clippy::cast_possible_truncation, reason = "NOTIFYICONDATAW is 976 bytes")]
   let cb_size = size_of::<NotifyIconDataW>() as u32;
   NotifyIconDataW {
      cb_size,
      hwnd,
      u_id: TRAY_UID,
      u_flags: 0,
      u_callback_message: WM_TRAY,
      h_icon: ptr::null_mut(),
      sz_tip: [0; 128],
      dw_state: 0,
      dw_state_mask: 0,
      sz_info: [0; 256],
      u_timeout_or_version: 5000,
      sz_info_title: [0; 64],
      dw_info_flags: 0,
      guid_item: [0; 16],
      h_balloon_icon: ptr::null_mut(),
   }
}

pub fn start_tray(dashboard_url: &str) {
   let _ = DASHBOARD_URL.set(dashboard_url.to_owned());
   if TRAY_STARTED.swap(true, Ordering::AcqRel) {
      return;
   }
   let _ = thread::Builder::new()
      .name("sunbird-tray".to_owned())
      .spawn(run_tray_loop);
}

fn run_tray_loop() {
   let class_name = wide("SunbirdTrayWindow");
   let title = wide("Sunbird");
   // SAFETY: Win32 hidden message window and tray registration on dedicated thread.
   unsafe {
      let instance = GetModuleHandleW(ptr::null());
      #[expect(clippy::cast_possible_truncation, reason = "WNDCLASSEXW is 80 bytes")]
      let cb_size = size_of::<WndClassExW>() as u32;
      let cls = WndClassExW {
         cb_size,
         style: 0,
         wnd_proc: tray_wnd_proc,
         cls_extra: 0,
         wnd_extra: 0,
         instance,
         icon: ptr::null_mut(),
         cursor: ptr::null_mut(),
         background: ptr::null_mut(),
         menu_name: ptr::null(),
         class_name: class_name.as_ptr(),
         icon_sm: ptr::null_mut(),
      };
      RegisterClassExW(&raw const cls);

      let hwnd = CreateWindowExW(
         0,
         class_name.as_ptr(),
         title.as_ptr(),
         0,
         0,
         0,
         0,
         0,
         ptr::null_mut(),
         ptr::null_mut(),
         instance,
         ptr::null_mut(),
      );
      if hwnd.is_null() {
         log::warn!("tray: CreateWindowExW failed");
         return;
      }

      let mut nid = empty_nid(hwnd);
      nid.u_flags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
      nid.h_icon = LoadIconW(ptr::null_mut(), IDI_APPLICATION as *const u16);
      fill_wide(&mut nid.sz_tip, "Sunbird — Click for Settings");
      if Shell_NotifyIconW(NIM_ADD, &raw mut nid) == 0 {
         log::warn!("tray: Shell_NotifyIconW(NIM_ADD) failed");
         return;
      }
      TRAY_HWND.store(hwnd as usize, Ordering::Release);

      let mut msg = Msg {
         hwnd:    ptr::null_mut(),
         message: 0,
         wparam:  0,
         lparam:  0,
         time:    0,
         pt:      Point { x: 0, y: 0 },
      };
      while GetMessageW(&raw mut msg, ptr::null_mut(), 0, 0) > 0 {
         TranslateMessage(&raw const msg);
         DispatchMessageW(&raw const msg);
      }

      TRAY_HWND.store(0, Ordering::Release);
      let mut del = empty_nid(hwnd);
      Shell_NotifyIconW(NIM_DELETE, &raw mut del);
   }
}

pub fn balloon(title: &str, body: &str) {
   let raw = TRAY_HWND.load(Ordering::Acquire);
   if raw == 0 {
      return;
   }
   let hwnd = raw as *mut c_void;
   let mut nid = empty_nid(hwnd);
   nid.u_flags = NIF_INFO;
   nid.dw_info_flags = NIIF_INFO;
   fill_wide(&mut nid.sz_info_title, title);
   fill_wide(&mut nid.sz_info, body);
   // SAFETY: `hwnd` is a live tray window registered via NIM_ADD.
   unsafe {
      Shell_NotifyIconW(NIM_MODIFY, &raw mut nid);
   }
}

unsafe extern "system" fn tray_wnd_proc(
   hwnd: *mut c_void,
   msg: u32,
   wparam: usize,
   lparam: isize,
) -> isize {
   match msg {
      WM_TRAY => {
         #[expect(clippy::cast_sign_loss, reason = "Win32 tray event code sits in low bits of lparam")]
         let event = (lparam as usize) & 0xffff;
         match event {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => open_dashboard(),
            WM_RBUTTONUP => {
               // SAFETY: Win32 popup menu owned and destroyed in this scope.
               unsafe {
                  let menu = CreatePopupMenu();
                  if !menu.is_null() {
                     let label = wide("Open Sunbird Settings");
                     AppendMenuW(menu, MF_STRING, ID_OPEN_SETTINGS, label.as_ptr());
                     let mut pt = Point { x: 0, y: 0 };
                     GetCursorPos(&raw mut pt);
                     SetForegroundWindow(hwnd);
                     let cmd = TrackPopupMenu(
                        menu,
                        TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
                        pt.x,
                        pt.y,
                        0,
                        hwnd,
                        ptr::null(),
                     );
                     DestroyMenu(menu);
                     #[expect(clippy::cast_sign_loss, reason = "menu command ids are positive")]
                     if cmd > 0 && (cmd as usize) == ID_OPEN_SETTINGS {
                        open_dashboard();
                     }
                  }
               }
            },
            _ => {},
         }
         0
      },
      WM_COMMAND if wparam == ID_OPEN_SETTINGS => {
         open_dashboard();
         0
      },
      WM_DESTROY => {
         // SAFETY: standard Win32 WM_DESTROY handler.
         unsafe { PostQuitMessage(0) };
         0
      },
      _ => {
         // SAFETY: delegate unhandled messages to DefWindowProcW.
         unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
      },
   }
}

pub fn copy(text: &str) -> Result<(), Error> {
   let units = wide(text);
   let bytes = units.len() * size_of::<u16>();

   // SAFETY: Win32 clipboard APIs with checked return values; memory is freed
   // on any error before SetClipboardData takes ownership.
   unsafe {
      if OpenClipboard(ptr::null_mut()) == 0 {
         return Err(os_err("OpenClipboard"));
      }
      if EmptyClipboard() == 0 {
         let err = os_err("EmptyClipboard");
         CloseClipboard();
         return Err(err);
      }
      let mem = GlobalAlloc(GMEM_MOVEABLE, bytes);
      if mem.is_null() {
         let err = os_err("GlobalAlloc");
         CloseClipboard();
         return Err(err);
      }
      let locked = GlobalLock(mem);
      if locked.is_null() {
         let err = os_err("GlobalLock");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      ptr::copy_nonoverlapping(units.as_ptr(), locked.cast::<u16>(), units.len());
      GlobalUnlock(mem);
      if SetClipboardData(CF_UNICODETEXT, mem).is_null() {
         let err = os_err("SetClipboardData");
         GlobalFree(mem);
         CloseClipboard();
         return Err(err);
      }
      CloseClipboard();
   }
   Ok(())
}

pub fn open_url(url: &str) -> Result<(), Error> {
   let op = wide("open");
   let target = wide(url);
   // SAFETY: NUL-terminated UTF-16 strings passed to ShellExecuteW.
   let result = unsafe {
      ShellExecuteW(
         ptr::null_mut(),
         op.as_ptr(),
         target.as_ptr(),
         ptr::null(),
         ptr::null(),
         SW_SHOWNORMAL,
      )
   };
   if result <= 32 {
      return Err(Error::Platform {
         message: format!("ShellExecuteW failed with code {result}"),
      });
   }
   Ok(())
}
