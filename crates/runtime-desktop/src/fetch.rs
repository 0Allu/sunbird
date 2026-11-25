//! `send` for the platform's HTTP stack. Both backends use the OS's TLS
//! certificates and proxy settings instead of bundling a TLS library.

#[cfg(target_os = "linux")] mod curl;
#[cfg(windows)] mod winhttp;

#[cfg(target_os = "linux")] pub use curl::send;
#[cfg(windows)] pub use winhttp::send;
