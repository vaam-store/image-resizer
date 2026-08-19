//! Shared cache-key validation for every [`StorageBackend`](super::core::StorageBackend) (#23).
//!
//! `key` arrives from an untrusted request path parameter (`DownloadPathParams::key`)
//! and is joined directly onto backend-specific storage roots: a local filesystem
//! path (`base_path.join(key)`) or an S3 object key. Neither join is safe against an
//! arbitrary string - `PathBuf::join` silently discards `base_path` entirely when
//! `key` is an absolute path, and on S3 an unvalidated key is an IDOR across the
//! whole bucket namespace. Every backend must therefore be protected by the *same*
//! check, applied once, before any backend is touched - not re-implemented per
//! backend where it's easy for one implementation to drift or be forgotten.
//!
//! Valid keys look exactly like what `CacheService::generate_key` produces:
//! `<STORAGE_SUB_PATH prefix><64 lowercase hex chars>.<jpg|png|webp>`. Anything else
//! - traversal (`../..`), absolute paths (`/etc/passwd`), percent-decoded forms of
//! either, wrong-length or non-hex hashes, disallowed extensions - is rejected.

use std::fmt;

/// Length in characters of the SHA-256 hex digest `CacheService::generate_key` emits.
const HASH_LEN: usize = 64;

/// Extensions `CacheService::generate_key` can produce (mirrors `ImageFormat`'s
/// `Display` impl in `packages/gen-server/src/models.rs`).
const ALLOWED_EXTENSIONS: [&str; 3] = ["jpg", "png", "webp"];

/// A cache key that failed validation.
///
/// Deliberately a distinct, named `std::error::Error` type (rather than a bare
/// `anyhow!(...)`) so a caller above the storage/resize layer can
/// `downcast_ref::<InvalidKeyError>()` the returned `anyhow::Error` and map it to
/// `404 Not Found` instead of a generic `500`, without the storage layer needing to
/// know anything about HTTP.
///
/// `Display` deliberately reads as "not found" rather than "invalid key": today
/// `AppError::classify_download_error` (`src/modules/utils/err.rs`, owned
/// elsewhere) classifies purely by matching `"not found"` in the error message,
/// pending typed-error support - see its doc comment. Phrasing it this way means a
/// malformed/malicious key and a well-formed-but-absent key produce the exact same
/// response, which also avoids handing back an oracle that distinguishes "that key
/// shape is invalid" from "that key doesn't exist". Propagated via plain `?` (not
/// `.context(...)`, which would replace this message as the top-level `Display`
/// anyhow uses for that string match) so both the message and the downcastable type
/// survive to the caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKeyError {
    pub key: String,
}

impl fmt::Display for InvalidKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Image not found in storage: {}", self.key)
    }
}

impl std::error::Error for InvalidKeyError {}

fn is_lower_hex(b: u8) -> bool {
    b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
}

/// Validates `key` against `<prefix><64 lowercase hex>.<jpg|png|webp>`, exactly the
/// shape `CacheService::generate_key` produces for the given `prefix` (the
/// operator-configured `STORAGE_SUB_PATH`, `""` for the default/no-sub-path case).
///
/// This is an **exact-prefix** match, not a "starts with something safe-looking"
/// heuristic: `key` must start with `prefix` byte-for-byte and nothing else may
/// precede the hash. That alone is what makes traversal and absolute paths
/// impossible to smuggle through - `/etc/passwd`, `../../../etc/passwd`, and any
/// percent-decoded equivalent can never satisfy "exactly 64 lowercase hex chars
/// immediately after `prefix`, then `.` then one of `jpg|png|webp`, then nothing else".
pub fn validate_cache_key(key: &str, prefix: &str) -> Result<(), InvalidKeyError> {
    let err = || InvalidKeyError {
        key: key.to_string(),
    };

    // Reject control characters (including NUL) up front, regardless of where they
    // land - guards against encoding oddities before any slicing happens below.
    if key.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return Err(err());
    }

    let rest = key.strip_prefix(prefix).ok_or_else(err)?;

    let (hash, ext) = rest.split_once('.').ok_or_else(err)?;

    if hash.len() != HASH_LEN || !hash.bytes().all(is_lower_hex) {
        return Err(err());
    }

    if !ALLOWED_EXTENSIONS.contains(&ext) {
        return Err(err());
    }

    Ok(())
}

