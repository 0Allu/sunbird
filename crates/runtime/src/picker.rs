//! The document picker, toasts, the clipboard and links through Phoenix's own
//! host modules, and dialogs through `phx.Picker` (`java/phx/Picker.java`),
//! loaded from an in-memory dex under Phoenix's class loader. The chosen file
//! goes to [`crate::settings::picked`].

use std::{
   collections::HashMap,
   ffi::{
      CString,
      c_void,
   },
   ptr,
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

use jni_sys::{
   JNIEnv,
   JNINativeMethod,
   jclass,
   jint,
   jlong,
   jobject,
   jvalue,
};
use phoenix_wire::Value;

use crate::{
   bridge,
   host::{
      self,
      Event,
      Field,
      Method,
   },
   jvm::{
      self,
      jni,
   },
};

const DEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/picker.dex"));
/// The user takes their time in the picker.
const PICK_TIMEOUT: Duration = Duration::from_mins(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The file the user chose, by name, or `None` when they backed out.
pub type Picked = Result<Option<(String, Vec<u8>)>, String>;

/// `phx.Picker` as a global reference.
static CLASS: OnceLock<usize> = OnceLock::new();
/// Who waits for each open [`choose`] dialog.
static CHOICES: Mutex<Option<HashMap<i32, Sender<i32>>>> = Mutex::new(None);

/// Shows a dialog over Phoenix's activity and waits for it: a list of
/// `options` without `message`, or `message` with up to three buttons.
/// Returns the chosen option's index, or -1 when it was dismissed.
pub fn choose(
   title: Option<&str>,
   message: Option<&str>,
   options: &[String],
) -> Result<i32, String> {
   static NEXT: AtomicI32 = AtomicI32::new(1);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get_or_insert_default()
      .insert(id, sender);
   let shown = call_static(c"choose", |env| {
      // SAFETY: `env` is this thread's, inside the caller's local frame.
      unsafe {
         let string_class = jni!(env, FindClass, c"java/lang/String".as_ptr());
         let array = jni!(
            env,
            NewObjectArray,
            jni_sys::jsize::try_from(options.len()).unwrap_or_default(),
            string_class,
            ptr::null_mut()
         );
         for (index, option) in options.iter().enumerate() {
            let option = jstring_of(env, option);
            jni!(
               env,
               SetObjectArrayElement,
               array,
               jni_sys::jsize::try_from(index).unwrap_or_default(),
               option
            );
         }
         (
            c"(Ljava/lang/Object;ILjava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V",
            vec![
               jvalue {
                  l: bridge::guest_host(),
               },
               jvalue { i: id },
               jvalue {
                  l: title.map_or(ptr::null_mut(), |title| jstring_of(env, title)),
               },
               jvalue {
                  l: message.map_or(ptr::null_mut(), |message| jstring_of(env, message)),
               },
               jvalue { l: array },
            ],
         )
      }
   });
   let forget = || {
      if let Some(choices) = CHOICES
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      {
         choices.remove(&id);
      }
   };
   if let Err(err) = shown {
      forget();
      return Err(err);
   }
   let chosen = receiver.recv().unwrap_or(-1);
   forget();
   Ok(chosen)
}

/// Shows `text` briefly at the bottom of the screen.
pub fn toast(text: &str) -> Result<(), String> {
   host::send(
      Method::SystemOverlayToast,
      &[(Field::Text, Value::Str(text.to_owned()))],
      Vec::new(),
   )
   .map_err(|err| err.to_string())
}

/// Copies `text` to the Android clipboard.
pub fn copy(text: &str) -> Result<(), String> {
   host::send(Method::ClipboardSetString, &[], text.as_bytes().to_vec())
      .map_err(|err| err.to_string())
}

/// Shows a scrollable selectable text/code dialog over Phoenix's activity and
/// waits for a button press (`0..options.len()`) or dismiss (-1).
pub fn view(
   title: Option<&str>,
   text: &str,
   monospace: bool,
   options: &[String],
) -> Result<i32, String> {
   static NEXT: AtomicI32 = AtomicI32::new(100_000);
   let id = NEXT.fetch_add(1, Ordering::Relaxed);
   let (sender, receiver) = mpsc::channel();
   CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .get_or_insert_default()
      .insert(id, sender);
   let shown = call_static(c"view", |env| {
      // SAFETY: `env` is this thread's, inside the caller's local frame.
      unsafe {
         let string_class = jni!(env, FindClass, c"java/lang/String".as_ptr());
         let array = jni!(
            env,
            NewObjectArray,
            jni_sys::jsize::try_from(options.len()).unwrap_or_default(),
            string_class,
            ptr::null_mut()
         );
         for (index, option) in options.iter().enumerate() {
            let option = jstring_of(env, option);
            jni!(
               env,
               SetObjectArrayElement,
               array,
               jni_sys::jsize::try_from(index).unwrap_or_default(),
               option
            );
         }
         (
            c"(Ljava/lang/Object;ILjava/lang/String;Ljava/lang/String;Z[Ljava/lang/String;)V",
            vec![
               jvalue {
                  l: bridge::guest_host(),
               },
               jvalue { i: id },
               jvalue {
                  l: title.map_or(ptr::null_mut(), |title| jstring_of(env, title)),
               },
               jvalue {
                  l: jstring_of(env, text),
               },
               jvalue { z: monospace },
               jvalue { l: array },
            ],
         )
      }
   });
   let forget = || {
      if let Some(choices) = CHOICES
         .lock()
         .unwrap_or_else(PoisonError::into_inner)
         .as_mut()
      {
         choices.remove(&id);
      }
   };
   if let Err(err) = shown {
      forget();
      return Err(err);
   }
   let chosen = receiver.recv().unwrap_or(-1);
   forget();
   Ok(chosen)
}

/// A Java string of `text`, with NULs dropped.
unsafe fn jstring_of(env: *mut JNIEnv, text: &str) -> jobject {
   let text = CString::new(text.replace('\0', "")).unwrap_or_default();
   // SAFETY: forwarded from the caller.
   unsafe { jni!(env, NewStringUTF, text.as_ptr()) }
}

/// Calls the static `phx.Picker.name` with the signature and arguments `args`
/// makes, in a local frame.
fn call_static(
   name: &core::ffi::CStr,
   args: impl FnOnce(*mut JNIEnv) -> (&'static core::ffi::CStr, Vec<jvalue>),
) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   // SAFETY: as in `pick`; every reference made here is local to the frame.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = (|| {
         let class = class(env)?;
         let (signature, args) = args(env);
         let method = jni!(
            env,
            GetStaticMethodID,
            class,
            name.as_ptr(),
            signature.as_ptr()
         );
         if method.is_null() {
            return Err(failure(
               env,
               &format!("phx.Picker.{} is missing", name.to_string_lossy()),
            ));
         }
         jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
         if jni!(env, ExceptionCheck) {
            return Err(failure(
               env,
               &format!("phx.Picker.{} threw", name.to_string_lossy()),
            ));
         }
         Ok(())
      })();
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Opens the document picker. The answer comes later through
/// [`crate::settings::picked`].
pub fn pick() {
   std::thread::spawn(|| crate::settings::picked(pick_file()));
}

/// Waits for the user to pick a file and reads it.
fn pick_file() -> Picked {
   let failed = |err: host::CallError| err.to_string();
   // Without `extensions` the picker takes any type, since pickers often file
   // `.js` under an unexpected MIME type.
   let picked = host::ask(
      Method::MediaPickerPick,
      &[
         (Field::Kind, Value::Str("files".to_owned())),
         (Field::Max, Value::I32(1)),
      ],
      Event::Picked,
      PICK_TIMEOUT,
   )
   .map_err(failed)?;
   if let Some(error) = host::field(&picked.args, Field::Error).and_then(Value::as_str) {
      return Err(error.to_owned());
   }
   let Some(Value::Arr(files)) = host::field(&picked.args, Field::Files) else {
      return Ok(None);
   };
   let Some(file) = files.first() else {
      return Ok(None);
   };
   let handle = host::field(file, Field::Handle)
      .and_then(Value::as_str)
      .ok_or("the picked file has no handle")?
      .to_owned();
   let name = host::field(file, Field::Name)
      .and_then(Value::as_str)
      .unwrap_or("plugin.js")
      .to_owned();

   let read = host::ask(
      Method::MediaPickerRead,
      &[(Field::Handle, Value::Str(handle.clone()))],
      Event::Read,
      READ_TIMEOUT,
   );
   let _ = host::send(
      Method::MediaPickerRelease,
      &[(Field::Handles, Value::Arr(vec![Value::Str(handle)]))],
      Vec::new(),
   );
   let read = read.map_err(failed)?;
   if let Some(error) = host::field(&read.args, Field::Error).and_then(Value::as_str) {
      return Err(error.to_owned());
   }
   Ok(Some((name, read.body)))
}

/// Opens `url` in the browser over Phoenix's activity.
pub fn open(url: &str) -> Result<(), String> {
   host::send(
      Method::LinkingOpenUrl,
      &[(Field::Url, Value::Str(url.to_owned()))],
      Vec::new(),
   )
   .map_err(|err| err.to_string())
}

/// Loads `phx.Picker` once and binds its native callback.
unsafe fn class(env: *mut JNIEnv) -> Result<jclass, String> {
   if let Some(&class) = CLASS.get() {
      return Ok(class as jclass);
   }
   let owner = bridge::guest_host();
   if owner.is_null() {
      return Err("Phoenix's UI bridge is not up yet".to_owned());
   }
   // SAFETY: forwarded from `pick`; `owner` is the live GuestHost global
   // reference and `DEX` is static, so the direct buffer never dangles.
   unsafe {
      let class_class = jni!(env, FindClass, c"java/lang/Class".as_ptr());
      let get_loader = jvm::method(
         env,
         class_class,
         c"getClassLoader",
         c"()Ljava/lang/ClassLoader;",
      );
      let owner_class = jni!(env, GetObjectClass, owner);
      let parent = jni!(env, CallObjectMethodA, owner_class, get_loader, ptr::null());
      let buffer = jni!(
         env,
         NewDirectByteBuffer,
         DEX.as_ptr().cast_mut().cast::<c_void>(),
         jlong::try_from(DEX.len()).expect("the picker dex is a few KB"),
      );
      let loader_class = jni!(
         env,
         FindClass,
         c"dalvik/system/InMemoryDexClassLoader".as_ptr()
      );
      let init = jvm::method(
         env,
         loader_class,
         c"<init>",
         c"(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
      );
      let args = [jvalue { l: buffer }, jvalue { l: parent }];
      let loader = jni!(env, NewObjectA, loader_class, init, args.as_ptr());
      if loader.is_null() || jni!(env, ExceptionCheck) {
         return Err(failure(env, "could not load the picker dex"));
      }
      let base = jni!(env, FindClass, c"java/lang/ClassLoader".as_ptr());
      let load = jvm::method(
         env,
         base,
         c"loadClass",
         c"(Ljava/lang/String;)Ljava/lang/Class;",
      );
      let name = jni!(env, NewStringUTF, c"phx.Picker".as_ptr());
      let class = jni!(
         env,
         CallObjectMethodA,
         loader,
         load,
         [jvalue { l: name }].as_ptr()
      );
      if class.is_null() || jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker is not in the dex"));
      }
      let natives = [JNINativeMethod {
         name:      c"onChosen".as_ptr().cast_mut(),
         signature: c"(II)V".as_ptr().cast_mut(),
         fnPtr:     on_chosen as *mut c_void,
      }];
      if jni!(env, RegisterNatives, class, natives.as_ptr(), 1) != 0 {
         return Err(failure(env, "could not bind phx.Picker's natives"));
      }
      let global = jni!(env, NewGlobalRef, class) as usize;
      Ok(*CLASS.get_or_init(|| global) as jclass)
   }
}

/// The pending exception's text, or `what` when there is none.
unsafe fn failure(env: *mut JNIEnv, what: &str) -> String {
   // SAFETY: forwarded from the caller.
   unsafe { jvm::take_exception(env) }
      .map_or_else(|| what.to_owned(), |thrown| format!("{what}: {thrown}"))
}

unsafe extern "system" fn on_chosen(_env: *mut JNIEnv, _class: jclass, id: jint, index: jint) {
   let waiter = CHOICES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .as_mut()
      .and_then(|choices| choices.remove(&id));
   if let Some(waiter) = waiter {
      let _ = waiter.send(index);
   }
}
