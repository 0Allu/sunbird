//! Plumbing for calling Java from any thread, shared by the UI bridge and
//! fetches.
//!
//! Every call goes through [`SAVED`], the JNI table as ART had it before the
//! bridge swapped two of its slots, so phoenix never sees its own calls.

use std::{
   ffi::{
      CStr,
      c_void,
   },
   ptr,
   sync::{
      OnceLock,
      atomic::{
         AtomicPtr,
         Ordering,
      },
   },
};

use jni_sys::{
   JNI_OK,
   JNI_VERSION_1_6,
   JNIEnv,
   JNINativeInterface__1_6,
   JavaVM,
   jbyte,
   jbyteArray,
   jclass,
   jint,
   jmethodID,
   jsize,
   jstring,
};

pub struct Table(pub JNINativeInterface__1_6);

// SAFETY: a copy of ART's table holds function pointers into libart, which are
// valid on every thread.
unsafe impl Send for Table {}
// SAFETY: the copy is never written after it is saved.
unsafe impl Sync for Table {}

pub static SAVED: OnceLock<Table> = OnceLock::new();

macro_rules! jni {
   ($env:expr, $name:ident $(, $arg:expr)* $(,)?) => {
      ($crate::jvm::table($env).$name)($env $(, $arg)*)
   };
}

/// A JNI method descriptor as a `&'static CStr`, written with Java's types:
/// `signature!((String, [byte]) -> boolean)` is `c"(Ljava/lang/String;[B)Z"`.
/// Other classes go by their paths with `::`, and nested classes follow a
/// `.`, so `android::database::sqlite::SQLiteDatabase.CursorFactory` is
/// `SQLiteDatabase$CursorFactory`.
macro_rules! signature {
   (($($args:tt)*) -> $($ret:tt)+) => {
      $crate::jvm::signature!(@type ["(",] [$($args)* @close $($ret)+])
   };
   (@type [$($done:tt)*] []) => {
      const {
         match ::core::ffi::CStr::from_bytes_with_nul(concat!($($done)* "\0").as_bytes()) {
            Ok(signature) => signature,
            Err(_) => panic!("signature! has a NUL inside"),
         }
      }
   };
   (@type [$($done:tt)*] [, $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)*] [$($rest)*])
   };
   (@type [$($done:tt)*] [@close $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* ")",] [$($rest)*])
   };
   (@type [$($done:tt)*] [[$($inner:tt)+] $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "[",] [$($inner)+ $($rest)*])
   };
   (@type [$($done:tt)*] [void $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "V",] [$($rest)*])
   };
   (@type [$($done:tt)*] [boolean $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "Z",] [$($rest)*])
   };
   (@type [$($done:tt)*] [byte $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "B",] [$($rest)*])
   };
   (@type [$($done:tt)*] [int $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "I",] [$($rest)*])
   };
   (@type [$($done:tt)*] [long $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "J",] [$($rest)*])
   };
   (@type [$($done:tt)*] [double $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "D",] [$($rest)*])
   };
   (@type [$($done:tt)*] [String $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "Ljava/lang/String;",] [$($rest)*])
   };
   (@type [$($done:tt)*] [Object $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "Ljava/lang/Object;",] [$($rest)*])
   };
   (@type [$($done:tt)*] [$package:ident :: $($rest:tt)*]) => {
      $crate::jvm::signature!(@path [$($done)* "L", stringify!($package),] [$($rest)*])
   };
   (@path [$($done:tt)*] [$package:ident :: $($rest:tt)*]) => {
      $crate::jvm::signature!(@path [$($done)* "/", stringify!($package),] [$($rest)*])
   };
   (@path [$($done:tt)*] [$class:ident . $($rest:tt)*]) => {
      $crate::jvm::signature!(@nested [$($done)* "/", stringify!($class),] [$($rest)*])
   };
   (@path [$($done:tt)*] [$class:ident $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "/", stringify!($class), ";",] [$($rest)*])
   };
   (@nested [$($done:tt)*] [$class:ident . $($rest:tt)*]) => {
      $crate::jvm::signature!(@nested [$($done)* "$", stringify!($class),] [$($rest)*])
   };
   (@nested [$($done:tt)*] [$class:ident $($rest:tt)*]) => {
      $crate::jvm::signature!(@type [$($done)* "$", stringify!($class), ";",] [$($rest)*])
   };
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
pub unsafe fn table(env: *mut JNIEnv) -> &'static JNINativeInterface__1_6 {
   SAVED.get().map_or_else(
      // SAFETY: forwarded from the caller. ART's table lives for the whole
      // process and starts with the JNI 1.6 layout.
      || unsafe { &(**env).v1_6 },
      |saved| &saved.0,
   )
}

