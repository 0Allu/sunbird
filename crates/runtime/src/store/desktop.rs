//! Read-only SQLite access over Desktop Phoenix's `cache.sqlite` via `rusqlite`.

use std::{
   path::Path,
   time::Duration,
};

use rusqlite::{
   Connection,
   OpenFlags,
   params_from_iter,
   types::ValueRef,
};

use super::{
   Cell,
   ROW_LIMIT,
   Rows,
   StoreError,
};

fn sql_err(err: rusqlite::Error) -> StoreError {
   StoreError::Sql {
      message: err.to_string(),
   }
}

pub fn query(database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
   let conn = Connection::open_with_flags(
      database,
      OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
   )
   .map_err(sql_err)?;
   let _ = conn.busy_timeout(Duration::from_secs(3));
   let mut stmt = conn.prepare(sql).map_err(sql_err)?;
   let columns = stmt
      .column_names()
      .into_iter()
      .map(str::to_owned)
      .collect::<Vec<_>>();
   let col_count = columns.len();

   let mut cursor = stmt
      .query(params_from_iter(args.iter().map(String::as_str)))
      .map_err(sql_err)?;
   let mut rows = Vec::new();
   while rows.len() < ROW_LIMIT
      && let Some(row) = cursor.next().map_err(sql_err)?
   {
      let mut cells = Vec::with_capacity(col_count);
      for index in 0..col_count {
         let cell = match row.get_ref(index).map_err(sql_err)? {
            ValueRef::Null => Cell::Null,
            ValueRef::Integer(value) => Cell::Integer(value),
            ValueRef::Real(value) => Cell::Float(value),
            ValueRef::Text(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
            ValueRef::Blob(bytes) => Cell::Blob(bytes.to_vec()),
         };
         cells.push(cell);
      }
      rows.push(cells);
   }
   Ok(Rows { columns, rows })
}

#[cfg(test)]
mod tests {
   use std::fs;

   use rusqlite::Connection;

   use super::super::{
      database,
      meta,
   };

   #[test]
   fn reads_phoenix_cache_schema() {
      let root = std::env::temp_dir().join(format!("sunbird-store-test-{}", std::process::id()));
      let account_dir = root.join("store").join("1234567890");
      fs::create_dir_all(&account_dir).unwrap();
      let db_path = account_dir.join("cache.sqlite");
      let _ = fs::remove_file(&db_path);

      let conn = Connection::open(&db_path).unwrap();
      conn.execute(
         "CREATE TABLE meta (k TEXT PRIMARY KEY NOT NULL, v BLOB NOT NULL) WITHOUT ROWID",
         [],
      )
      .unwrap();
      conn.execute(
         "INSERT INTO meta (k, v) VALUES (?, ?)",
         ("me", br#"{"id":"1234567890","username":"sunbird"}"#.as_slice()),
      )
      .unwrap();
      drop(conn);

      let found = database(&root).unwrap();
      assert_eq!(found, db_path);
      let me = meta(&found, "me").unwrap().unwrap();
      assert!(me.contains("sunbird"));

      let _ = fs::remove_dir_all(&root);
   }
}
