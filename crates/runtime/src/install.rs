//! Opens Android's installer on an APK in the app's `files` directory, shared
//! through the `FileProvider` Discord declares at `<package>.file-provider`,
//! a name the patcher's package rename keeps.

use std::{
   ffi::CString,
   path::Path,
   ptr,
};

use jni_sys::{
   JNIEnv,
   jobject,
   jvalue,
};

use crate::{
   bridge,
   jvm::{
      self,
      jni,
      method,
      signature,
   },
};

const FLAG_GRANT_READ_URI_PERMISSION: i32 = 0x0000_0001;
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

pub fn open(apk: &Path) -> Result<(), String> {
   let env = jvm::env().ok_or("no JNI env")?;
   let apk = CString::new(apk.to_str().ok_or("the update path is not UTF-8")?)
      .map_err(|err| err.to_string())?;
   // SAFETY: `env` is this thread's, and every reference made here is local
   // to the frame popped below.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(failure(env, "out of local references"));
      }
      let result = application(env).and_then(|app| {
         let uri = content_uri(env, app, &apk)?;
         start_view(env, app, uri)
      });
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// The app's `Application`.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`, inside a local frame.
unsafe fn application(env: *mut JNIEnv) -> Result<jobject, String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let thread = jni!(env, FindClass, c"android/app/ActivityThread".as_ptr());
      let current = jni!(
         env,
         GetStaticMethodID,
         thread,
         c"currentApplication".as_ptr(),
         signature!(() -> android::app::Application).as_ptr()
      );
      checked(
         env,
         jni!(env, CallStaticObjectMethodA, thread, current, ptr::null()),
         "no Application",
      )
   }
}

/// A `content://` URI for `apk` that the installer may read.
///
/// # Safety
///
/// As [`application`], with `app` the `Application`.
unsafe fn content_uri(env: *mut JNIEnv, app: jobject, apk: &CString) -> Result<jobject, String> {
   // SAFETY: forwarded from `open`. Each call's exception is checked before
   // its result is used.
   unsafe {
      let app_class = jni!(env, GetObjectClass, app);
      let package_name = method(env, app_class, c"getPackageName", signature!(() -> String));
      let package = checked(
         env,
         jni!(env, CallObjectMethodA, app, package_name, ptr::null()),
         "no package name",
      )?;
      let package = jvm::read_string(env, package).ok_or("no package name")?;
      let authority =
         CString::new(format!("{package}.file-provider")).map_err(|err| err.to_string())?;

      let file_class = jni!(env, FindClass, c"java/io/File".as_ptr());
      let file_init = method(env, file_class, c"<init>", signature!((String) -> void));
      let file = checked(
         env,
         jni!(
            env,
            NewObjectA,
            file_class,
            file_init,
            [jvalue {
               l: jni!(env, NewStringUTF, apk.as_ptr()),
            }]
            .as_ptr()
         ),
         "could not make a File",
      )?;

      let provider = bridge::app_class(env, c"androidx.core.content.FileProvider")
         .ok_or("Discord has no androidx FileProvider")?;
      let uri_for_file = jni!(
         env,
         GetStaticMethodID,
         provider,
         c"getUriForFile".as_ptr(),
         signature!((android::content::Context, String, java::io::File) -> android::net::Uri)
            .as_ptr()
      );
      checked(
         env,
         jni!(
            env,
            CallStaticObjectMethodA,
            provider,
            uri_for_file,
            [
               jvalue { l: app },
               jvalue {
                  l: jni!(env, NewStringUTF, authority.as_ptr()),
               },
               jvalue { l: file },
            ]
            .as_ptr()
         ),
         "the FileProvider does not share the update",
      )
   }
}

/// Starts the installer's activity on `uri`.
///
/// # Safety
///
/// As [`content_uri`], with `uri` a `content://` URI.
unsafe fn start_view(env: *mut JNIEnv, app: jobject, uri: jobject) -> Result<(), String> {
   // SAFETY: forwarded from `open`.
   unsafe {
      let intent_class = jni!(env, FindClass, c"android/content/Intent".as_ptr());
      let intent_init = method(env, intent_class, c"<init>", signature!((String) -> void));
      let intent = checked(
         env,
         jni!(
            env,
            NewObjectA,
            intent_class,
            intent_init,
            [jvalue {
               l: jni!(env, NewStringUTF, c"android.intent.action.VIEW".as_ptr()),
            }]
            .as_ptr()
         ),
         "could not make an Intent",
      )?;
      let set_data = method(
         env,
         intent_class,
         c"setDataAndType",
         signature!((android::net::Uri, String) -> android::content::Intent),
      );
      let kind = jni!(
         env,
         NewStringUTF,
         c"application/vnd.android.package-archive".as_ptr()
      );
      jni!(
         env,
         CallObjectMethodA,
         intent,
         set_data,
         [jvalue { l: uri }, jvalue { l: kind }].as_ptr()
      );
      let add_flags = method(
         env,
         intent_class,
         c"addFlags",
         signature!((int) -> android::content::Intent),
      );
      jni!(
         env,
         CallObjectMethodA,
         intent,
         add_flags,
         [jvalue {
            i: FLAG_GRANT_READ_URI_PERMISSION | FLAG_ACTIVITY_NEW_TASK,
         }]
         .as_ptr()
      );
      let start = method(
         env,
         jni!(env, GetObjectClass, app),
         c"startActivity",
         signature!((android::content::Intent) -> void),
      );
      jni!(
         env,
         CallVoidMethodA,
         app,
         start,
         [jvalue { l: intent }].as_ptr()
      );
      if jni!(env, ExceptionCheck) {
         return Err(failure(env, "the installer did not open"));
      }
      Ok(())
   }
}

/// `object` unless the call that made it threw or returned null.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn checked(env: *mut JNIEnv, object: jobject, what: &str) -> Result<jobject, String> {
   // SAFETY: forwarded from the caller.
   unsafe {
      if jni!(env, ExceptionCheck) || object.is_null() {
         return Err(failure(env, what));
      }
   }
   Ok(object)
}

/// The pending exception's text, or `what` when there is none.
unsafe fn failure(env: *mut JNIEnv, what: &str) -> String {
   // SAFETY: forwarded from the caller.
   unsafe { jvm::take_exception(env) }
      .map_or_else(|| what.to_owned(), |thrown| format!("{what}: {thrown}"))
}
