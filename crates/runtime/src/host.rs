//! Numbers of the Phoenix host calls, events and fields phoenix uses. Phoenix
//! numbers them through a name table of `"<name> <number>"` strings in its
//! dex, which a Discord build can renumber, so they are read from the
//! installed APK at startup.

use std::{
   fs::File,
   os::fd::AsRawFd,
   sync::{
      Mutex,
      OnceLock,
      PoisonError,
      atomic::{
         AtomicI32,
         Ordering,
      },
      mpsc::{
         self,
         Sender,
      },
   },
   time::Duration,
};

use phoenix_wire::{
   DispatchEvent,
   HostCall,
   Key,
   Value,
   decode_dispatch,
   edit_frames,
};

use crate::bridge::{
   self,
   Action,
};

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

#[derive(Debug, flub::Error)]
pub enum CallError {
   #[error("Phoenix's host names are not read yet")]
   NotLoaded,
   #[error("this Discord build has no {0}")]
   Missing(&'static str),
   #[error("Phoenix's bridge is not up yet")]
   NoBridge,
   #[error("{0} did not answer")]
   NoAnswer(&'static str),
}

/// Calls `method` with `args` and `body`, as Phoenix's guest would.
pub fn send(method: Method, args: &[(Field, Value)], body: Vec<u8>) -> Result<(), CallError> {
   let host = table().ok_or(CallError::NotLoaded)?;
   let (module, number) = host
      .call(method)
      .ok_or_else(|| CallError::Missing(method.name()))?;
   let args = args
      .iter()
      .map(|(field, value)| {
         let key = host
            .key(*field)
            .ok_or_else(|| CallError::Missing(field.name()))?;
         Ok((key, value.clone()))
      })
      .collect::<Result<Vec<_>, CallError>>()?;
   let call = HostCall {
      module,
      method: number,
      args: Value::Obj(args),
      body,
   };
   if bridge::invoke(&call.encode()) {
      Ok(())
   } else {
      Err(CallError::NoBridge)
   }
}

/// `wanted` of a call's or event's arguments.
pub fn field(args: &Value, wanted: Field) -> Option<&Value> {
   args.field(table()?.key(wanted)?)
}

/// An event's arguments and body.
pub struct Reply {
   pub args: Value,
   pub body: Vec<u8>,
}

/// Far above the ids Phoenix's guest numbers its own calls with.
const FIRST_ID: i32 = 0x6000_0000;

static NEXT_ID: AtomicI32 = AtomicI32::new(FIRST_ID);
/// Who waits for which event with which id.
static WAITING: Mutex<Vec<(u16, i32, Sender<Reply>)>> = Mutex::new(Vec::new());

/// Calls `method` with an `id` field and waits up to `timeout` for the
/// `event` that answers it.
pub fn ask(
   method: Method,
   args: &[(Field, Value)],
   event: Event,
   timeout: Duration,
) -> Result<Reply, CallError> {
   let number = table()
      .ok_or(CallError::NotLoaded)?
      .event(event)
      .ok_or_else(|| CallError::Missing(event.name()))?;
   let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   WAITING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .push((number, id, sender));
   let forget = || {
      WAITING
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .retain(|(_, waiting, _)| *waiting != id);
   };
   let mut with_id = vec![(Field::Id, Value::I32(id))];
   with_id.extend_from_slice(args);
   if let Err(err) = send(method, &with_id, Vec::new()) {
      forget();
      return Err(err);
   }
   let reply = receiver.recv_timeout(timeout);
   forget();
   reply.map_err(|_| CallError::NoAnswer(method.name()))
}

/// Takes the events answering [`ask`] out of a `nativeDispatch` call, since
/// Phoenix's guest knows nothing of their ids.
pub fn filter(bytes: &[u8]) -> Action {
   if WAITING
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_empty()
   {
      return Action::Pass;
   }
   let Some(id_key) = table().and_then(|host| host.key(Field::Id)) else {
      return Action::Pass;
   };
   let Ok(items) = decode_dispatch(bytes) else {
      return Action::Pass;
   };
   let count = items.len();
   let mut taken = Vec::new();
   for item in items {
      let DispatchEvent::Host { event, args, body } = item.event else {
         continue;
      };
      let Some(id) = args.field(id_key).and_then(Value::as_i32) else {
         continue;
      };
      let mut waiting = WAITING.lock().unwrap_or_else(PoisonError::into_inner);
      let Some(at) = waiting
         .iter()
         .position(|(number, waiting_id, _)| *number == event && *waiting_id == id)
      else {
         continue;
      };
      let (_, _, sender) = waiting.swap_remove(at);
      drop(waiting);
      let _ = sender.send(Reply {
         args,
         body: body.to_vec(),
      });
      taken.push((item.offset, None));
   }
   match taken.len() {
      0 => Action::Pass,
      len if len == count => Action::Drop,
      _ => Action::Replace(edit_frames(bytes, &taken)),
   }
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
