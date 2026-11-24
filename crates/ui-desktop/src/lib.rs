//! Desktop UI host primitives for Phoenix (Windows / macOS / Linux).
//!
//! Desktop Phoenix renders natively in Rust with GPUI rather than Android's
//! serialized UI-op bridge (`phoenix-ui-mobile`). This crate owns the
//! desktop-side UI interactions: clipboard, opening URLs in the system browser,
//! notifications/toasts, and future GPUI surface hooks.

#[cfg(windows)] mod windows;

#[derive(Debug, flub::Error)]
pub enum Error {
   #[error("desktop ui: {message}")]
   Platform { message: String },
   #[error("desktop ui: unsupported on this target")]
   Unsupported,
}

/// Copies `text` to the system clipboard.
pub fn copy(text: &str) -> Result<(), Error> {
   #[cfg(windows)]
   {
      windows::copy(text)
   }
   #[cfg(not(windows))]
   {
      let _ = text;
      Err(Error::Unsupported)
   }
}

/// Opens `url` in the system's default browser / handler.
pub fn open_url(url: &str) -> Result<(), Error> {
   if !url.starts_with("https://") && !url.starts_with("http://") && !url.starts_with("discord://")
   {
      return Err(Error::Platform {
         message: format!("refusing to open non-web URL: {url}"),
      });
   }
   #[cfg(windows)]
   {
      windows::open_url(url)
   }
   #[cfg(not(windows))]
   {
      let _ = url;
      Err(Error::Unsupported)
   }
}

/// Shows a short desktop toast / notification message.
pub fn toast(message: &str) {
   log::info!("[toast] {message}");
}

/// Shows a titled desktop notification.
pub fn notify(title: &str, body: Option<&str>) {
   match body {
      Some(body) if !body.is_empty() => log::info!("[notify] {title}: {body}"),
      _ => log::info!("[notify] {title}"),
   }
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn rejects_unsafe_url_schemes() {
      assert!(open_url("file:///C:/Windows/notepad.exe").is_err());
      assert!(open_url("javascript:alert(1)").is_err());
   }
}
