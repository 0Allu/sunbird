//! Numbers of the Phoenix host calls, events and fields phoenix uses. Phoenix
//! numbers them through a name table of `"<name> <number>"` strings in its
//! dex, which a Discord build can renumber, so they are read from the
//! installed APK at startup.

use std::{
   fs::File,
   os::fd::AsRawFd,
   sync::OnceLock,
};

use phoenix_wire::Key;

/// A host module's method, named `Module.method`.
#[derive(Clone, Copy)]
pub enum Method {
   NetworkingRequest,
   LinkingOpenUrl,
   SystemOverlayToast,
   ClipboardSetString,
   MediaPickerPick,
   MediaPickerRead,
   MediaPickerRelease,
}

impl Method {
   const ALL: [Self; 7] = [
      Self::NetworkingRequest,
      Self::LinkingOpenUrl,
      Self::SystemOverlayToast,
      Self::ClipboardSetString,
      Self::MediaPickerPick,
      Self::MediaPickerRead,
      Self::MediaPickerRelease,
   ];

   const fn name(self) -> &'static str {
      match self {
         Self::NetworkingRequest => "Networking.request",
         Self::LinkingOpenUrl => "Linking.openURL",
         Self::SystemOverlayToast => "SystemOverlay.toast",
         Self::ClipboardSetString => "Clipboard.setString",
         Self::MediaPickerPick => "MediaPicker.pick",
         Self::MediaPickerRead => "MediaPicker.read",
         Self::MediaPickerRelease => "MediaPicker.release",
      }
   }

   fn module(self) -> &'static str {
      let name = self.name();
      name.split_once('.').map_or(name, |(module, _)| module)
   }
}

/// A host event Phoenix's Java side dispatches to the guest.
#[derive(Clone, Copy)]
pub enum Event {
   Http,
   Picked,
   Read,
   Ws,
}

impl Event {
   const ALL: [Self; 4] = [Self::Http, Self::Picked, Self::Read, Self::Ws];

   const fn name(self) -> &'static str {
      match self {
         Self::Http => "@http",
         Self::Picked => "@picked",
         Self::Read => "@read",
         Self::Ws => "@ws",
      }
   }
}

/// A field of a host call's or event's arguments.
#[derive(Clone, Copy)]
pub enum Field {
   Id,
   Error,
   Method,
   Url,
   Headers,
   Status,
   Text,
   Kind,
   Max,
   Extensions,
   Files,
   Handle,
   Handles,
   Name,
   Denied,
}

impl Field {
   const ALL: [Self; 15] = [
      Self::Id,
      Self::Error,
      Self::Method,
      Self::Url,
      Self::Headers,
      Self::Status,
      Self::Text,
      Self::Kind,
      Self::Max,
      Self::Extensions,
      Self::Files,
      Self::Handle,
      Self::Handles,
      Self::Name,
      Self::Denied,
   ];

   const fn name(self) -> &'static str {
      match self {
         Self::Id => "id",
         Self::Error => "error",
         Self::Method => "method",
         Self::Url => "url",
         Self::Headers => "headers",
         Self::Status => "status",
         Self::Text => "text",
         Self::Kind => "kind",
         Self::Max => "max",
         Self::Extensions => "extensions",
         Self::Files => "files",
         Self::Handle => "handle",
         Self::Handles => "handles",
         Self::Name => "name",
         Self::Denied => "denied",
      }
   }
}

/// The numbers, laid out as the module and method of each [`Method`], then
/// each [`Event`], then each [`Field`], `None` where the build lacks the name.
pub struct Table(Vec<Option<u16>>);

impl Table {
   /// The module and method numbers of `method`.
   pub fn call(&self, method: Method) -> Option<(u16, u16)> {
      let at = 2 * method as usize;
      Some((self.0[at]?, self.0[at + 1]?))
   }

   pub fn event(&self, event: Event) -> Option<u16> {
      self.0[2 * Method::ALL.len() + event as usize]
   }

   pub fn key(&self, field: Field) -> Option<Key> {
      self.0[2 * Method::ALL.len() + Event::ALL.len() + field as usize].map(Key::new)
   }
}

#[derive(Debug, flub::Error)]
pub enum LoadError {
   #[error("no base.apk is mapped")]
   NoApk,
   #[error("{path}: {source}")]
   Io {
      path:   String,
      source: std::io::Error,
   },
   #[error(transparent)]
   Names(patcher::Error),
}

static TABLE: OnceLock<Table> = OnceLock::new();

/// The numbers, once [`load`] has read them.
pub fn table() -> Option<&'static Table> {
   TABLE.get()
}

/// Reads the numbers from the APK the app runs from. Takes about 100 ms, so
/// it runs off the threads that wait on it.
pub fn load() -> Result<(), LoadError> {
   let path = base_apk()?;
   let map = Map::open(&path).map_err(|source| LoadError::Io { path, source })?;

   let mut names = Vec::new();
   for method in Method::ALL {
      names.extend([method.module(), method.name()]);
   }
   names.extend(Event::ALL.map(Event::name));
   names.extend(Field::ALL.map(Field::name));
   let numbers = patcher::host_names(map.bytes(), &names).map_err(LoadError::Names)?;
   for (name, number) in names.iter().zip(&numbers) {
      if number.is_none() {
         log::warn!("host names: this build has no {name}");
      }
   }
   let _ = TABLE.set(Table(numbers));
   Ok(())
}

/// The path of the APK the app runs from, which ART keeps mapped.
pub fn base_apk() -> Result<String, LoadError> {
   std::fs::read_to_string("/proc/self/maps")
      .map_err(|source| {
         LoadError::Io {
            path: "/proc/self/maps".to_owned(),
            source,
         }
      })?
      .lines()
      .filter_map(|line| line.split_whitespace().nth(5))
      .find(|path| path.ends_with("/base.apk"))
      .map(str::to_owned)
      .ok_or(LoadError::NoApk)
}

/// A read-only mapping of a whole file.
pub struct Map {
   at:  *mut libc::c_void,
   len: usize,
}

impl Map {
   pub fn open(path: &str) -> std::io::Result<Self> {
      let file = File::open(path)?;
      let len = usize::try_from(file.metadata()?.len()).map_err(std::io::Error::other)?;
      // SAFETY: a fresh private read-only mapping of an open file, released in
      // `Drop`.
      let at = unsafe {
         libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
         )
      };
      if at == libc::MAP_FAILED {
         return Err(std::io::Error::last_os_error());
      }
      Ok(Self { at, len })
   }

   pub const fn bytes(&self) -> &[u8] {
      // SAFETY: `at` maps `len` readable bytes until `self` drops. The APK is
      // the installed app's, which is not rewritten in place.
      unsafe { std::slice::from_raw_parts(self.at.cast(), self.len) }
   }
}

impl Drop for Map {
   fn drop(&mut self) {
      // SAFETY: `at` and `len` came from the `mmap` in `new`.
      unsafe { libc::munmap(self.at, self.len) };
   }
}
