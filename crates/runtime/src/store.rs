//! Read-only SQL over Phoenix's cache, `<files>/store/<account>/cache.sqlite`.
//!
//! Every Phoenix shares the `(k TEXT PRIMARY KEY NOT NULL, v BLOB NOT NULL)
//! WITHOUT ROWID` schema, and the platform runs the query.

use std::{
   fs,
   path::{
      Path,
      PathBuf,
   },
};

use serde::{
   Serialize,
   Serializer,
   ser::{
      SerializeMap,
      SerializeSeq,
   },
};

/// Keeps a careless `select *` from copying a whole message table into JS.
pub const ROW_LIMIT: usize = 10_000;

/// One column of one row.
#[derive(Serialize)]
#[serde(untagged)]
pub enum Cell {
   Null,
   Integer(i64),
   Float(f64),
   Text(String),
   Blob(Vec<u8>),
}

impl Cell {
   /// The text of a string column, or of a BLOB holding UTF-8, which is how
   /// Phoenix stores its JSON values.
   #[must_use]
   pub fn text(&self) -> Option<&str> {
      match self {
         Self::Text(text) => Some(text),
         Self::Blob(bytes) => str::from_utf8(bytes).ok(),
         Self::Null | Self::Integer(_) | Self::Float(_) => None,
      }
   }
}

/// A query result. Serializes as an array of objects keyed by column name.
pub struct Rows {
   pub columns: Vec<String>,
   pub rows:    Vec<Vec<Cell>>,
}

impl Rows {
   /// The `column` of the first row.
   #[must_use]
   pub fn first(&self, column: &str) -> Option<&Cell> {
      let at = self.columns.iter().position(|name| name == column)?;
      self.rows.first()?.get(at)
   }
}

impl Serialize for Rows {
   fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
      struct Row<'a>(&'a [String], &'a [Cell]);

      impl Serialize for Row<'_> {
         fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (name, cell) in self.0.iter().zip(self.1) {
               map.serialize_entry(name, cell)?;
            }
            map.end()
         }
      }

      let mut seq = serializer.serialize_seq(Some(self.rows.len()))?;
      for row in &self.rows {
         seq.serialize_element(&Row(&self.columns, row))?;
      }
      seq.end()
   }
}

#[derive(Debug, flub::Error)]
pub enum StoreError {
   #[error("no Phoenix store under {dir}")]
   Missing { dir: PathBuf },
   #[error("{message}")]
   Sql { message: String },
   #[error("phoenix has not started")]
   NotStarted,
   #[error(transparent)]
   Backend(Box<dyn std::error::Error + Send + Sync>),
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

/// The JSON in row `key` of Phoenix's `meta` table, like `me` or `selected`.
pub fn meta(database: &Path, key: &str) -> Result<Option<String>, StoreError> {
   let rows = query(
      database,
      "SELECT v FROM meta WHERE k = ?",
      &[key.to_owned()],
   )?;
   Ok(rows.first("v").and_then(Cell::text).map(str::to_owned))
}

/// Runs `sql` with `args` bound to its `?`s.
pub fn query(database: &Path, sql: &str, args: &[String]) -> Result<Rows, StoreError> {
   crate::platform()
      .ok_or(StoreError::NotStarted)?
      .query(database, sql, args)
}
