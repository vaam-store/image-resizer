//! HMAC-SHA256 signed URLs (#27), matching imgproxy's scheme closely enough
//! that a client library written for imgproxy works against this service:
//! `signature = base64url_nopad(HMAC-SHA256(key, salt || signed_path))`,
//! where `signed_path` is the request path exactly as received (still
//! percent-encoded, leading `/` included) with the signature segment itself
//! stripped off - see [`crate::modules::url::split`] for how that path is
//! derived, and [`verify::verify_signature`] for the constant-time check.

pub mod config;
pub mod verify;

pub use config::SigningConfig;
