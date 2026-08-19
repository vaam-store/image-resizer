//! Integration coverage for GH-38 (non-atomic local_fs writes + directories
//! mis-treated as cache hits), exercised through the real public API
//! (`StorageService` + `StorageConfig::with_local_fs_config`) backed by a
//! real local_fs backend on a real temp directory.

#![cfg(feature = "local_fs")]

use emgr::services::storage::handler::{StorageConfig, StorageService};
use std::sync::atomic::{AtomicU64, Ordering};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn build_storage_service() -> (StorageService, std::path::PathBuf) {
    let id = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "emgr-storage-local-fs-{}-{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&dir).expect("create test storage dir");

    let storage_config =
        StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&dir);
    let storage_service = StorageService::new(storage_config).expect("build storage service");

    (storage_service, dir)
}

/// Mirrors `LocalFSStorage`'s sharding scheme (`SHARD_PREFIX_LEN = 2`,
/// `src/services/storage/local_fs_handler.rs`): `<first 2 hex chars of
/// hash>/<hash>.<ext>` under the storage root, with no `STORAGE_SUB_PATH`
/// prefix (this test's `StorageConfig` never sets one). If that sharding
/// constant ever changes, this helper needs updating alongside it.
fn shard_path(dir: &std::path::Path, key: &str) -> std::path::PathBuf {
    let (hash, ext) = key.split_once('.').expect("key must have an extension");
    dir.join(&hash[..2]).join(format!("{hash}.{ext}"))
}

fn hash_key(byte: u8) -> String {
    // A syntactically valid 64-lowercase-hex-char key, distinct per `byte`
    // so different callers land in different (or the same, if the first
    // byte repeats) shard directories.
    let hex = format!("{byte:02x}");
    format!("{}.jpg", hex.repeat(32))
}

#[tokio::test]
async fn directory_at_key_path_is_reported_as_a_miss_not_an_error() {
    let (storage, dir) = build_storage_service();
    let key = hash_key(0xab);

    // Plant a directory at exactly the path this key would resolve to,
    // simulating whatever could put one there (a partial/aborted write in
    // an older version of this code, manual tampering, etc).
    let path = shard_path(&dir, &key);
    std::fs::create_dir_all(&path).expect("seed a directory at the cache path");

    let result = storage.check_cache(&key).await;
    assert_eq!(
        result.unwrap(),
        false,
        "a directory at the key's path must be a clean miss, not treated as a hit"
    );

    // get_image must likewise fail cleanly (not e.g. try to read a
    // directory's bytes) once it's asked to fetch a path that's a directory.
    assert!(storage.get_image(&key).await.is_err());
}

#[tokio::test]
async fn upload_shards_by_hash_prefix_transparently() {
    let (storage, dir) = build_storage_service();
    let key = hash_key(0xcd);
    let payload = b"shard me".to_vec();

    storage
        .upload_image(&key, "image/jpeg", payload.clone())
        .await
        .expect("upload should succeed");

    // Public behaviour is unchanged: the same key round-trips correctly...
    assert_eq!(storage.get_image(&key).await.unwrap(), payload);

    // ...but on disk, it landed under a shard directory rather than flat
    // under the storage root, so a large cache doesn't dump millions of
    // entries into one directory.
    let expected_path = shard_path(&dir, &key);
    assert!(
        expected_path.is_file(),
        "expected sharded file at {}",
        expected_path.display()
    );
    assert!(
        !dir.join(&key).exists(),
        "key must not also land flat directly under the storage root"
    );
}

/// Concurrent writers to the SAME key, with a reader loop racing them, must
/// never observe a partial/mixed file - only ever a complete previous
/// write, a complete new write, or "not found" before the first write
/// lands. This is the core GH-38 property: writes go through a temp file in
/// the same directory + fsync + atomic rename, so a reader can never open
/// the file mid-write.
///
/// Runs on a real multi-threaded runtime (not just cooperative `yield_now`
/// interleaving on one thread) so readers and writers can genuinely overlap
/// in wall-clock time, and readers poll for the entire duration the writers
/// are running (via `done`) rather than a fixed iteration count, so the test
/// doesn't depend on guessing how fast either side runs on a given machine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_to_same_key_never_produce_a_partial_read() {
    let (storage, _dir) = build_storage_service();
    let key = hash_key(0xef);

    const WRITERS: usize = 12;
    const PAYLOAD_LEN: usize = 256 * 1024; // large enough that a non-atomic
    // write interleaved with a concurrent read would, in practice, be
    // observed as truncated or mixed rather than accidentally still valid.

    // Each writer's payload is `PAYLOAD_LEN` copies of a single distinguishing
    // byte, so "was this read partial/mixed" reduces to "are all the bytes
    // in this read the same value, and is one of the known writer bytes".
    let writer_bytes: Vec<u8> = (0..WRITERS as u8).collect();
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let mut reader_handles = Vec::new();
    for _ in 0..4 {
        let storage = storage.clone();
        let key = key.clone();
        let done = done.clone();
        reader_handles.push(tokio::spawn(async move {
            let mut observed = Vec::new();
            // Keep polling for as long as writers are still running, plus a
            // little extra to also catch reads racing the very last write.
            while !done.load(Ordering::Relaxed) {
                if let Ok(bytes) = storage.get_image(&key).await {
                    observed.push(bytes);
                }
            }
            for _ in 0..50 {
                if let Ok(bytes) = storage.get_image(&key).await {
                    observed.push(bytes);
                }
            }
            observed
        }));
    }

    let mut writer_handles = Vec::new();
    for &b in &writer_bytes {
        let storage = storage.clone();
        let key = key.clone();
        writer_handles.push(tokio::spawn(async move {
            let payload = vec![b; PAYLOAD_LEN];
            storage
                .upload_image(&key, "image/jpeg", payload)
                .await
                .expect("concurrent upload must succeed");
        }));
    }

    for h in writer_handles {
        h.await.expect("writer task panicked");
    }
    done.store(true, Ordering::Relaxed);

    let mut all_observed = Vec::new();
    for h in reader_handles {
        all_observed.extend(h.await.expect("reader task panicked"));
    }

    assert!(
        !all_observed.is_empty(),
        "test should have observed at least one successful concurrent read"
    );

    for bytes in &all_observed {
        assert_eq!(
            bytes.len(),
            PAYLOAD_LEN,
            "every read must be a complete write, never truncated"
        );
        let first = bytes[0];
        assert!(
            writer_bytes.contains(&first),
            "read bytes must match one of the writers' payloads"
        );
        assert!(
            bytes.iter().all(|&b| b == first),
            "every byte in a read must come from a single writer's payload - a mix would mean a torn/partial read"
        );
    }

    // The final state must also be one complete writer's payload, not a mix.
    let final_bytes = storage
        .get_image(&key)
        .await
        .expect("key must exist after all writers finish");
    assert_eq!(final_bytes.len(), PAYLOAD_LEN);
    let first = final_bytes[0];
    assert!(final_bytes.iter().all(|&b| b == first));
}
