//! Integration coverage for GH-23 (arbitrary file read via an unvalidated
//! `key`) exercised through the real public API (`StorageService`), backed
//! by a real local_fs backend - not just the pure `validate_cache_key`
//! unit tests in `src/services/storage/key_validation.rs`.
//!
//! Confirms traversal, absolute paths, and percent-decoded forms of either
//! are rejected *before* ever touching the backend (so they can never read
//! outside the per-test storage directory), and that a well-formed key is
//! accepted.

#![cfg(feature = "local_fs")]

use emgr::services::storage::handler::{StorageConfig, StorageService};
use std::sync::atomic::{AtomicU64, Ordering};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A syntactically valid cache key: 64 lowercase hex chars + a real
/// extension, exactly the shape `CacheService::generate_key` produces.
const VALID_KEY: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.jpg";

/// Builds a `StorageService` backed by an isolated, per-call local_fs
/// directory under the OS temp dir, plus the directory itself so tests can
/// inspect the on-disk layout.
fn build_storage_service() -> (StorageService, std::path::PathBuf) {
    let id = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "emgr-storage-key-validation-{}-{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&dir).expect("create test storage dir");

    let storage_config =
        StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&dir);
    let storage_service = StorageService::new(storage_config).expect("build storage service");

    (storage_service, dir)
}

#[tokio::test]
async fn absolute_path_key_is_rejected() {
    let (storage, dir) = build_storage_service();

    // Plant a real file outside the storage directory - if the absolute
    // path ever reached the local_fs backend unvalidated,
    // `base_path.join("/etc/passwd")` would discard `base_path` entirely
    // (PathBuf::join's documented behaviour for an absolute `path`) and
    // read this file straight off disk.
    let outside_secret = dir.parent().unwrap().join("emgr-outside-secret.txt");
    std::fs::write(&outside_secret, b"do not serve me").expect("seed outside file");

    let key = outside_secret.to_str().unwrap().to_string();
    assert!(key.starts_with('/'), "test key must be absolute");

    assert!(
        storage.check_cache(&key).await.is_err(),
        "absolute-path key must be rejected, not silently checked"
    );
    assert!(
        storage.get_image(&key).await.is_err(),
        "absolute-path key must never reach the backend's read path"
    );

    let _ = std::fs::remove_file(&outside_secret);
}

#[tokio::test]
async fn dot_dot_traversal_key_is_rejected() {
    let (storage, _dir) = build_storage_service();

    for key in [
        "../../../etc/passwd",
        "../secret.jpg",
        "a/../../../etc/passwd.jpg",
    ] {
        assert!(
            storage.check_cache(key).await.is_err(),
            "traversal key {key:?} must be rejected"
        );
        assert!(
            storage.get_image(key).await.is_err(),
            "traversal key {key:?} must never reach the backend's read path"
        );
    }
}

#[tokio::test]
async fn percent_decoded_traversal_forms_are_rejected() {
    let (storage, _dir) = build_storage_service();

    // Neither form can ever satisfy "prefix + 64 lowercase hex + . + valid
    // extension", whether or not something upstream already percent-decoded
    // it before this key arrived here.
    for key in [
        "%2e%2e%2fetc%2fpasswd",       // still percent-encoded
        "..%2f..%2f..%2fetc%2fpasswd", // partially decoded
        "../../../etc/passwd",         // fully decoded
    ] {
        assert!(
            storage.check_cache(key).await.is_err(),
            "percent-encoded/decoded traversal key {key:?} must be rejected"
        );
    }
}

#[tokio::test]
async fn malformed_shape_keys_are_rejected() {
    let (storage, _dir) = build_storage_service();

    for key in [
        "",
        "not-a-hash.jpg",
        "present.png", // realistic-looking but not what generate_key emits
        &VALID_KEY[..VALID_KEY.len() - 1], // hash one char short
        &format!("{VALID_KEY}x"),          // trailing garbage after the extension
        &VALID_KEY.to_uppercase(),         // uppercase hex
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.gif", // wrong extension
    ] {
        assert!(
            storage.check_cache(key).await.is_err(),
            "malformed key {key:?} must be rejected"
        );
    }
}

#[tokio::test]
async fn well_formed_key_is_accepted_end_to_end() {
    let (storage, _dir) = build_storage_service();

    // A well-formed key that doesn't exist yet is a clean miss, not an
    // error - validation and "does it exist" are different questions.
    assert_eq!(storage.check_cache(VALID_KEY).await.unwrap(), false);

    let payload = b"not really a jpeg, just test bytes".to_vec();
    storage
        .upload_image(VALID_KEY, "image/jpeg", payload.clone())
        .await
        .expect("upload of a well-formed key must succeed");

    assert_eq!(storage.check_cache(VALID_KEY).await.unwrap(), true);
    assert_eq!(storage.get_image(VALID_KEY).await.unwrap(), payload);
}
