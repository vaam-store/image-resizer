//! Integration coverage for the S3/MinIO storage backend
//! (`src/services/storage/s3_handler.rs`), which previously had NO tests at
//! all (no `mod tests` in the file, no `tests/` file referencing it).
//!
//! `is_expired` (the TTL/expiry check, #40) is a private associated fn, so
//! it's unit-tested inside `s3_handler.rs` itself
//! (`#[cfg(test)] mod tests`) via same-module access - see that file. This
//! file instead exercises everything that needs a real request/response
//! round trip: `upload_image_with_ttl`, `check_cache`, `get_image`,
//! `delete`, and the S3 error-mapping contract
//! (`HeadObjectError::NotFound` -> `Ok(false)`,
//! `GetObjectError::NoSuchKey` -> an `Err` whose message contains
//! "not found").
//!
//! ## Test double: in-process fake-S3 HTTP server, not a trait-level double
//!
//! The task owner asked for a trait-level `StorageBackend` double if a real
//! MinIO couldn't be used, to keep `cargo test` hermetic. A strict
//! trait-level double (a fake struct implementing `StorageBackend` that
//! never calls into `s3_handler.rs`) was rejected here: it would be
//! hermetic, but it would test nothing about `s3_handler.rs` itself - the
//! whole point of this file. A real MinIO container was also rejected: it
//! needs Docker at `cargo test` time and a new dependency (e.g.
//! testcontainers), both out of scope.
//!
//! Instead, this file spins up a minimal in-process HTTP server (built with
//! `axum`, already a real dependency - no `Cargo.toml` change needed) bound
//! to `127.0.0.1:0` (OS-assigned port), implementing just enough of the S3
//! REST surface (PUT/GET/HEAD/DELETE on `/{bucket}/{key}`) to drive a REAL
//! `aws_sdk_s3::Client` - via `MinIOStorage::new_minio` pointed at
//! `http://127.0.0.1:<port>` - through actual `upload_image_with_ttl` /
//! `check_cache` / `get_image` / `delete` calls end-to-end. This is
//! hermetic (nothing external, no Docker, no real network egress, fully
//! in-process and deterministic) while exercising `s3_handler.rs`'s real
//! request/response and error-mapping code, unlike a trait-level double.
//!
//! The fake server does not validate SigV4 `Authorization`/`x-amz-*`
//! headers (auth isn't what's under test here). PUT captures whatever
//! `Expires` header the SDK attached (set by `upload_image_with_ttl` from
//! `ttl`) and echoes it back verbatim on GET/HEAD, so the TTL round trip
//! through the real HTTP-date formatting/parsing code in `s3_handler.rs`
//! is exercised for real rather than faked on the server side.

#![cfg(feature = "s3")]

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use emgr::services::storage::handler::{StorageConfig, StorageService};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A syntactically valid cache key: 64 lowercase hex chars + a real
/// extension, matching the shape `StorageService`'s key validation (#23)
/// requires - this file goes through `StorageService`, not `MinIOStorage`
/// directly, for at least the round-trip test, so that wiring is also
/// implicitly covered for the S3 path.
fn hash_key(byte: u8) -> String {
    let hex = format!("{byte:02x}");
    format!("{}.jpg", hex.repeat(32))
}

#[derive(Clone)]
struct StoredObject {
    body: Vec<u8>,
    expires: Option<String>,
}

#[derive(Clone, Default)]
struct FakeS3State {
    objects: Arc<Mutex<HashMap<String, StoredObject>>>,
}

fn object_key(bucket: &str, key: &str) -> String {
    format!("{bucket}/{key}")
}

async fn put_object(
    State(state): State<FakeS3State>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let expires = headers
        .get(header::EXPIRES)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    state.objects.lock().unwrap().insert(
        object_key(&bucket, &key),
        StoredObject {
            body: body.to_vec(),
            expires,
        },
    );
    StatusCode::OK.into_response()
}

