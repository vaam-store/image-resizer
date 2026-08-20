use crate::config::performance::PerformanceConfig;
use crate::models::params::ResizeQuery;
use crate::services::cache::handler::CacheService;
use crate::services::image::handler::ImageService;
use crate::services::storage::handler::StorageService;
use anyhow::Result;
use dashmap::DashMap;
use dashmap::mapref::entry::Entry as MapEntry;
use derive_builder::Builder;
// #53: `gen_server` (OpenAPI codegen) was deleted; `DownloadPathParams` is
// now hand-written in `src/models/params.rs` (owned by the URL-grammar/
// signed-URL rewrite), same single `key: String` field. Mechanical import
// change only - no logic here changed.
use crate::models::params::DownloadPathParams;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;
use tracing::{debug, error, info, instrument, warn};

/// Result type shared across every follower of a single-flight group
/// (#37): `broadcast` requires its payload to be `Clone`, and
/// `anyhow::Error` is not, so the leader's `Result<String>` is flattened to
/// `Result<String, String>` (the error's `Display` text) before being
/// broadcast. The public `resize` API still returns a real `anyhow::Result`
/// - only the internal hand-off between leader and followers uses this
/// type.
type InFlightResult = Result<String, String>;

/// Tracks resize work already in progress, keyed by cache key (#37).
///
/// # Why `dashmap` + `tokio::sync::broadcast`, not a `Mutex`-guarded
/// `HashMap` of futures or a `OnceCell` per key
///
/// The map itself only ever needs to be touched for the instant it takes to
/// check "is someone already working on this key" and, if not, register
/// that this caller now is - never held across an `.await`. `dashmap` gives
/// sharded, lock-free-ish access for exactly that pattern without pulling
/// the whole map behind a single `tokio::sync::Mutex` that every concurrent
/// request - even ones for entirely different keys - would contend on.
///
/// For the "rest await its result" half, `broadcast::channel(1)` was chosen
/// over `futures::future::Shared` or a `OnceCell` because it has the
/// failure-handling behaviour the issue calls out for free: if the leader's
/// task is cancelled or panics before it ever calls `finish` on its
/// [`InFlightGuard`], the guard's `Drop` impl still runs (Rust guarantees
/// this on both panic-unwind and future-cancellation-via-drop), removes the
/// map entry, and sends a synthetic failure on the channel. Every follower's
/// `rx.recv()` then resolves to that failure instead of hanging forever -
/// "a failed leader must not poison followers indefinitely" is enforced
/// structurally, not by a timeout. A bare `Shared` future has no equivalent
/// "the original future was dropped without completing" signal a follower
/// could observe distinctly from "it's still running"; a per-key `OnceCell`
/// has the same gap unless paired with its own drop-guard, at which point
/// it is no simpler than this.
type InFlightMap = Arc<DashMap<String, broadcast::Sender<InFlightResult>>>;

/// RAII guard that owns a single-flight leader's map entry for the duration
/// of its work (#37).
///
/// Registered the instant this caller wins the race to become leader, and
/// dropped exactly once - either after [`Self::finish`] records the real
/// outcome, or, if the leader's future is dropped early (panic, the caller
/// cancelling the request, ...) without ever calling `finish`, with the
/// fallback "leader did not complete" failure below. Either way `Drop`
/// removes the map entry and broadcasts a result, so followers can never be
/// left waiting on an entry whose leader is gone.
struct InFlightGuard {
    map: InFlightMap,
    key: String,
    result: Option<InFlightResult>,
}

impl InFlightGuard {
    fn new(map: InFlightMap, key: String) -> Self {
        Self {
            map,
            key,
            result: None,
        }
    }

