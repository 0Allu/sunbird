//! `phoenix-inject [phoenix.dll] [-- discord args]`
//!
//! `Discord.exe` resolves its imports from System32 only (`DependentLoadFlags`
//! 0x800), so a DLL planted beside it is never loaded. This starts it
//! suspended and queues `LoadLibraryW(phoenix.dll)` as an APC on its main
//! thread, which runs once the OS loader finishes and before Discord's own
//! code. Quit Discord first: a second instance hands off to the running one
//! and exits.

#[cfg(windows)]
fn main() -> misstep::Result<()> {
   misstep::install_panic_hook();
   windows::run()
}

#[cfg(not(windows))]
fn main() {
   eprintln!("phoenix-inject only runs on Windows");
}

#[cfg(windows)]
mod windows {
   use std::{
      env,
      ffi::{
         OsStr,
         OsString,
         c_void,
      },
      io,
      os::windows::ffi::OsStrExt,
      path::PathBuf,
      ptr,
   };

   use misstep::OptionExt;

   type Handle = *mut c_void;

   #[repr(C)]
   struct ProcessInformation {
      process:    Handle,
      thread:     Handle,
      process_id: u32,
      thread_id:  u32,
   }

   const CREATE_SUSPENDED: u32 = 0x4;
   const MEM_COMMIT_RESERVE: u32 = 0x3000;
   const PAGE_READWRITE: u32 = 0x4;

   #[link(name = "kernel32")]
   unsafe extern "system" {
      fn CreateProcessW(
         application: *const u16,
         command_line: *mut u16,
         process_attributes: *const c_void,
         thread_attributes: *const c_void,
         inherit_handles: i32,
         flags: u32,
         environment: *const c_void,
         directory: *const u16,
         startup: *const c_void,
         info: *mut ProcessInformation,
      ) -> i32;
      fn VirtualAllocEx(
         process: Handle,
         address: *const c_void,
         size: usize,
         kind: u32,
         protect: u32,
      ) -> *mut c_void;
      fn WriteProcessMemory(
         process: Handle,
         address: *mut c_void,
         buffer: *const c_void,
         size: usize,
         written: *mut usize,
      ) -> i32;
      fn GetModuleHandleW(name: *const u16) -> Handle;
      fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
      fn QueueUserAPC(function: *mut c_void, thread: Handle, data: usize) -> u32;
      fn ResumeThread(thread: Handle) -> u32;
      fn TerminateProcess(process: Handle, code: u32) -> i32;
      fn CloseHandle(handle: Handle) -> i32;
   }

   fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
      text.as_ref().encode_wide().chain([0]).collect()
   }

   fn check(ok: bool) -> io::Result<()> {
      if ok {
         Ok(())
      } else {
         Err(io::Error::last_os_error())
      }
   }

   pub fn run() -> misstep::Result<()> {
      let mut args = env::args_os().skip(1);
      let dll = match args.next().filter(|arg| arg != "--") {
         Some(path) => PathBuf::from(path),
         None => env::current_exe()?.with_file_name("phoenix.dll"),
      };
      let dll = dll.canonicalize()?;
      let app = PathBuf::from(env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unset")?)
         .join("DiscordPhoenix")
         .join("app");
      let exe = app.join("Discord.exe");
      let mut command = OsString::from(format!("\"{}\"", exe.display()));
      for arg in args.skip_while(|arg| arg == "--") {
         command.push(" ");
         command.push(arg);
      }
      let mut command = wide(command);

      // STARTUPINFOW is 104 bytes on x64 and only `cb` must be set.
      let mut startup = [0u64; 13];
      startup[0] = 104;
      let mut info = ProcessInformation {
         process:    ptr::null_mut(),
         thread:     ptr::null_mut(),
         process_id: 0,
         thread_id:  0,
      };
      // SAFETY: every pointer is valid for the call and the strings are
      // NUL-terminated.
      check(unsafe {
         CreateProcessW(
            wide(&exe).as_ptr(),
            command.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            CREATE_SUSPENDED,
            ptr::null(),
            wide(&app).as_ptr(),
            startup.as_ptr().cast(),
            &raw mut info,
         ) != 0
      })?;

      let queued = queue_load(&info, &wide(&dll));
      if queued.is_err() {
         // SAFETY: `info.process` is the suspended child created above.
         unsafe { TerminateProcess(info.process, 1) };
      } else {
         // SAFETY: `info.thread` is the child's suspended main thread.
         unsafe { ResumeThread(info.thread) };
      }
      // SAFETY: both handles came from CreateProcessW and are closed once.
      unsafe {
         CloseHandle(info.thread);
         CloseHandle(info.process);
      }
      queued?;
      println!(
         "started Discord (pid {}) with {}",
         info.process_id,
         dll.display()
      );
      Ok(())
   }

   /// Copies `dll` into the child and queues `LoadLibraryW` on its main
   /// thread. kernel32 sits at the same address in every process of a boot.
   fn queue_load(info: &ProcessInformation, dll: &[u16]) -> io::Result<()> {
      let size = size_of_val(dll);
      // SAFETY: `info.process` is a live process handle with full access.
      let remote = unsafe {
         VirtualAllocEx(
            info.process,
            ptr::null(),
            size,
            MEM_COMMIT_RESERVE,
            PAGE_READWRITE,
         )
      };
      check(!remote.is_null())?;
      // SAFETY: `remote` holds `size` bytes and `dll` is `size` bytes long.
      check(unsafe {
         WriteProcessMemory(
            info.process,
            remote,
            dll.as_ptr().cast(),
            size,
            ptr::null_mut(),
         ) != 0
      })?;
      // SAFETY: kernel32 is always loaded and the names are NUL-terminated.
      let load = unsafe {
         GetProcAddress(
            GetModuleHandleW(wide("kernel32.dll").as_ptr()),
            c"LoadLibraryW".as_ptr().cast(),
         )
      };
      check(!load.is_null())?;
      // SAFETY: LoadLibraryW takes one pointer argument, as an APC does.
      check(unsafe { QueueUserAPC(load, info.thread, remote as usize) } != 0)
   }
}