async fn head_object(
    State(state): State<FakeS3State>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    let objects = state.objects.lock().unwrap();
    match objects.get(&object_key(&bucket, &key)) {
        Some(obj) => {
            let mut resp = StatusCode::OK.into_response();
            if let Some(expires) = &obj.expires {
                if let Ok(v) = HeaderValue::from_str(expires) {
                    resp.headers_mut().insert(header::EXPIRES, v);
                }
            }
            resp
        }
        // HeadObjectError::NotFound is triggered by a plain 404 status - S3
        // does not require a body for HEAD errors.
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn get_object(
    State(state): State<FakeS3State>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    let objects = state.objects.lock().unwrap();
    match objects.get(&object_key(&bucket, &key)) {
        Some(obj) => {
            let mut resp = (StatusCode::OK, obj.body.clone()).into_response();
            if let Some(expires) = &obj.expires {
                if let Ok(v) = HeaderValue::from_str(expires) {
                    resp.headers_mut().insert(header::EXPIRES, v);
                }
            }
            resp
        }
        // A minimal S3/MinIO-style error body - the AWS SDK parses the
        // bare (unwrapped) <Error><Code>...</Code></Error> XML shape to
        // construct the typed `GetObjectError::NoSuchKey` variant. Getting
        // this shape wrong makes the SDK fall through to a generic/
        // unmodeled error instead.
        None => {
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message><Key>{key}</Key><RequestId>FAKE-REQUEST-ID</RequestId></Error>"
            );
            (
                StatusCode::NOT_FOUND,
                [(header::CONTENT_TYPE, "application/xml")],
                xml,
            )
                .into_response()
        }
    }
}

async fn delete_object(
    State(state): State<FakeS3State>,
    Path((bucket, key)): Path<(String, String)>,
) -> Response {
    state
        .objects
        .lock()
        .unwrap()
        .remove(&object_key(&bucket, &key));
    StatusCode::NO_CONTENT.into_response()
}

/// Starts the fake S3 double on an OS-assigned loopback port and returns its
/// base URL plus a handle that tears the server down when dropped/aborted.
async fn spawn_fake_s3() -> (String, tokio::task::JoinHandle<()>) {
    let state = FakeS3State::default();
    // force_path_style(true) (set in `MinIOStorage::new_minio`) means
    // requests land as `/{bucket}/{key}`, not virtual-hosted-style - the
    // route below matches that shape directly.
    let app: Router = Router::new()
        .route(
            "/{bucket}/{*key}",
            get(get_object)
                .put(put_object)
                .head(head_object)
                .delete(delete_object),
        )
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake S3 listener");
    let addr = listener.local_addr().expect("local_addr");

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (format!("http://{addr}"), handle)
}

/// Builds a `StorageService` wired to the fake S3 double via
/// `StorageConfig::with_s3_config` - the real public builder path, going
/// through key validation (#23) before ever touching `MinIOStorage`.
fn build_storage_service(endpoint: &str) -> StorageService {
    let storage_config = StorageConfig::new("http://cdn.test".to_string())
        // Explicit, so this test's behaviour doesn't depend on which other
        // storage features happen to be compiled in alongside `s3`.
        .with_storage_type("S3")
        .with_s3_config(
            endpoint.to_string(),
            "test-access-key".to_string(),
            "test-secret-key".to_string(),
            "test-bucket".to_string(),
            "us-east-1".to_string(),
        );
    StorageService::new(storage_config).expect("build S3-backed storage service")
}

#[tokio::test]
async fn upload_then_check_cache_and_get_image_round_trip() {
    let (endpoint, server) = spawn_fake_s3().await;
    let storage = build_storage_service(&endpoint);
    let key = hash_key(0x11);
    let payload = b"fake-jpeg-bytes-for-s3-round-trip".to_vec();

    storage
        .upload_image(&key, "image/jpeg", payload.clone())
        .await
        .expect("upload_image should succeed against the fake S3 double");

    assert!(
        storage.check_cache(&key).await.expect("check_cache"),
        "just-uploaded key should be a cache hit"
    );

    let fetched = storage
        .get_image(&key)
        .await
        .expect("get_image should return the uploaded bytes");
    assert_eq!(fetched, payload);

    server.abort();
}

#[tokio::test]
async fn check_cache_and_get_image_on_missing_key_are_a_clean_miss() {
    let (endpoint, server) = spawn_fake_s3().await;
    let storage = build_storage_service(&endpoint);
    let key = hash_key(0x22);

    assert_eq!(
        storage.check_cache(&key).await.expect("check_cache"),
        false,
        "a key that was never uploaded must be reported as a clean miss, not an error"
    );

    let err = storage
        .get_image(&key)
        .await
        .expect_err("get_image on a missing key must fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("not found"),
        "error message must contain 'not found' (matched by \
         AppError::classify_download_error one layer up): got {msg:?}"
    );

    server.abort();
}

/// Covers the lazy-eviction / TTL contract (#40) for the S3 backend
/// specifically - `LocalFSStorage`'s and `InMemoryStorage`'s versions of
/// this are already covered elsewhere (`tests/storage_local_fs_atomicity.rs`
/// indirectly, and `handler.rs`'s own `#[cfg(test)]` module respectively),
/// but `s3_handler.rs`'s `is_expired`-driven lazy eviction on `check_cache`/
/// `get_image` had no coverage at all before this file.
#[tokio::test]
async fn ttl_expiry_is_honored_through_the_fake_s3_double() {
    let (endpoint, server) = spawn_fake_s3().await;
    let storage = build_storage_service(&endpoint);
    let key = hash_key(0x33);

    storage
        .upload_image_with_ttl(
            &key,
            "image/jpeg",
            b"will-expire-soon".to_vec(),
            Some(std::time::Duration::from_millis(1)),
        )
        .await
        .expect("upload_image_with_ttl should succeed");

    // Let real wall-clock time pass the TTL, mirroring
    // `storage_local_fs_atomicity.rs`'s / `handler.rs`'s own TTL tests -
    // `is_expired` compares against `SystemTime::now()`, so there's no way
    // to fast-forward it deterministically without reaching into private
    // state.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    assert_eq!(
        storage.check_cache(&key).await.expect("check_cache"),
        false,
        "an expired entry must be reported as a cache miss"
    );

    let err = storage
        .get_image(&key)
        .await
        .expect_err("get_image on an expired key must fail");
    assert!(
        err.to_string().to_lowercase().contains("not found"),
        "expired entry must fail exactly like a missing one: {err}"
    );

    server.abort();
}

#[tokio::test]
async fn delete_is_idempotent_whether_or_not_the_key_existed() {
    let (endpoint, server) = spawn_fake_s3().await;
    let storage = build_storage_service(&endpoint);
    let key = hash_key(0x44);

    // Deleting a key that was never uploaded must still succeed (S3's
    // DeleteObject is itself idempotent - no NotFound special-casing).
    storage
        .delete(&key)
        .await
        .expect("deleting an absent key should still succeed");

    storage
        .upload_image(&key, "image/jpeg", b"to-be-deleted".to_vec())
        .await
        .expect("upload_image");
    assert!(storage.check_cache(&key).await.expect("check_cache"));

    storage.delete(&key).await.expect("delete should succeed");
    assert_eq!(storage.check_cache(&key).await.expect("check_cache"), false);
    assert!(storage.get_image(&key).await.is_err());

    // Deleting again (now absent again) must still succeed.
    storage
        .delete(&key)
        .await
        .expect("deleting an already-absent key should still succeed");

    server.abort();
}
