//! Read-only SQL over Phoenix's own cache,
//! `files/store/<account>/cache.sqlite`, through Android's `SQLiteDatabase` so
//! plumage ships no SQLite.

use std::{
   ffi::CString,
   fs,
   path::{
      Path,
      PathBuf,
   },
   ptr,
};

use jni_sys::{
   JNIEnv,
   jint,
   jobject,
   jvalue,
};
use serde_json::{
   Map,
   Value,
};

use crate::jvm::{
   clear_exception,
   env,
   jni,
   method,
   read_bytes,
   read_string,
   take_exception,
};

/// `SQLiteDatabase.OPEN_READONLY | NO_LOCALIZED_COLLATORS`.
const OPEN_FLAGS: jint = 0x01 | 0x10;
/// Keeps a careless `select *` from copying a whole message table into JS.
const ROW_LIMIT: usize = 10_000;
/// `Cursor.FIELD_TYPE_*`.
const FIELD_INTEGER: jint = 1;
const FIELD_FLOAT: jint = 2;
const FIELD_STRING: jint = 3;
const FIELD_BLOB: jint = 4;

#[derive(Debug, flub::Error)]
pub enum StoreError {
   #[error("no Phoenix store under {dir}")]
   Missing { dir: PathBuf },
   #[error("{message}")]
   Sql { message: String },
   #[error("a query argument or the query holds a NUL byte")]
   Nul,
   #[error("no Java VM is running in this process")]
   NoVm,
}

/// The cache of the account Phoenix wrote to last.
pub fn database(files: &Path) -> Result<PathBuf, StoreError> {
   let dir = files.join("store");
   fs::read_dir(&dir)
      .into_iter()
      .flatten()
      .filter_map(Result::ok)
      .map(|entry| entry.path().join("cache.sqlite"))
      .filter_map(|path| Some((fs::metadata(&path).ok()?.modified().ok()?, path)))
      .max()
      .map(|(_, path)| path)
      .ok_or(StoreError::Missing { dir })
}

