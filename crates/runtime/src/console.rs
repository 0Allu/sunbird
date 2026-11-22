//! The last lines plumage logged, kept for the Console page.

use std::{
   collections::VecDeque,
   sync::{
      Mutex,
      PoisonError,
   },
};

use log::{
   Log,
   Metadata,
   Record,
};

const CAPACITY: usize = 200;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Logs through `L` and keeps a copy of each line.
pub struct Tee<L>(pub L);

impl<L: Log> Log for Tee<L> {
   fn enabled(&self, metadata: &Metadata<'_>) -> bool {
      self.0.enabled(metadata)
   }

   fn log(&self, record: &Record<'_>) {
      if !self.enabled(record.metadata()) {
         return;
      }
      self.0.log(record);

      let line = format!(
         "{} {}",
         record.level().as_str().chars().next().unwrap_or('?'),
         record.args()
      );
      {
         let mut lines = LINES.lock().unwrap_or_else(PoisonError::into_inner);
         if lines.len() == CAPACITY {
            lines.pop_front();
         }
         lines.push_back(line);
      }
      // A warning from refreshing the page would refresh it again, forever.
      #[cfg(target_os = "android")]
      if !record.target().starts_with("plumage_runtime::settings") {
         crate::settings::log_changed();
      }
   }

   fn flush(&self) {
      self.0.flush();
   }
}

/// Every kept line, newest first.
pub fn lines() -> Vec<String> {
   LINES
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .iter()
      .rev()
      .cloned()
      .collect()
}