    /// Records the leader's actual outcome, to be broadcast when this guard
    /// drops (normally, right after this call, at the end of `resize`).
    fn finish(&mut self, result: InFlightResult) {
        self.result = Some(result);
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        // Only the leader that registered this key removes it - a stale
        // remove here can't happen because a given key only ever has one
        // `InFlightGuard` alive at a time (the map entry is what prevents a
        // second caller from also becoming leader for the same key).
        if let Some((_, tx)) = self.map.remove(&self.key) {
            let result = self.result.take().unwrap_or_else(|| {
                warn!(
                    "in-flight leader for cache key {} was dropped before completing \
                     (panic or cancellation) - failing {} follower(s)",
                    self.key,
                    tx.receiver_count()
                );
                Err(format!(
                    "in-flight request for key {} was cancelled or panicked before completing",
                    self.key
                ))
            });
            // No receivers is a normal outcome (every follower may have
            // already given up), not an error worth surfacing.
            let _ = tx.send(result);
        }
    }
}

/// Main service for image resizing with performance optimizations
#[derive(Clone, Builder)]
pub struct ResizeService {
    storage_service: StorageService,
    cache_service: CacheService,
    image_service: ImageService,
    /// Single-flight registry (#37): coalesces concurrent `resize` calls
    /// that land on the same cache key so only one actually downloads,
    /// processes and uploads.
    #[builder(default)]
    in_flight: InFlightMap,
    /// TTL applied to entries this service uploads (#40). `None` (the
    /// default from both constructors below) preserves the pre-#40
    /// behaviour of never expiring - there is currently no operator-facing
    /// config knob feeding a real duration in here (that lives in
    /// `config::performance`, owned separately); this field exists so the
    /// TTL mechanism in `StorageBackend` has a real, tested caller today and
    /// is a single, obvious place to wire a configured duration into once
    /// one exists.
    #[builder(default)]
    cache_ttl: Option<Duration>,
}

impl ResizeService {
    /// Create a new ResizeService with default performance configuration
    pub fn new(storage_service: StorageService, cache_service: CacheService) -> Result<Self> {
        let image_service = ImageService::new()?;
        Ok(Self {
            storage_service,
            cache_service,
            image_service,
            in_flight: InFlightMap::default(),
            cache_ttl: None,
        })
    }

    /// Create a new ResizeService with custom performance configuration
    pub fn with_config(
        storage_service: StorageService,
        cache_service: CacheService,
        config: PerformanceConfig,
    ) -> Result<Self> {
        let image_service = ImageService::with_config(config)?;
        Ok(Self {
            storage_service,
            cache_service,
            image_service,
            in_flight: InFlightMap::default(),
            cache_ttl: None,
        })
    }