pub fn vm() -> *mut JavaVM {
   type GetCreatedVms = unsafe extern "system" fn(*mut *mut JavaVM, jsize, *mut jsize) -> jint;
   static VM: AtomicPtr<JavaVM> = AtomicPtr::new(ptr::null_mut());

   let cached = VM.load(Ordering::Acquire);
   if !cached.is_null() {
      return cached;
   }

   // SAFETY: the names are NUL-terminated, and the symbol, when present, is
   // JNI's `JNI_GetCreatedJavaVMs` with this signature.
   unsafe {
      let mut symbol = libc::dlsym(libc::RTLD_DEFAULT, c"JNI_GetCreatedJavaVMs".as_ptr());
      if symbol.is_null() {
         let handle = libc::dlopen(c"libnativehelper.so".as_ptr(), libc::RTLD_NOW);
         if !handle.is_null() {
            symbol = libc::dlsym(handle, c"JNI_GetCreatedJavaVMs".as_ptr());
         }
      }
      if symbol.is_null() {
         return ptr::null_mut();
      }

      let get_created = std::mem::transmute::<*mut c_void, GetCreatedVms>(symbol);
      let (mut vm, mut count) = (ptr::null_mut(), 0);
      if get_created(&raw mut vm, 1, &raw mut count) != JNI_OK || count < 1 {
         return ptr::null_mut();
      }
      VM.store(vm, Ordering::Release);
      vm
   }
}

/// The current thread's `JNIEnv`, attaching the thread if needed. Attached
/// threads stay attached.
pub fn env() -> Option<*mut JNIEnv> {
   let vm = vm();
   if vm.is_null() {
      return None;
   }
   let mut env = ptr::null_mut();
   // SAFETY: `vm` is the process's live JavaVM, and both calls only write
   // `env`.
   unsafe {
      if ((**vm).v1_2.GetEnv)(vm, &raw mut env, JNI_VERSION_1_6) == JNI_OK {
         return Some(env.cast());
      }
      (((**vm).v1_2.AttachCurrentThread)(vm, &raw mut env, ptr::null_mut()) == JNI_OK)
         .then_some(env.cast())
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
pub unsafe fn clear_exception(env: *mut JNIEnv) -> bool {
   // SAFETY: forwarded from the caller.
   unsafe {
      let pending = jni!(env, ExceptionCheck);
      if pending {
         jni!(env, ExceptionClear);
      }
      pending
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `array` is null or a live
/// `byte[]`.
pub unsafe fn read_bytes(env: *mut JNIEnv, array: jbyteArray, limit: Option<jsize>) -> Vec<u8> {
   if array.is_null() {
      return Vec::new();
   }
   // SAFETY: forwarded from the caller. The region stays within the array's
   // length and the buffer's.
   unsafe {
      let mut len = jni!(env, GetArrayLength, array).max(0);
      if let Some(limit) = limit
         && (0..len).contains(&limit)
      {
         len = limit;
      }
      let mut out = vec![0u8; usize::try_from(len).unwrap_or(0)];
      jni!(
         env,
         GetByteArrayRegion,
         array,
         0,
         len,
         out.as_mut_ptr().cast::<jbyte>()
      );
      out
   }
}

/// A new local `byte[]` holding `bytes`, and its length.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
pub unsafe fn new_bytes(env: *mut JNIEnv, bytes: &[u8]) -> Option<(jbyteArray, jsize)> {
   let Ok(len) = jsize::try_from(bytes.len()) else {
      log::warn!(
         "jvm: a {} byte frame does not fit a Java array",
         bytes.len()
      );
      return None;
   };
   // SAFETY: forwarded from the caller. The region covers exactly `bytes`.
   unsafe {
      let array = jni!(env, NewByteArray, len);
      if array.is_null() {
         clear_exception(env);
         log::warn!("jvm: could not allocate a {len} byte array");
         return None;
      }
      jni!(
         env,
         SetByteArrayRegion,
         array,
         0,
         len,
         bytes.as_ptr().cast::<jbyte>()
      );
      Some((array, len))
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `string` is null or a live
/// `String`.
pub unsafe fn read_string(env: *mut JNIEnv, string: jstring) -> Option<String> {
   if string.is_null() {
      return None;
   }
   // SAFETY: forwarded from the caller. The chars are NUL-terminated modified
   // UTF-8 and released after the copy.
   unsafe {
      let chars = jni!(env, GetStringUTFChars, string, ptr::null_mut());
      if chars.is_null() {
         clear_exception(env);
         return None;
      }
      let owned = CStr::from_ptr(chars).to_string_lossy().into_owned();
      jni!(env, ReleaseStringUTFChars, string, chars);
      Some(owned)
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `class` a live class.
pub unsafe fn method(env: *mut JNIEnv, class: jclass, name: &CStr, signature: &CStr) -> jmethodID {
   // SAFETY: forwarded from the caller, and both strings are NUL-terminated.
   unsafe { jni!(env, GetMethodID, class, name.as_ptr(), signature.as_ptr()) }
}

/// The pending exception as `Throwable.toString()`, cleared. None when nothing
/// was thrown or the message could not be read.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
pub unsafe fn take_exception(env: *mut JNIEnv) -> Option<String> {
   // SAFETY: forwarded from the caller. The exception is cleared before
   // `toString` runs, as JNI requires.
   unsafe {
      let thrown = jni!(env, ExceptionOccurred);
      jni!(env, ExceptionClear);
      if thrown.is_null() {
         return None;
      }
      let class = jni!(env, GetObjectClass, thrown);
      let to_string = method(env, class, c"toString", signature!(() -> String));
      let text = jni!(env, CallObjectMethodA, thrown, to_string, ptr::null());
      jni!(env, ExceptionClear);
      read_string(env, text)
   }
}

pub(crate) use jni;
pub(crate) use signature;
