//! HTTP through the platform's own stack, so requests follow the system's
//! certificates, proxy and VPN, and phoenix ships no TLS code.

#[cfg(target_os = "android")] mod android;
#[cfg(windows)] mod windows;

/// Plugin bundles are a few hundred KiB, so anything far larger is a wrong URL.
pub(super) const LIMIT: usize = 8 << 20;
pub(super) const TIMEOUT_MS: i32 = 15_000;

#[derive(Debug, flub::Error)]
pub enum FetchError {
   #[error("{url}: HTTP {status}")]
   Status { url: String, status: i32 },
   #[error("{url}: {message}")]
   Platform { url: String, message: String },
   #[error("{url}: larger than {LIMIT} bytes")]
   TooLarge { url: String },
   #[error("{url}: not an HTTP URL")]
   NotHttp { url: String },
   #[cfg(target_os = "android")]
   #[error("no Java VM is running in this process")]
   NoVm,
   #[cfg(not(any(target_os = "android", windows)))]
   #[error("{url}: HTTP fetch is not implemented on this platform")]
   Unsupported { url: String },
}

pub struct Request {
   pub url:     String,
   pub method:  String,
   pub headers: Vec<(String, String)>,
   pub body:    Option<Vec<u8>>,
}

pub struct Response {
   pub status: i32,
   pub body:   Vec<u8>,
}

/// The body of a 2xx answer to a GET.
pub fn get(url: &str) -> Result<Vec<u8>, FetchError> {
   let response = send(&Request {
      url:     url.to_owned(),
      method:  "GET".to_owned(),
      headers: Vec::new(),
      body:    None,
   })?;
   if !(200..300).contains(&response.status) {
      return Err(FetchError::Status {
         url:    url.to_owned(),
         status: response.status,
      });
   }
   Ok(response.body)
}

/// Sends `request` through the platform's native HTTP stack.
pub fn send(request: &Request) -> Result<Response, FetchError> {
   #[cfg(target_os = "android")]
   {
      android::send(request)
   }
   #[cfg(windows)]
   {
      windows::send(request)
   }
   #[cfg(not(any(target_os = "android", windows)))]
   {
      Err(FetchError::Unsupported {
         url: request.url.clone(),
      })
   }
}