/// Runs `sql` with `args` bound to its `?`s and returns each row as an object
/// keyed by column name.
pub fn query(
   database: &Path,
   sql: &str,
   args: &[String],
) -> Result<Vec<Map<String, Value>>, StoreError> {
   let env = env().ok_or(StoreError::NoVm)?;
   // SAFETY: `env` belongs to this thread, and every call below passes
   // references made on it with the argument types its signature names.
   unsafe {
      if jni!(env, PushLocalFrame, 64) != 0 {
         return Err(sql_error(env));
      }
      let result = run(env, database, sql, args);
      jni!(env, PopLocalFrame, ptr::null_mut());
      result
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame.
unsafe fn run(
   env: *mut JNIEnv,
   database: &Path,
   sql: &str,
   args: &[String],
) -> Result<Vec<Map<String, Value>>, StoreError> {
   // SAFETY: forwarded from the caller.
   unsafe {
      let string = |text: &str| -> Result<jobject, StoreError> {
         let text = CString::new(text).map_err(|_| StoreError::Nul)?;
         Ok(jni!(env, NewStringUTF, text.as_ptr()))
      };
      let db_class = jni!(
         env,
         FindClass,
         c"android/database/sqlite/SQLiteDatabase".as_ptr()
      );
      let string_class = jni!(env, FindClass, c"java/lang/String".as_ptr());
      if db_class.is_null() || string_class.is_null() {
         return Err(sql_error(env));
      }

      let open = jni!(
         env,
         GetStaticMethodID,
         db_class,
         c"openDatabase".as_ptr(),
         c"(Ljava/lang/String;Landroid/database/sqlite/SQLiteDatabase$CursorFactory;I)Landroid/database/sqlite/SQLiteDatabase;"
            .as_ptr()
      );
      let path = string(&database.to_string_lossy())?;
      let db = jni!(
         env,
         CallStaticObjectMethodA,
         db_class,
         open,
         [jvalue { l: path }, jvalue { l: ptr::null_mut() }, jvalue {
            i: OPEN_FLAGS,
         }]
         .as_ptr()
      );
      if db.is_null() || jni!(env, ExceptionCheck) {
         return Err(sql_error(env));
      }

      let count = jint::try_from(args.len()).map_err(|_| StoreError::Nul)?;
      let bound = jni!(env, NewObjectArray, count, string_class, ptr::null_mut());
      for (index, arg) in (0..).zip(args) {
         jni!(env, SetObjectArrayElement, bound, index, string(arg)?);
      }
      let raw_query = method(
         env,
         db_class,
         c"rawQuery",
         c"(Ljava/lang/String;[Ljava/lang/String;)Landroid/database/Cursor;",
      );
      let cursor = jni!(
         env,
         CallObjectMethodA,
         db,
         raw_query,
         [jvalue { l: string(sql)? }, jvalue { l: bound }].as_ptr()
      );
      let rows = if cursor.is_null() || jni!(env, ExceptionCheck) {
         Err(sql_error(env))
      } else {
         rows(env, cursor)
      };

      let close = method(env, db_class, c"close", c"()V");
      jni!(env, CallVoidMethodA, db, close, ptr::null());
      clear_exception(env);
      rows
   }
}

/// # Safety
///
/// `env` is the current thread's `JNIEnv` inside a local frame, and `cursor` a
/// live `Cursor`, closed here.
unsafe fn rows(env: *mut JNIEnv, cursor: jobject) -> Result<Vec<Map<String, Value>>, StoreError> {
   // SAFETY: forwarded from the caller. Each column value is a local ref
   // deleted after it is read, so a long result does not fill the frame.
   unsafe {
      let class = jni!(env, GetObjectClass, cursor);
      let column_count = method(env, class, c"getColumnCount", c"()I");
      let column_name = method(env, class, c"getColumnName", c"(I)Ljava/lang/String;");
      let move_next = method(env, class, c"moveToNext", c"()Z");
      let kind = method(env, class, c"getType", c"(I)I");
      let get_long = method(env, class, c"getLong", c"(I)J");
      let get_double = method(env, class, c"getDouble", c"(I)D");
      let get_string = method(env, class, c"getString", c"(I)Ljava/lang/String;");
      let get_blob = method(env, class, c"getBlob", c"(I)[B");
      let close = method(env, class, c"close", c"()V");

      let columns = jni!(env, CallIntMethodA, cursor, column_count, ptr::null());
      let names = (0..columns)
         .map(|column| {
            let name = jni!(
               env,
               CallObjectMethodA,
               cursor,
               column_name,
               [jvalue { i: column }].as_ptr()
            );
            let text = read_string(env, name).unwrap_or_default();
            jni!(env, DeleteLocalRef, name);
            text
         })
         .collect::<Vec<String>>();

      let mut out = Vec::new();
      while out.len() < ROW_LIMIT && jni!(env, CallBooleanMethodA, cursor, move_next, ptr::null()) {
         let mut row = Map::new();
         for (column, name) in (0..).zip(&names) {
            let arg = [jvalue { i: column }];
            let value = match jni!(env, CallIntMethodA, cursor, kind, arg.as_ptr()) {
               FIELD_INTEGER => {
                  Value::from(jni!(env, CallLongMethodA, cursor, get_long, arg.as_ptr()))
               },
               FIELD_FLOAT => {
                  Value::from(jni!(
                     env,
                     CallDoubleMethodA,
                     cursor,
                     get_double,
                     arg.as_ptr()
                  ))
               },
               FIELD_STRING => {
                  let text = jni!(env, CallObjectMethodA, cursor, get_string, arg.as_ptr());
                  let value = read_string(env, text).map_or(Value::Null, Value::from);
                  jni!(env, DeleteLocalRef, text);
                  value
               },
               FIELD_BLOB => {
                  let blob = jni!(env, CallObjectMethodA, cursor, get_blob, arg.as_ptr());
                  let value = Value::from(read_bytes(env, blob, None));
                  jni!(env, DeleteLocalRef, blob);
                  value
               },
               _ => Value::Null,
            };
            row.insert(name.clone(), value);
         }
         out.push(row);
      }
      let failed = jni!(env, ExceptionCheck);
      let error = failed.then(|| sql_error(env));
      jni!(env, CallVoidMethodA, cursor, close, ptr::null());
      clear_exception(env);
      error.map_or(Ok(out), Err)
   }
}

/// The pending exception as `Throwable.toString()`, cleared.
///
/// # Safety
///
/// `env` is the current thread's `JNIEnv`.
unsafe fn sql_error(env: *mut JNIEnv) -> StoreError {
   // SAFETY: forwarded from the caller.
   let message = unsafe { take_exception(env) };
   StoreError::Sql {
      message: message.unwrap_or_else(|| "a Java call failed".to_owned()),
   }
}
