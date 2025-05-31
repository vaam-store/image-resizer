#[cfg(feature = "otel")]
mod init;

#[cfg(feature = "otel")]
pub use init::init_tracing;
