//! Android's document picker and opening links, through `phx.Picker` (`java/phx/Picker.java`),
//! loaded from an in-memory dex under Phoenix's class loader. The chosen file
//! arrives on a Java thread and goes to [`crate::settings::picked`].

use std::{
   ffi::c_void,
   ptr,
   sync::OnceLock,
};

use jni_sys::{
   JNIEnv,
   JNINativeMethod,
   jbyteArray,
   jclass,
   jint,
   jlong,
   jstring,
   jvalue,
};

use crate::{
   bridge,
   jvm::{
      self,
      jni,
   },
};

const DEX: &[u8] = include_bytes!("../java/picker.dex");
/// Any type: pickers often file `.js` under an unexpected MIME type.
const MIME: &core::ffi::CStr = c"*/*";

/// The file the user chose, by name, or `None` when they backed out.
pub type Picked = Result<Option<(String, Vec<u8>)>, String>;

/// `phx.Picker` as a global reference.
static CLASS: OnceLock<usize> = OnceLock::new();

/// Opens the picker over Phoenix's activity. The answer comes later through
/// [`crate::settings::picked`].
pub fn pick() -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   // SAFETY: `env` is this thread's, and every reference made here is local
   // to the frame popped below, except the class kept as a global reference.
   unsafe {
      if jni!(env, PushLocalFrame, 16) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = call_pick(env);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// Opens `url` in the browser over Phoenix's activity.
pub fn open(url: &str) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   let url = std::ffi::CString::new(url).map_err(|err| err.to_string())?;
   // SAFETY: as in `pick`.
   unsafe {
      if jni!(env, PushLocalFrame, 8) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = call_open(env, &url);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

unsafe fn call_open(env: *mut JNIEnv, url: &core::ffi::CStr) -> Result<(), String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let class = class(env)?;
      let method = jni!(
         env,
         GetStaticMethodID,
         class,
         c"open".as_ptr(),
         c"(Ljava/lang/Object;Ljava/lang/String;)V".as_ptr(),
      );
      if method.is_null() {
         return Err(failure(env, "phx.Picker.open is missing"));
      }
      let args = [
         jvalue {
            l: bridge::guest_host(),
         },
         jvalue {
            l: jni!(env, NewStringUTF, url.as_ptr()),
         },
      ];
      jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker.open threw"));
      }
      Ok(())
   }
}

unsafe fn call_pick(env: *mut JNIEnv) -> Result<(), String> {
   // SAFETY: forwarded from `pick`.
   unsafe {
      let class = class(env)?;
      let method = jni!(
         env,
         GetStaticMethodID,
         class,
         c"pick".as_ptr(),
         c"(Ljava/lang/Object;ILjava/lang/String;)V".as_ptr(),
      );
      if method.is_null() {
         return Err(failure(env, "phx.Picker.pick is missing"));
      }
      let mime = jni!(env, NewStringUTF, MIME.as_ptr());
      let args = [
         jvalue {
            l: bridge::guest_host(),
         },
         jvalue { i: 0 },
         jvalue { l: mime },
      ];
      jni!(env, CallStaticVoidMethodA, class, method, args.as_ptr());
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker.pick threw"));
      }
      Ok(())
   }
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
         DEX.len() as jlong,
      );
      let loader_class = jni!(env, FindClass, c"dalvik/system/InMemoryDexClassLoader".as_ptr());
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
      let class = jni!(env, CallObjectMethodA, loader, load, [jvalue { l: name }].as_ptr());
      if class.is_null() || jni!(env, ExceptionCheck) {
         return Err(failure(env, "phx.Picker is not in the dex"));
      }
      let native = JNINativeMethod {
         name:      c"onPicked".as_ptr().cast_mut(),
         signature: c"(ILjava/lang/String;[BLjava/lang/String;)V".as_ptr().cast_mut(),
         fnPtr:     on_picked as *mut c_void,
      };
      if jni!(env, RegisterNatives, class, &raw const native, 1) != 0 {
         return Err(failure(env, "could not bind phx.Picker.onPicked"));
      }
      let global = jni!(env, NewGlobalRef, class) as usize;
      Ok(*CLASS.get_or_init(|| global) as jclass)
   }
}

/// The pending exception's text, or `what` when there is none.
unsafe fn failure(env: *mut JNIEnv, what: &str) -> String {
   // SAFETY: forwarded from the caller.
   unsafe { jvm::take_exception(env) }.map_or_else(|| what.to_owned(), |thrown| format!("{what}: {thrown}"))
}

unsafe extern "system" fn on_picked(
   env: *mut JNIEnv,
   _class: jclass,
   _id: jint,
   name: jstring,
   data: jbyteArray,
   error: jstring,
) {
   // SAFETY: Java passes live references or null, on its own attached thread.
   let picked = unsafe {
      match jvm::read_string(env, error) {
         Some(error) => Err(error),
         None => Ok(jvm::read_string(env, name).map(|name| (name, jvm::read_bytes(env, data, None)))),
      }
   };
   crate::settings::picked(picked);
}