/// Structurally splits an already-validated key into `(prefix, hash, ext)`.
///
/// Unlike [`validate_cache_key`], this does not need to know the configured
/// `STORAGE_SUB_PATH`: it derives the split purely from the fixed `HASH_LEN`, taking
/// the last `HASH_LEN` characters before the final `.` as the hash and everything
/// before that as the prefix. Used by `local_fs_handler` to derive the on-disk shard
/// directory from a key that has already passed [`validate_cache_key`] - kept
/// separate from validation itself so sharding never has to reason about which
/// `STORAGE_SUB_PATH` is currently configured.
pub fn split_key(key: &str) -> Option<(&str, &str, &str)> {
    let dot = key.rfind('.')?;
    let (before_ext, ext_with_dot) = key.split_at(dot);
    let ext = &ext_with_dot[1..];

    if before_ext.len() < HASH_LEN {
        return None;
    }
    let hash_start = before_ext.len() - HASH_LEN;
    let (prefix, hash) = before_ext.split_at(hash_start);
    Some((prefix, hash, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn accepts_valid_key_with_no_prefix() {
        let key = format!("{VALID_HASH}.jpg");
        assert!(validate_cache_key(&key, "").is_ok());
    }

    #[test]
    fn accepts_valid_key_with_configured_prefix() {
        let key = format!("sub/{VALID_HASH}.png");
        assert!(validate_cache_key(&key, "sub/").is_ok());
    }

    #[test]
    fn rejects_all_allowed_extensions_only() {
        for ext in ["jpg", "png", "webp"] {
            let key = format!("{VALID_HASH}.{ext}");
            assert!(
                validate_cache_key(&key, "").is_ok(),
                "{ext} should be accepted"
            );
        }
        for ext in ["gif", "bmp", "jpeg", "svg", "JPG"] {
            let key = format!("{VALID_HASH}.{ext}");
            assert!(
                validate_cache_key(&key, "").is_err(),
                "{ext} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_absolute_path() {
        assert!(validate_cache_key("/etc/passwd", "").is_err());
        let key = format!("/{VALID_HASH}.jpg");
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn rejects_traversal() {
        assert!(validate_cache_key("../../../etc/passwd", "").is_err());
        let key = format!("../{VALID_HASH}.jpg");
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn rejects_percent_encoded_traversal_forms() {
        // Not decoded at all - '%' isn't a hex digit, so this can never match.
        assert!(validate_cache_key("%2e%2e%2fetc%2fpasswd", "").is_err());
        // Already decoded by an upstream router into literal ".." - still rejected.
        assert!(validate_cache_key("..%2f..%2fetc%2fpasswd", "").is_err());
    }

    #[test]
    fn rejects_wrong_hash_length() {
        let short = format!("{}.jpg", &VALID_HASH[..63]);
        assert!(validate_cache_key(&short, "").is_err());
        let long = format!("{VALID_HASH}a.jpg");
        assert!(validate_cache_key(&long, "").is_err());
    }

    #[test]
    fn rejects_uppercase_hex() {
        let key = format!("{}.jpg", VALID_HASH.to_uppercase());
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn rejects_non_hex_characters() {
        let mut hash = VALID_HASH.to_string();
        hash.replace_range(0..1, "g");
        let key = format!("{hash}.jpg");
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn rejects_mismatched_prefix() {
        let key = format!("{VALID_HASH}.jpg");
        assert!(validate_cache_key(&key, "sub/").is_err());
        let key = format!("other/{VALID_HASH}.jpg");
        assert!(validate_cache_key(&key, "sub/").is_err());
    }

    #[test]
    fn rejects_double_extension_smuggling() {
        let key = format!("{VALID_HASH}.jpg.png");
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn rejects_nul_byte() {
        let key = format!("{VALID_HASH}.jpg\0.png");
        assert!(validate_cache_key(&key, "").is_err());
    }

    #[test]
    fn split_key_derives_prefix_hash_ext() {
        let key = format!("sub/{VALID_HASH}.webp");
        let (prefix, hash, ext) = split_key(&key).expect("should split");
        assert_eq!(prefix, "sub/");
        assert_eq!(hash, VALID_HASH);
        assert_eq!(ext, "webp");
    }

    #[test]
    fn split_key_none_for_too_short_input() {
        assert!(split_key("short.jpg").is_none());
    }
}
