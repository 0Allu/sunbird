//! `sunbird.exe`, the Windows launcher, also `phoenix-inject [phoenix.dll] [--
//! discord args]`.
//!
//! `Discord.exe` resolves its imports from System32 only (`DependentLoadFlags`
//! 0x800), so a DLL planted beside it is never loaded. This starts it
//! suspended and queues `LoadLibraryW(phoenix.dll)` as an APC on its main
//! thread, which runs once the OS loader finishes and before Discord's own
//! code. Quit Discord first: a second instance hands off to the running one
//! and exits.
//!
//! Without a DLL argument it installs itself to `%LOCALAPPDATA%\Sunbird` with
//! Start Menu and Desktop shortcuts, and fetches the site's newest
//! `phoenix.dll` there before each launch. Discord's own files stay untouched,
//! so its updates never undo Sunbird. `--uninstall` removes both.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
   misstep::install_panic_hook();
   if let Err(err) = windows::run() {
      windows::alert(&format!("{err:?}"));
      std::process::exit(1);
   }
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
      fs,
      io,
      os::windows::ffi::OsStrExt,
      path::{
         Path,
         PathBuf,
      },
      process::Command,
      ptr,
   };

   use misstep::{
      OptionExt,
      ResultExt,
   };

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

   #[link(name = "user32")]
   unsafe extern "system" {
      fn MessageBoxW(window: Handle, text: *const u16, caption: *const u16, kind: u32) -> i32;
   }

   /// `CREATE_NO_WINDOW`, so `curl` and PowerShell don't flash a console.
   const NO_WINDOW: u32 = 0x0800_0000;

   /// Where the site serves `version.json` and the loaders. `SUNBIRD_SITE`
   /// points it at a local server for testing; anything else must be https,
   /// since what it serves is loaded into Discord.
   fn site() -> misstep::Result<String> {
      let site =
         env::var("SUNBIRD_SITE").unwrap_or_else(|_| "https://assets.sunbird.tools".to_owned());
      if !site_allowed(&site) {
         misstep::bail!("SUNBIRD_SITE must be https:// or a localhost server, not {site}");
      }
      Ok(site)
   }

   fn site_allowed(site: &str) -> bool {
      site.starts_with("https://")
         || ["http://127.0.0.1", "http://localhost"]
            .iter()
            .any(|local| {
               site
                  .strip_prefix(local)
                  .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
            })
   }

   /// A program in System32, by full path: `Command` looks beside this
   /// launcher first, which is the Downloads folder on the first run.
   fn system32(program: &str) -> PathBuf {
      PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()))
         .join("System32")
         .join(program)
   }

   /// Shows `text` in a message box, since the launcher has no console.
   pub fn alert(text: &str) {
      // SAFETY: both strings are NUL-terminated and outlive the call.
      unsafe {
         MessageBoxW(
            ptr::null_mut(),
            wide(text).as_ptr(),
            wide("Sunbird").as_ptr(),
            0x10,
         )
      };
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
      let local = PathBuf::from(env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unset")?);
      let home = local.join("Sunbird");
      let mut args = env::args_os().skip(1).peekable();
      if args.peek().is_some_and(|arg| arg == "--uninstall") {
         uninstall(&local, &home);
         return Ok(());
      }
      let app = local.join("DiscordPhoenix").join("app");
      let exe = app.join("Discord.exe");
      if !exe.is_file() {
         misstep::bail!(
            "Discord Phoenix is not installed at {}. Install it first, then open Sunbird again.",
            exe.display()
         );
      }
      let explicit = args.next_if(|arg| {
         Path::new(arg)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
      });
      let dll = match explicit {
         Some(path) => PathBuf::from(path),
         None => install(&local, &home)?,
      };
      let dll = dll.canonicalize()?;
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

   /// Copies this launcher into `home`, adds its shortcuts once, and brings
   /// `phoenix.dll` up to the site's version. A failed update keeps the DLL
   /// already there, so Sunbird still starts offline.
   fn install(local: &Path, home: &Path) -> misstep::Result<PathBuf> {
      fs::create_dir_all(home)?;
      let launcher = home.join("sunbird.exe");
      let me = env::current_exe()?;
      let copied = !same_file(&me, &launcher);
      if copied {
         fs::copy(&me, &launcher).context("copying the launcher")?;
      }
      let dll = home.join("phoenix.dll");
      if let Err(err) = update(home, &dll)
         && !dll.is_file()
      {
         return Err(err.context("downloading phoenix.dll"));
      }
      // Missing shortcuts never stop Discord from starting, and a shortcut the
      // user deleted stays deleted until they run the installer again.
      if copied {
         let _ = shortcuts(local, &launcher, false);
      }
      Ok(dll)
   }

   fn same_file(a: &Path, b: &Path) -> bool {
      a.canonicalize()
         .ok()
         .zip(b.canonicalize().ok())
         .is_some_and(|(a, b)| a == b)
   }

   /// Downloads `phoenix.dll` when the site's loader version differs from the
   /// one recorded beside it. A running Discord keeps the old DLL mapped,
   /// which Windows lets us rename but not overwrite.
   fn update(home: &Path, dll: &Path) -> misstep::Result<()> {
      let site = site()?;
      let manifest = curl(&site, 10, &[&format!("{site}/version.json")])?;
      let version = loader_version(&manifest).context("version.json has no loader")?;
      let stamp = home.join("version.txt");
      if dll.is_file() && fs::read_to_string(&stamp).is_ok_and(|have| have.trim() == version) {
         return Ok(());
      }
      // Per process, so a second launch mid-download writes its own file.
      let fresh = home.join(format!("phoenix.dll.{}.new", std::process::id()));
      curl(&site, 60, &[
         "-o",
         &fresh.to_string_lossy(),
         &format!("{site}/plugins/phoenix-windows-x86_64.dll"),
      ])?;
      let old = home.join("phoenix.dll.old");
      let _ = fs::remove_file(&old);
      if dll.is_file() {
         fs::rename(dll, &old)?;
      }
      fs::rename(&fresh, dll)?;
      let _ = fs::remove_file(&old);
      fs::write(stamp, version)?;
      Ok(())
   }

   /// Runs Windows' own `curl.exe` and returns what it printed. Only
   /// `site`'s scheme is allowed, and redirects never leave https.
   fn curl(site: &str, max_secs: u32, args: &[&str]) -> misstep::Result<String> {
      use std::os::windows::process::CommandExt;
      let proto = if site.starts_with("https://") {
         "=https"
      } else {
         "=http"
      };
      let out = Command::new(system32("curl.exe"))
         .args(["-fsSL", "--proto", proto, "--proto-redir", "=https"])
         .args([
            "--ssl-revoke-best-effort",
            "--max-time",
            &max_secs.to_string(),
         ])
         .args(args)
         .creation_flags(NO_WINDOW)
         .output()
         .context("running curl.exe")?;
      if !out.status.success() {
         misstep::bail!("curl: {}", String::from_utf8_lossy(&out.stderr).trim());
      }
      Ok(String::from_utf8_lossy(&out.stdout).into_owned())
   }

   /// `"loader": "<version>"` from `version.json`.
   fn loader_version(manifest: &str) -> Option<&str> {
      let rest = &manifest[manifest.find("\"loader\"")? + 8..];
      let rest = &rest[rest.find('"')? + 1..];
      rest.get(..rest.find('"')?)
   }

   /// The Start Menu and Desktop shortcuts, through PowerShell's
   /// `WScript.Shell`, so OneDrive-redirected desktops resolve. Adds them when
   /// missing, or removes them. Paths reach the script through the
   /// environment, never inside its text.
   fn shortcuts(local: &Path, launcher: &Path, remove: bool) -> misstep::Result<()> {
      use std::os::windows::process::CommandExt;
      let script = format!(
         "$w = New-Object -ComObject WScript.Shell; foreach ($dir in \
          @([Environment]::GetFolderPath('Programs'), [Environment]::GetFolderPath('Desktop'))) \
          {{ $p = Join-Path $dir 'Sunbird.lnk'; if (${remove}) {{ Remove-Item -LiteralPath $p \
          -ErrorAction SilentlyContinue; continue }}; if (Test-Path -LiteralPath $p) {{ continue \
          }}; $s = $w.CreateShortcut($p); $s.TargetPath = $env:SB_TARGET; $s.IconLocation = \
          $env:SB_ICON + ',0'; $s.Description = 'Discord with Sunbird'; $s.Save() }}"
      );
      let status = Command::new(system32(r"WindowsPowerShell\v1.0\powershell.exe"))
         .args(["-NoProfile", "-NonInteractive", "-Command", &script])
         .env("SB_TARGET", launcher)
         .env("SB_ICON", local.join(r"DiscordPhoenix\app\Discord.exe"))
         .creation_flags(NO_WINDOW)
         .status()
         .context("running powershell.exe")?;
      if !status.success() {
         misstep::bail!("creating the Sunbird shortcuts failed");
      }
      Ok(())
   }

   /// Removes the shortcuts and `home`. Files a running Discord or launcher
   /// still holds stay until they exit, and the user is told so.
   fn uninstall(local: &Path, home: &Path) {
      let _ = shortcuts(local, &home.join("sunbird.exe"), true);
      match fs::remove_dir_all(home) {
         Err(err) if err.kind() != io::ErrorKind::NotFound => {
            alert(&format!(
               "The shortcuts are removed, but {} could not be deleted ({err}). Quit Discord and \
                delete it.",
               home.display()
            ));
         },
         _ => alert("Sunbird is uninstalled. Discord itself was not changed."),
      }
   }

   #[cfg(test)]
   mod tests {
      #[test]
      fn reads_the_loader_version() {
         let manifest = "{\n  \"loader\": \"2026.11.23.2353-b51d77c\",\n  \"plugins\": {}\n}";
         assert_eq!(
            super::loader_version(manifest),
            Some("2026.11.23.2353-b51d77c")
         );
         assert_eq!(super::loader_version("{}"), None);
      }

      #[test]
      fn allows_https_or_localhost_sites() {
         for ok in [
            "https://assets.sunbird.tools",
            "http://127.0.0.1:8765",
            "http://localhost",
            "http://localhost/x",
         ] {
            assert!(super::site_allowed(ok), "{ok}");
         }
         for bad in [
            "http://assets.sunbird.tools",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "ftp://x",
         ] {
            assert!(!super::site_allowed(bad), "{bad}");
         }
      }
   }
}