    /// Main resize method with optimized processing.
    ///
    /// Single-flight coalescing (#37): once the cache is confirmed a miss,
    /// concurrent callers for the *same cache key* no longer each run their
    /// own download/process/upload - the first caller becomes the "leader"
    /// and does the work, every other concurrent caller ("follower")
    /// awaits and shares the leader's result. See [`InFlightMap`] and
    /// [`InFlightGuard`] for how leader failure/cancellation is handled so
    /// followers are never left hanging.
    #[instrument(skip(self), fields(url = %params.url))]
    pub async fn resize(&self, params: &ResizeQuery) -> Result<String> {
        // Generate cache key
        let cache_key = self.cache_service.generate_key(params);
        debug!("Generated cache key: {}", cache_key);

        // Check cache
        match self.storage_service.check_cache(&cache_key).await {
            Ok(true) => {
                info!("Cache hit for key: {}", cache_key);
                return Ok(self.storage_service.get_cdn_url(&cache_key));
            }
            Ok(false) => {
                info!(
                    "Cache miss for key: {}. Proceeding with processing.",
                    cache_key
                );
            }
            Err(e) => {
                error!("Error checking cache for key {}: {:?}", cache_key, e);
                // Continue as if it's a cache miss
            }
        }

        // Single-flight: become the leader for this key, or subscribe to
        // whoever already is one (#37). `DashMap::entry` takes a shard lock
        // synchronously - no `.await` happens while it's held, so this
        // never blocks another key's request and never holds the lock
        // across suspension.
        let mut rx = match self.in_flight.entry(cache_key.clone()) {
            MapEntry::Occupied(occupied) => {
                debug!("Joining in-flight request for key: {}", cache_key);
                Some(occupied.get().subscribe())
            }
            MapEntry::Vacant(vacant) => {
                let (tx, _rx) = broadcast::channel(1);
                vacant.insert(tx);
                None
            }
        };

        if let Some(rx) = rx.as_mut() {
            return match rx.recv().await {
                Ok(Ok(url)) => Ok(url),
                Ok(Err(e)) => Err(anyhow::anyhow!(e)),
                Err(_) => Err(anyhow::anyhow!(
                    "in-flight request for key {} was cancelled or panicked before completing",
                    cache_key
                )),
            };
        }

        // We won the race and are now the leader. The guard below is what
        // guarantees followers get unblocked no matter how this function
        // exits - normal return, `?`-propagated error, or the whole future
        // being dropped out from under us.
        let mut guard = InFlightGuard::new(Arc::clone(&self.in_flight), cache_key.clone());
        let result = self.do_resize_work(&cache_key, params).await;
        guard.finish(
            result
                .as_ref()
                .map(|url| url.clone())
                .map_err(|e| e.to_string()),
        );
        // `guard` drops here (end of scope), broadcasting `result` to any
        // followers that subscribed while we were working and removing the
        // map entry so the *next* distinct request for this key starts a
        // fresh attempt rather than joining a completed one.
        result
    }

