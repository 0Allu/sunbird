//! HTTP GET through the platform's own stack, so downloads follow the system's
//! certificates, proxy and VPN, and plumage ships no TLS code.

use std::{
   ffi::{
      CStr,
      CString,
   },
   ptr,
};

use jni_sys::{
   JNIEnv,
   jclass,
   jmethodID,
   jobject,
   jvalue,
};

use crate::jvm::{
   env,
   jni,
   read_bytes,
   read_string,
};

/// Plugin bundles are a few hundred KiB, so anything far larger is a wrong URL.
const LIMIT: usize = 8 << 20;

#[derive(Debug, flub::Error)]
pub enum FetchError {
   #[error("{url}: HTTP {status}")]
   Status { url: String, status: i32 },
   #[error("{url}: {message}")]
   Java { url: String, message: String },
   #[error("{url}: larger than {LIMIT} bytes")]
   TooLarge { url: String },
   #[error("{url}: not an HTTP URL")]
   NotHttp { url: String },
   #[error("no Java VM is running in this process")]
   NoVm,
}

const TIMEOUT_MS: i32 = 15_000;
const CHUNK: i32 = 64 << 10;

/// Runs on a worker thread. The local frame frees every reference this
/// creates, since the thread never returns to Java to free them.
pub fn get(url: &str) -> Result<Vec<u8>, FetchError> {
   let env = env().ok_or(FetchError::NoVm)?;
   // SAFETY: `env` belongs to this thread, and every call below passes
   // references made on it with the argument types its signature names.
   unsafe {
      if jni!(env, PushLocalFrame, 32) != 0 {
         return Err(java_error(env, url));
      }
      let result = request(env, url);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn request(env: *mut JNIEnv, url: &str) -> Result<Vec<u8>, FetchError> {
   // SAFETY: forwarded from the caller.
   unsafe {
      let (connection, http_class) = connect(env, url)?;
      let stream_class = jni!(env, FindClass, c"java/io/InputStream".as_ptr());
      let input = method(
         env,
         http_class,
         c"getInputStream",
         c"()Ljava/io/InputStream;",
      );
      let stream = jni!(env, CallObjectMethodA, connection, input, ptr::null());
      if stream_class.is_null() || stream.is_null() {
         return Err(java_error(env, url));
      }

      let read = method(env, stream_class, c"read", c"([B)I");
      let buffer = jni!(env, NewByteArray, CHUNK);
      let mut body = Vec::new();
      let outcome = loop {
         let count = jni!(
            env,
            CallIntMethodA,
            stream,
            read,
            [jvalue { l: buffer }].as_ptr()
         );
         if jni!(env, ExceptionCheck) {
            break Err(java_error(env, url));
         }
         if count < 0 {
            break Ok(());
         }
         body.extend(read_bytes(env, buffer, Some(count)));
         if body.len() > LIMIT {
            break Err(FetchError::TooLarge {
               url: url.to_owned(),
            });
         }
      };

      let close = method(env, stream_class, c"close", c"()V");
      let disconnect = method(env, http_class, c"disconnect", c"()V");
      jni!(env, CallVoidMethodA, stream, close, ptr::null());
      jni!(env, CallVoidMethodA, connection, disconnect, ptr::null());
      jni!(env, ExceptionClear);
      outcome.map(|()| body)
   }
}

/// An `HttpURLConnection` to `url` that answered 2xx, and its class.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn connect(env: *mut JNIEnv, url: &str) -> Result<(jobject, jclass), FetchError> {
   let not_http = || {
      FetchError::NotHttp {
         url: url.to_owned(),
      }
   };
   // SAFETY: forwarded from the caller.
   unsafe {
      let url_class = jni!(env, FindClass, c"java/net/URL".as_ptr());
      let http_class = jni!(env, FindClass, c"java/net/HttpURLConnection".as_ptr());
      if url_class.is_null() || http_class.is_null() {
         return Err(java_error(env, url));
      }

      let text = CString::new(url).map_err(|_| not_http())?;
      let url_string = jni!(env, NewStringUTF, text.as_ptr());
      let init = method(env, url_class, c"<init>", c"(Ljava/lang/String;)V");
      let url_object = jni!(
         env,
         NewObjectA,
         url_class,
         init,
         [jvalue { l: url_string }].as_ptr()
      );
      if url_object.is_null() {
         return Err(java_error(env, url));
      }

      let open = method(
         env,
         url_class,
         c"openConnection",
         c"()Ljava/net/URLConnection;",
      );
      let connection = jni!(env, CallObjectMethodA, url_object, open, ptr::null());
      if connection.is_null() {
         return Err(java_error(env, url));
      }
      if !jni!(env, IsInstanceOf, connection, http_class) {
         return Err(not_http());
      }

      for name in [c"setConnectTimeout", c"setReadTimeout"] {
         let set = method(env, http_class, name, c"(I)V");
         jni!(
            env,
            CallVoidMethodA,
            connection,
            set,
            [jvalue { i: TIMEOUT_MS }].as_ptr()
         );
      }
      let code = method(env, http_class, c"getResponseCode", c"()I");
      let status = jni!(env, CallIntMethodA, connection, code, ptr::null());
      if jni!(env, ExceptionCheck) {
         return Err(java_error(env, url));
      }
      if !(200..300).contains(&status) {
         let disconnect = method(env, http_class, c"disconnect", c"()V");
         jni!(env, CallVoidMethodA, connection, disconnect, ptr::null());
         return Err(FetchError::Status {
            url: url.to_owned(),
            status,
         });
      }
      Ok((connection, http_class))
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv`, and `class` a live class.
unsafe fn method(env: *mut JNIEnv, class: jclass, name: &CStr, signature: &CStr) -> jmethodID {
   // SAFETY: forwarded from the caller, and both strings are NUL-terminated.
   unsafe { jni!(env, GetMethodID, class, name.as_ptr(), signature.as_ptr()) }
}

/// The pending exception as `Throwable.toString()`, cleared.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn java_error(env: *mut JNIEnv, url: &str) -> FetchError {
   // SAFETY: forwarded from the caller. The exception is cleared before
   // `toString` runs, as JNI requires.
   let message = unsafe {
      let thrown: jobject = jni!(env, ExceptionOccurred);
      jni!(env, ExceptionClear);
      if thrown.is_null() {
         None
      } else {
         let class = jni!(env, GetObjectClass, thrown);
         let to_string = method(env, class, c"toString", c"()Ljava/lang/String;");
         let text = jni!(env, CallObjectMethodA, thrown, to_string, ptr::null());
         jni!(env, ExceptionClear);
         read_string(env, text)
      }
   };
   FetchError::Java {
      url:     url.to_owned(),
      message: message.unwrap_or_else(|| "a Java call failed".to_owned()),
   }
}