    /// The actual download -> process -> upload pipeline for a confirmed
    /// cache miss. Factored out of [`Self::resize`] so the single-flight
    /// leader path has a single call site to await, keeping the
    /// [`InFlightGuard`] bookkeeping in `resize` easy to follow.
    async fn do_resize_work(&self, cache_key: &str, params: &ResizeQuery) -> Result<String> {
        // Download image
        let download_timer = Instant::now();
        let image_bytes = match self.image_service.download_image(&params.url).await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to download image: {}", e);
                return Err(e);
            }
        };
        debug!("Image download took {:?}", download_timer.elapsed());
        info!("Image downloaded, {} bytes", image_bytes.len());

        // Process image
        let process_timer = Instant::now();
        let (processed_image, content_type) =
            match self.image_service.process_image(&image_bytes, params).await {
                Ok(result) => result,
                Err(e) => {
                    error!("Failed to process image: {}", e);
                    return Err(e);
                }
            };
        debug!("Image processing took {:?}", process_timer.elapsed());
        info!("Image processed, {} bytes", processed_image.len());

        // Upload to storage
        let upload_timer = Instant::now();
        if let Err(e) = self
            .storage_service
            .upload_image_with_ttl(cache_key, &content_type, processed_image, self.cache_ttl)
            .await
        {
            error!("Failed to upload image: {}", e);
            return Err(e);
        }
        debug!("Image upload took {:?}", upload_timer.elapsed());
        info!("Upload successful");

        // Return CDN URL
        let cdn_url = self.storage_service.get_cdn_url(cache_key);
        info!("Returning CDN URL: {}", cdn_url);

        Ok(cdn_url)
    }

    /// Batch processing for multiple images with controlled concurrency
    pub async fn resize_batch(
        &self,
        requests: Vec<ResizeQuery>,
        max_concurrent: usize,
    ) -> Vec<Result<String>> {
        use futures::stream::{self, StreamExt};

        stream::iter(requests)
            .map(|params| async move { self.resize(&params).await })
            .buffer_unordered(max_concurrent)
            .collect()
            .await
    }

    /// Collapses what used to be a `check_cache` (HEAD) followed by a
    /// `get_image` (GET) into a single call (#40): on S3 that was a HEAD
    /// plus a GET for every download, with a TOCTOU window between them
    /// where the object could be deleted/expire in between. Just attempt
    /// the GET and let a "not found" error from the backend do the same
    /// job `AppError::classify_download_error` (`src/modules/utils/err.rs`,
    /// owned separately) already relies on: it maps a `key_validation`
    /// `InvalidKeyError` (still raised first, before any backend is
    /// touched - see `StorageService::get_image`) and any other message
    /// containing "not found" to a 404, and every backend's `get_image`
    /// (including the lazy-expiry paths added for #40) produces exactly
    /// that phrasing for a missing or expired key.
    #[instrument(skip(self), fields(url = %params.key))]
    pub async fn download(&self, params: &DownloadPathParams) -> Result<Vec<u8>> {
        let download_timer = Instant::now();

        match self.storage_service.get_image(&params.key).await {
            Ok(data) => {
                info!("download successful");
                debug!("Image download took {:?}", download_timer.elapsed());
                Ok(data)
            }
            Err(e) => {
                error!("download failed: {}", e);
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::performance::PerformanceConfig;
    use crate::services::cache::handler::CacheServiceBuilder;
    use crate::services::storage::core::StorageBackend;
    use crate::services::storage::handler::StorageServiceBuilder;
    // #53: mechanical import change, same reasoning as the top-of-file one.
    use crate::models::params::ImageFormat;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Test-only `StorageBackend` that counts uploads instead of persisting
    /// anywhere real.
    ///
    /// #37's acceptance test needs to assert "exactly one encode" as well
    /// as "exactly one origin fetch". `do_resize_work` always calls
    /// `upload_image_with_ttl` immediately after `process_image` succeeds,
    /// and there is no other path in this service that uploads without
    /// having just encoded - so counting uploads is an exact proxy for
    /// counting encodes, without needing to instrument `ImageService`
    /// itself (owned elsewhere, and not something this test's ownership
    /// covers changing).
    #[derive(Default)]
    struct CountingStorage {
        uploads: AtomicUsize,
        data: Mutex<HashMap<String, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl StorageBackend for CountingStorage {
        async fn upload_image_with_ttl(
            &self,
            key: &str,
            _content_type: &str,
            data: Vec<u8>,
            _ttl: Option<Duration>,
        ) -> anyhow::Result<()> {
            self.uploads.fetch_add(1, Ordering::SeqCst);
            self.data.lock().unwrap().insert(key.to_string(), data);
            Ok(())
        }

        async fn check_cache(&self, key: &str) -> anyhow::Result<bool> {
            Ok(self.data.lock().unwrap().contains_key(key))
        }

        async fn get_image(&self, key: &str) -> anyhow::Result<Vec<u8>> {
            self.data
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("not found in CountingStorage"))
        }

        async fn delete(&self, key: &str) -> anyhow::Result<()> {
            self.data.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn tiny_png_bytes() -> Vec<u8> {
        let img = image::ImageBuffer::from_fn(4, 4, |_, _| image::Rgb([255u8, 0, 0]));
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let mut buf = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode test png");
        buf.into_inner()
    }

    /// Spawns a local HTTP server that serves `body` as `image/png` for
    /// every connection it accepts, incrementing `request_count` once per
    /// accepted connection. Used to observe how many times the resize
    /// pipeline actually reaches "the origin" (#37's acceptance test) -
    /// unlike a one-shot test server that only ever answers a single
    /// request, this one keeps accepting so a single-flight regression
    /// (every caller actually downloading) would show up as a count > 1
    /// instead of the test hanging.
    async fn spawn_counting_test_image_server(
        body: Vec<u8>,
        request_count: Arc<AtomicUsize>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                request_count.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&body).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://{}/image.png", addr)
    }

    /// #37's acceptance criterion: 100 concurrent `resize()` calls for the
    /// same uncached key must produce exactly one origin fetch and exactly
    /// one encode (proxied by exactly one storage upload - see
    /// `CountingStorage`'s doc comment), not 100 of each.
    #[tokio::test]
    async fn concurrent_requests_for_same_key_coalesce_to_single_flight() {
        let request_count = Arc::new(AtomicUsize::new(0));
        let url =
            spawn_counting_test_image_server(tiny_png_bytes(), Arc::clone(&request_count)).await;

        let storage_backend = Arc::new(CountingStorage::default());
        let storage_service = StorageServiceBuilder::default()
            .storage(storage_backend.clone() as Arc<dyn StorageBackend>)
            .cdn_base_url("http://cdn.test".to_string())
            .key_prefix(String::new())
            .build()
            .expect("build storage service");

        let cache_service = CacheServiceBuilder::default()
            .minio_sub_path(String::new())
            .build()
            .expect("build cache service");

        let config = PerformanceConfig {
            allow_loopback_source_addresses: true, // this test's origin is 127.0.0.1
            ..PerformanceConfig::default()
        };
        let resize_service = Arc::new(
            ResizeService::with_config(storage_service, cache_service, config)
                .expect("build resize service"),
        );

        let params = Arc::new(ResizeQuery {
            url,
            width: Some(2),
            height: Some(2),
            format: ImageFormat::Png,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
        });

        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let resize_service = Arc::clone(&resize_service);
            let params = Arc::clone(&params);
            handles.push(tokio::spawn(async move {
                resize_service.resize(params.as_ref()).await
            }));
        }

        let mut ok_count = 0;
        for handle in handles {
            let result = handle.await.expect("resize task should not panic");
            assert!(
                result.is_ok(),
                "every follower should observe the leader's success, got {:?}",
                result
            );
            ok_count += 1;
        }
        assert_eq!(ok_count, 100);

        assert_eq!(
            request_count.load(Ordering::SeqCst),
            1,
            "expected exactly one origin fetch across 100 concurrent callers for the same key"
        );
        assert_eq!(
            storage_backend.uploads.load(Ordering::SeqCst),
            1,
            "expected exactly one encode+upload across 100 concurrent callers for the same key"
        );
    }

    /// A single-flight leader that fails must not poison followers
    /// indefinitely (#37's explicit failure-handling requirement): every
    /// follower must observe a real error and return promptly, not hang.
    #[tokio::test]
    async fn leader_failure_is_propagated_to_all_followers_not_hung_forever() {
        // No server listening at this address - every connection attempt
        // fails immediately, standing in for a leader that fails.
        let dead_url = "http://127.0.0.1:1".to_string();

        let storage_backend = Arc::new(CountingStorage::default());
        let storage_service = StorageServiceBuilder::default()
            .storage(storage_backend as Arc<dyn StorageBackend>)
            .cdn_base_url("http://cdn.test".to_string())
            .key_prefix(String::new())
            .build()
            .expect("build storage service");

        let cache_service = CacheServiceBuilder::default()
            .minio_sub_path(String::new())
            .build()
            .expect("build cache service");

        let config = PerformanceConfig {
            allow_loopback_source_addresses: true,
            ..PerformanceConfig::default()
        };
        let resize_service = Arc::new(
            ResizeService::with_config(storage_service, cache_service, config)
                .expect("build resize service"),
        );

        let params = Arc::new(ResizeQuery {
            url: dead_url,
            width: Some(2),
            height: Some(2),
            format: ImageFormat::Png,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
        });

        let mut handles = Vec::with_capacity(20);
        for _ in 0..20 {
            let resize_service = Arc::clone(&resize_service);
            let params = Arc::clone(&params);
            handles.push(tokio::spawn(async move {
                resize_service.resize(params.as_ref()).await
            }));
        }

        for handle in handles {
            let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
                .await
                .expect("follower must not hang forever waiting on a failed leader")
                .expect("resize task should not panic");
            assert!(result.is_err(), "a dead origin should surface as an error");
        }

        // The in-flight entry must have been cleaned up after the failure,
        // so a subsequent request for the same key starts a fresh attempt
        // rather than being permanently stuck.
        assert!(resize_service.in_flight.is_empty());
    }
}
