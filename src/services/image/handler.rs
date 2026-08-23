use crate::config::performance::PerformanceConfig;
use crate::models::params::{
    Crop, CropDimension, Gravity, Padding, ResizeQuery, ResizeType, TrimOptions,
    WatermarkPosition, WatermarkQuery,
};
use crate::services::image::source_guard;
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use derive_builder::Builder;
use fast_image_resize as fir;
use futures::StreamExt;
use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageDecoder, ImageEncoder, ImageFormat,
    Rgb, RgbImage, Rgba, RgbaImage,
};
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::warn;
use url::Url;

/// Default lossy-WebP encode quality (0.0-100.0, libwebp's own scale), used
/// when neither `ResizeQuery::webp_quality` nor `ResizeQuery::quality` is
/// set (#35). 82.0 is a commonly-cited "high quality, still meaningfully
/// smaller than lossless" WebP setting, and is corroborated by
/// `adr/0003-webp-measurement.md`'s matched-DSSIM sweep against the Kodak
/// corpus: 82 falls between the measured q80 bucket (median WebP/JPEG size
/// ratio 0.8497, n=23 images) and q85 bucket (0.8596, n=18), both inside the
/// flat, no-crossover 40-85 range where WebP is consistently 15-19% smaller
/// than JPEG at equal perceptual quality. That ADR explicitly re-checked
/// this constant and requested no change.
///
/// `pub` (like `encode_webp` itself) so `benches/encode.rs` can benchmark
/// the exact default production uses instead of a hardcoded duplicate that
/// could silently drift from it.
pub const DEFAULT_WEBP_QUALITY: f32 = 82.0;

/// Default JPEG encode quality (1-100, the `image` crate's own scale), used
/// when neither `ResizeQuery::jpeg_quality` nor `ResizeQuery::quality` is
/// set (#35). Matches `image::codecs::jpeg::JpegEncoder::new`'s own default
/// (`image-0.25.10/src/codecs/jpeg/encoder.rs:392`, `new_with_quality(w,
/// 75)`) - this crate's JPEG output already encoded at 75 before #35 (via
/// `DynamicImage::write_to`'s default encoder construction), just without a
/// named constant or a way to override it per-request. `adr/0003` measured
/// against this exact default and requested no change to it either.
pub const DEFAULT_JPEG_QUALITY: u8 = 75;

/// Default AVIF encode quality (0-100, `avifEncoder.quality`'s own scale
/// via `libavif`/AOM as of #68 - previously `ravif`'s scale via
/// `image::codecs::avif::AvifEncoder`, before AOM replaced it), used when
/// `ResizeQuery::quality` (the `q:{0-100}` processing option) isn't set.
/// `80` was `AvifEncoder::new`'s own pre-#68 default and `cavif`'s
/// reference default, and it survives a second look: `adr/0005`
/// re-measured against the encoders actually shipped today (libavif/AOM
/// and mozjpeg, superseding `adr/0004`, whose numbers are void) and the
/// owner reviewed the result and chose to keep 80.
///
/// **Read this before "optimising" it.** At 80 an AVIF is a median 1.14x
/// LARGER than default JPEG, larger on 19 of 24 Kodak images - because
/// AOM's q80 is a far more conservative quality point than rav1e's q80
/// was, roughly 2x better DSSIM. The widely-quoted "AVIF is ~24% smaller"
/// is real but lands at quality ~66, not here. Lowering 80 -> 66 is
/// therefore not a free saving: it buys bytes by shipping visibly worse
/// images, roughly matching what JPEG q75 already delivers. That trade was
/// considered and declined. AVIF is smaller than JPEG at every quality
/// from 40 to 90 (no crossover), so 80 is not a mistake - it is a
/// deliberately higher quality point.
///
/// If you change this, bump `CACHE_KEY_VERSION`: `generate_key` hashes the
/// REQUESTED `params.quality`, which is `None` for any url without an
/// explicit `q:` option, so the default is invisible to the cache key and
/// old entries would keep being served at the old quality forever.
pub const DEFAULT_AVIF_QUALITY: u8 = 80;

/// AVIF encode speed (0-10, `avifEncoder.speed`'s own scale via
/// `libavif`/AOM - 10 = fastest, 0 = slowest, driving AOM's `cpu-used`
/// internally). **Not the same scale `ravif`/`cavif` used before #68** -
/// this is the single most important finding from that change's own
/// report: naively keeping the old value (`4`, calibrated for rav1e) on
/// the new AOM-backed encoder cost 900ms+ median per encode (Kodak
/// corpus, quality=80) for barely any size/quality benefit over faster
/// settings. A real sweep (speed 4/6/8/9/10 at fixed quality=80, all 24
/// Kodak images) found AOM speed=6 both *smaller* and *lower-DSSIM*
/// (higher quality) than speed=8, while still ~6x faster than speed=4 -
/// speed 8-10 were faster still but strictly worse on both size and
/// DSSIM than speed=6, i.e. a genuinely dominated choice, not a
/// speed/quality tradeoff. `6` is therefore not "the same number as
/// before" preserved out of caution - it is a re-derived default for a
/// different encoder's differently-calibrated knob. See this change's own
/// report for the full sweep table and the corpus-wide matched-DSSIM
/// size-ratio measurement at this setting (median ~1.04x `ravif`'s old
/// output - i.e. slightly *larger*, not smaller, at matched perceptual
/// quality - materially different from the informally-cited "13%
/// smaller" figure; see that report for why).
///
/// `adr/0005` re-measured this setting end to end: 119.2 ms median encode
/// (range 81-197), 3.1x faster than the `ravif`/speed-4 configuration
/// `adr/0004` measured, with the content-dependent tail collapsing from
/// 986 ms to 197 ms.
pub const DEFAULT_AVIF_SPEED: u8 = 6;

/// Default background colour (#34) used both to flatten alpha before
/// encoding to a format with no alpha channel, and as the fill colour for
/// fully-transparent pixels when the output format keeps alpha (#60), when
/// `ResizeQuery::background` (the `bg:` processing option) isn't set.
///
/// Deliberately opaque white rather than imgproxy's own default of
/// "disabled" (no flattening at all unless `bg:` is given) - this crate
/// always flattens/normalises rather than treating it as opt-in, since a
/// bare PNG-with-transparency -> JPEG conversion with no explicit `bg:`
/// should still produce a sane image instead of the undefined-RGB fringing
/// #34 exists to fix.
pub const DEFAULT_BACKGROUND: [u8; 3] = [255, 255, 255];

/// The two width/height boxes `ImageService::effective_resize_box` (#51)
/// computes from a request's `width`/`height`/`zoom`/`dpr`/`min-width`/
/// `min-height`/`rotate` options: `resize_box` (fed into the actual resize
/// dispatch, enlarge-capped and min-floored) and `extend_box` (the
/// separate, *not* enlarge-capped target `extend` pads toward). See
/// `effective_resize_box`'s doc comment for why these two boxes are
/// deliberately not the same value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectiveSizing {
    resize_box: (Option<u32>, Option<u32>),
    extend_box: Option<(u32, u32)>,
}

#[derive(Clone, Builder)]
pub struct ImageService {
    // Limit concurrent downloads to prevent memory exhaustion
    download_semaphore: Arc<Semaphore>,
    // Bounds concurrent calls to the CPU-bound decode/resize/encode stage
    // (#30). Acquired with `try_acquire_owned` right before that stage so
    // load is shed with a distinguishable error the moment
    // `max_concurrent_processing` concurrent jobs are already running,
    // instead of letting an unbounded number queue up behind it.
    processing_semaphore: Arc<Semaphore>,
    config: PerformanceConfig,
}

/// Decode result shared by `ImageService::decode_with_limits`/
/// `decode_with_image_crate`/`decode_jpeg_scaled`: the pixels, the source's
/// EXIF `Orientation` (#33), its embedded ICC colour profile if any (#33),
/// and its raw EXIF metadata blob if any (#5) - see each function's own doc
/// comment for how the latter three are read off the source. A named alias
/// (clippy's `type_complexity`) rather than repeating the 4-tuple at every
/// one of those signatures.
type DecodedImage = (DynamicImage, Orientation, Option<Vec<u8>>, Option<Vec<u8>>);

impl ImageService {
    pub fn new() -> Result<Self> {
        Self::with_config(PerformanceConfig::default())
    }

    pub fn with_config(config: PerformanceConfig) -> Result<Self> {
        // Limit concurrent downloads based on configuration
        let download_semaphore = Arc::new(Semaphore::new(config.max_concurrent_downloads));

        // Limit concurrent CPU-bound processing based on configuration
        // (#30). The CPU stage itself now runs on tokio's own managed
        // blocking-thread pool via `spawn_blocking` rather than a
        // hand-rolled rayon pool - see `process_image`'s doc comment for
        // why rayon bought nothing here.
        let processing_semaphore = Arc::new(Semaphore::new(config.max_concurrent_processing));

        Ok(Self {
            download_semaphore,
            processing_semaphore,
            config,
        })
    }

    /// Builds an HTTP client pinned to a single, already-validated
    /// `(host, addr)` pair (#21).
    ///
    /// Two things matter here:
    /// - `.resolve(host, addr)` overrides DNS resolution for `host` to the
    ///   exact `addr` the caller already validated, so the actual TCP
    ///   connection can never observe a different address than the one
    ///   that was checked. Without this, a second DNS lookup at connect
    ///   time could return a different (attacker-controlled) address than
    ///   the one just validated - classic DNS rebinding.
    /// - `.redirect(Policy::none())` disables reqwest's own redirect
    ///   following entirely. Redirects are instead handled one hop at a
    ///   time by `fetch_validated`, which re-runs every check (scheme,
    ///   allowlist, resolved-address range) for each new location instead
    ///   of blindly trusting it.
    fn build_pinned_client(&self, host: &str, addr: SocketAddr) -> Result<Client> {
        Client::builder()
            .timeout(self.config.http_timeout)
            .tcp_keepalive(self.config.keep_alive_timeout)
            .redirect(Policy::none())
            .resolve(host, addr)
            .build()
            .context("Failed to build validated HTTP client")
    }

    /// Fetches `url`, enforcing the full SSRF guard from #21 on every hop:
    /// scheme allowlist, optional `ALLOWED_SOURCES` prefix allowlist, and
    /// blocked-IP-range resolution, re-validated after every redirect
    /// rather than only on the original URL. Returns the final (non-3xx)
    /// response with the download size cap still unenforced - that's
    /// `download_image`'s job, since it needs to stream the body.
    ///
    /// #57: an explicit `ALLOWED_SOURCES` match is authoritative for the
    /// private-IP-range block (RFC1918/CGNAT/IPv6-ULA) - `source_matches_allowlist`
    /// below is recomputed from `current` at the top of *every* iteration
    /// of this loop, so the bypass only ever applies to the one hop whose
    /// host actually matched, never to a redirect target that didn't.
    /// Loopback and link-local keep their own separate, unconditional
    /// flags (`allow_loopback_source_addresses`/`allow_link_local_source_addresses`)
    /// - an allowlist match never touches those, which is what keeps the
    /// cloud metadata endpoint (link-local) hard to reach even from an
    /// allowlisted origin's redirect.
    async fn fetch_validated(&self, url: &str) -> Result<Response> {
        let mut current = Url::parse(url).context("Invalid source URL")?;

        // `max_redirects` redirects means `max_redirects + 1` requests: the
        // original attempt plus up to `max_redirects` hops.
        for _ in 0..=self.config.max_redirects {
            source_guard::validate_scheme(&current)?;

            let source_matches_allowlist = match &self.config.allowed_sources {
                Some(allowed) if !allowed.is_empty() => {
                    if !source_guard::is_allowed_source(&current, allowed) {
                        return Err(source_guard::SourceRejected::NotAllowlisted {
                            url: current.to_string(),
                        }
                        .into());
                    }
                    true
                }
                // No allowlist configured (or configured empty) - no
                // restriction on which URLs are fetched, but also no
                // private-range bypass: private ranges stay blocked unless
                // an operator has explicitly named this host.
                _ => false,
            };

            let host = current
                .host_str()
                .with_context(|| format!("Source URL '{current}' has no host"))?
                .to_string();
            let port = current
                .port_or_known_default()
                .with_context(|| format!("Unable to determine port for source URL '{current}'"))?;

            // Resolve (or decode a literal) exactly once, validate the
            // result, then pin the client to it - see `build_pinned_client`.
            let addr = source_guard::resolve_validated_addr(
                &host,
                port,
                self.config.allow_loopback_source_addresses,
                self.config.allow_link_local_source_addresses,
                source_matches_allowlist,
            )
            .await?;

            let client = self.build_pinned_client(&host, addr)?;
            let response = client
                .get(current.clone())
                .send()
                .await
                .with_context(|| format!("Request to source URL '{current}' failed"))?;

            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .with_context(|| {
                        format!("Redirect response from '{current}' has no Location header")
                    })?
                    .to_str()
                    .context("Redirect Location header is not valid UTF-8")?;

                current = current
                    .join(location)
                    .with_context(|| format!("Failed to resolve redirect location '{location}'"))?;
                continue;
            }

            return Ok(response);
        }

        anyhow::bail!(
            "Too many redirects while fetching source image (max {})",
            self.config.max_redirects
        )
    }

    /// Download an image from a URL with optimizations
    ///
    /// Returns `Bytes` rather than `Vec<u8>` (#31): the body is still read
    /// incrementally with the size cap enforced per chunk (unchanged from
    /// #22), but the final buffer is handed to callers as a refcounted
    /// `Bytes` - `.freeze()` on the accumulation `BytesMut` is a type
    /// conversion, not a copy - instead of a `Vec<u8>` that `process_image`
    /// used to re-copy wholesale into a fresh `Bytes` via
    /// `Bytes::copy_from_slice` before it could be moved into the blocking
    /// task. `process_image` now just clones the `Bytes` handle (an atomic
    /// refcount bump) to move it into that task.
    pub async fn download_image(&self, url: &str) -> Result<Bytes> {
        // Acquire semaphore to limit concurrent downloads
        let _permit = self
            .download_semaphore
            .acquire()
            .await
            .context("Failed to acquire download permit")?;

        let response = self.fetch_validated(url).await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to download image from {}: status {}",
                url,
                response.status()
            ));
        }

        // Cheap early rejection when the origin is honest about the size -
        // but this header is attacker-controlled and, for chunked transfer
        // encoding, simply absent, so it must never be the actual
        // enforcement point (#22).
        if let Some(content_length) = response.content_length() {
            if content_length > self.config.max_image_size {
                return Err(anyhow::anyhow!(
                    "Image too large: {} bytes (max: {} bytes)",
                    content_length,
                    self.config.max_image_size
                ));
            }
        }

        // Real enforcement: stream the body and abort the moment the
        // running total exceeds the cap, so a chunked-encoded (no
        // Content-Length) or dishonest origin can't buffer an unbounded
        // response in memory.
        let capacity_hint = response
            .content_length()
            .unwrap_or(0)
            .min(self.config.max_image_size) as usize;
        let mut buffer = BytesMut::with_capacity(capacity_hint);
        let mut total_len: u64 = 0;
        let mut stream = response.bytes_stream();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("Error while streaming image body")?;
            total_len += chunk.len() as u64;

            if total_len > self.config.max_image_size {
                return Err(anyhow::anyhow!(
                    "Image too large: exceeded {} bytes while streaming from {}",
                    self.config.max_image_size,
                    url
                ));
            }

            buffer.extend_from_slice(&chunk);
        }

        // `.freeze()` turns the mutable accumulation buffer into a
        // refcounted `Bytes` in place - no further copy of the image data.
        Ok(buffer.freeze())
    }

    /// Process image on tokio's managed blocking thread pool, bounded by
    /// `processing_semaphore` (#30).
    ///
    /// ## Why `spawn_blocking`, not the rayon pool this used to run on
    ///
    /// Rayon's entire value proposition is work-stealing parallelism
    /// *within* a single job (`par_iter`, `rayon::join`, `rayon::scope`,
    /// `par_chunks`). Grepping this crate for all four returns zero hits -
    /// `process_image_blocking_with_limits` is a strictly sequential
    /// decode -> resize -> encode for one image, so nothing here ever
    /// fans a job out across rayon's pool. The old `cpu_pool.spawn(..)`
    /// call used rayon purely as a hand-rolled blocking-task pool, which
    /// bought two real problems for zero benefit: the queue in front of it
    /// was unbounded (no backpressure - see the semaphore below), and
    /// rayon's own worker count was fixed at startup rather than scaling
    /// with the runtime the way tokio's blocking pool does. `spawn_blocking`
    /// gets the same "off the async runtime" property with dynamic sizing
    /// and no separate pool to manage, at zero intra-image parallelism
    /// cost since there was never any intra-image parallelism to lose.
    ///
    /// ## Load shedding
    ///
    /// `processing_semaphore.try_acquire_owned()` is non-blocking: when
    /// `max_concurrent_processing` jobs are already running, this returns
    /// immediately with an error containing "permit" rather than queueing
    /// the caller behind an unbounded backlog -
    /// `AppError::classify_resize_error` (`src/modules/utils/err.rs`, owned
    /// separately) already maps any message containing "permit" or
    /// "cancelled" to `503 Service Unavailable`, so this lands there
    /// without needing a new error variant.
    ///
    /// ## Cancellation on caller disconnect
    ///
    /// The permit is moved into the blocking closure so the semaphore
    /// reflects real in-flight work for its whole duration, not just the
    /// hand-off. Before doing any decode/resize/encode work, the closure
    /// checks `tx.is_closed()` - true if `rx` (and therefore the
    /// `process_image` future `rx.await` is driving) has already been
    /// dropped, which is exactly what happens when the caller's request
    /// future is cancelled (e.g. the client disconnected upstream and axum
    /// drops the whole response future). A blocking-pool task that was
    /// merely queued, not yet running, when that happened skips the CPU
    /// work entirely instead of paying full decode/resize/encode cost for a
    /// response nobody will read. A task already mid-decode when the
    /// disconnect happens still runs to completion - Rust has no
    /// preemption point inside synchronous decode/resize/encode calls - so
    /// this bounds the *queued*, not in-flight, waste.
    pub async fn process_image(
        &self,
        image_bytes: &Bytes,
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        // Watermark fetch (#52), done *before* acquiring the CPU-bound
        // `processing_semaphore` permit below - this is network I/O, not
        // CPU work, so it must not hold a processing slot idle while it
        // waits on the network. Reuses `download_image` - and therefore
        // `fetch_validated`'s full SSRF guard (#21/#57) - unconditionally,
        // whether the URL came from the request's own `wmu:` option or this
        // deployment's configured `WATERMARK_URL` default: a watermark URL
        // is just as much an attacker-reachable fetch target as the main
        // source URL when it's caller-supplied, and treating the
        // operator-configured default through the same guarded path is
        // simpler than maintaining two fetch code paths for one option.
        let watermark_bytes = self.download_watermark_if_needed(params).await?;

        let permit = match Arc::clone(&self.processing_semaphore).try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                anyhow::bail!(
                    "No processing permit available: {} concurrent image processing jobs already running",
                    self.config.max_concurrent_processing
                );
            }
        };

        // Cheap: an `Arc`-backed refcount bump, not a copy of the image
        // bytes (#31). Same for `watermark_bytes` (also `Bytes`).
        let image_bytes = image_bytes.clone();
        let params = params.clone();
        let config = self.config.clone();

        let (tx, rx) = tokio::sync::oneshot::channel();

        tokio::task::spawn_blocking(move || {
            // Held for the lifetime of this closure so the semaphore keeps
            // reflecting real concurrency until the work is actually done,
            // not just until it was handed to the blocking pool.
            let _permit = permit;

            if tx.is_closed() {
                // The caller already disconnected while this task was
                // queued - drop the work rather than pay full CPU cost for
                // a result nobody will receive.
                return;
            }

            let result = Self::process_image_blocking_with_limits_and_watermark(
                &image_bytes,
                &params,
                &config,
                watermark_bytes.as_deref(),
            );
            let _ = tx.send(result);
        });

        rx.await.context("Image processing task was cancelled")?
    }

    /// Resolves and fetches the watermark image for `params`, if `params`
    /// requests one (#52). `None` when `params.watermark` is `None` -
    /// watermarking is off, nothing to fetch.
    ///
    /// URL resolution order: the request's own `wmu:` (`WatermarkQuery::url`)
    /// takes priority over this deployment's configured `WATERMARK_URL`
    /// default. `wm:` with neither is a clear error rather than silently
    /// skipping the watermark - a caller (or operator) who asked for one
    /// should be told it couldn't be honoured, not served a response that
    /// silently doesn't match what they asked for.
    async fn download_watermark_if_needed(&self, params: &ResizeQuery) -> Result<Option<Bytes>> {
        let Some(watermark) = &params.watermark else {
            return Ok(None);
        };

        let url = watermark
            .url
            .clone()
            .or_else(|| self.config.watermark_url.clone())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "watermark requested (wm:) but no watermark image is available - set \
                     WATERMARK_URL or pass wmu:{{base64url}} in the request"
                )
            })?;

        let bytes = self
            .download_image(&url)
            .await
            .context("Failed to download watermark image")?;
        Ok(Some(bytes))
    }

    /// CPU-intensive image processing with optimizations
    ///
    /// Visibility note: this is `pub` (rather than private) solely so that
    /// `benches/pipeline.rs` can call the exact same decode/resize/encode
    /// logic production traffic goes through, without duplicating it in the
    /// benchmark. No behaviour was changed to make this possible.
    ///
    /// This is a thin wrapper around
    /// [`Self::process_image_blocking_with_limits`] using
    /// [`PerformanceConfig::default`]'s resolution limits (50 source MP,
    /// 4096x4096 output), since the benchmark has no `ImageService`/config
    /// instance to draw real limits from. `process_image` (the real,
    /// production call path) calls `process_image_blocking_with_limits`
    /// directly with the service's actual configured limits.
    pub fn process_image_blocking(
        image_bytes: &[u8],
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        Self::process_image_blocking_with_limits(image_bytes, params, &PerformanceConfig::default())
    }

    /// [`Self::process_image_blocking_with_limits_and_watermark`] with no
    /// watermark bytes - kept as its own function (rather than inlining
    /// `None` at every call site) so the ~20 existing tests and benches
    /// calling this 3-argument form are unaffected by #52.
    fn process_image_blocking_with_limits(
        image_bytes: &[u8],
        params: &ResizeQuery,
        config: &PerformanceConfig,
    ) -> Result<(Vec<u8>, String)> {
        Self::process_image_blocking_with_limits_and_watermark(image_bytes, params, config, None)
    }

    /// Decode/resize/encode a single image, enforcing the resolution
    /// limits from #26:
    /// - the decoded *source* resolution (in megapixels) is checked
    ///   against `config.max_src_resolution_mp` using only the header
    ///   dimensions, before the image is fully decoded;
    /// - the requested *output* width/height are checked against
    ///   `config.max_output_width`/`max_output_height` before any resize
    ///   is attempted;
    /// - the `image` crate's decode `Limits` are configured explicitly
    ///   (width/height/alloc) instead of inheriting its accidental 512MiB
    ///   `max_alloc` default, as defense in depth behind the header check.
    ///
    /// #49: also the entry point for the animated-GIF/WebP path. When the
    /// requested output format can itself carry animation (`Gif`/`Webp`)
    /// *and* the detected source format can too, this dispatches to
    /// [`Self::decode_animation_source`]/[`Self::encode_animation`] instead
    /// of the single-`DynamicImage` pipeline below. Every other
    /// combination - including an animated source requested as a
    /// non-animatable format like `.jpg` - is unaffected and keeps the
    /// pre-#49 behaviour of decoding (and encoding) only the first frame.
    ///
    /// `watermark_bytes` (#52) is `Some(..)` exactly when `params.watermark`
    /// is also `Some(..)` (the caller - `process_image` - only fetches it in
    /// that case); compositing happens after grayscale/blur but *before*
    /// the #34/#60 alpha-flatten/normalise stage below, so a watermark's
    /// own alpha contributes to what gets flattened/normalised instead of
    /// slipping past it.
    fn process_image_blocking_with_limits_and_watermark(
        image_bytes: &[u8],
        params: &ResizeQuery,
        config: &PerformanceConfig,
        watermark_bytes: Option<&[u8]>,
    ) -> Result<(Vec<u8>, String)> {
        Self::check_output_dimensions(params, config)?;

        // Use faster image decoding with format hints
        let format = Self::detect_format_from_bytes(image_bytes);

        let wants_animatable_output = matches!(
            params.format,
            crate::models::params::ImageFormat::Gif | crate::models::params::ImageFormat::Webp
        );

        if wants_animatable_output {
            if let Some(source_format) = format {
                if let Some((frames, src_width, src_height)) =
                    Self::decode_animation_source(image_bytes, source_format, config)?
                {
                    if frames.len() > 1 {
                        return Self::encode_animation(frames, src_width, src_height, params);
                    }

                    // Exactly one frame decoded (the source wasn't
                    // actually animated, e.g. a static single-frame GIF
                    // requested as `.gif`) - reuse the frame already
                    // decoded above directly as the `DynamicImage` for the
                    // ordinary single-image pipeline below instead of
                    // decoding the source a second time.
                    let frame = frames
                        .into_iter()
                        .next()
                        .expect("checked frames.len() == 1 above");
                    let img = DynamicImage::ImageRgba8(frame.into_buffer());
                    return Self::encode_single_image(
                        img,
                        src_width,
                        src_height,
                        params,
                        None,
                        // #5: same reasoning as the `icc_profile: None` just
                        // above - `decode_animation_source` doesn't extract
                        // EXIF from the GIF/WebP animation decoders either,
                        // and there's no tracked orientation to have applied
                        // (animation frames are never auto-rotated), so
                        // there's nothing to keep and nothing to neutralize.
                        None,
                        false,
                        config,
                        watermark_bytes,
                    );
                }
            }
        }

        // Peek the header-only dimensions *before* touching the full
        // decode path, so a decompression-bomb-style source (tiny on disk,
        // huge decoded) is rejected without ever allocating the decoded
        // buffer.
        let (src_width, src_height) = Self::peek_dimensions(image_bytes, format)?;
        Self::check_source_resolution(src_width, src_height, config.max_src_resolution_mp)?;

        let (mut img, orientation, icc_profile, exif_metadata) =
            Self::decode_with_limits(image_bytes, format, config.max_src_resolution_mp, params)?;

        // #33: autorotate (imgproxy's `auto_rotate`/`ar` option, on by
        // default - see `ResizeQuery::autorotate`) must be applied *before*
        // any resize/crop math below, not after: `Orientation::Rotate90`/
        // `Rotate270` swap width and height, so `src_width`/`src_height`
        // (read immediately below, and used for every fit/fill/force/auto
        // decision and the #36 upscale guard) must already reflect the
        // corrected axes. Applying it after a crop would compose the crop
        // against the wrong axes entirely.
        //
        // #5: `exif_orientation_applied` records whether that happened, for
        // `encode_single_image`'s benefit - once `apply_orientation` runs,
        // the *pixels* are corrected but a kept, forwarded EXIF blob would
        // still carry the *original* (now-stale) Orientation tag, telling
        // any EXIF-aware viewer to rotate an already-rotated image a second
        // time. See `Self::neutralize_exif_orientation`'s doc comment for
        // how that's avoided.
        let exif_orientation_applied = params.autorotate && orientation != Orientation::NoTransforms;
        if params.autorotate {
            img.apply_orientation(orientation);
        }

        // #51: `trim` is always the *first* geometry operation - imgproxy's
        // own pipeline runs it before `scaleOnLoad`/`crop`/`scale`, ahead of
        // anything else that reads dimensions. Every dimension read below
        // (the explicit `c:` crop, `src_width`/`src_height`, the enlarge
        // guard, zoom/dpr/min-width/min-height, the resize itself) must
        // therefore see the *post-trim* image, not the original decode.
        let img = match &params.trim {
            Some(trim) => Self::apply_trim(&img, trim),
            None => img,
        };

        // Explicit `c:` crop (#50) - applied *after* trim but *before* any
        // resize math, matching imgproxy's own "trim, then crop before
        // resize" ordering (<https://docs.imgproxy.net/usage/processing#crop>,
        // see the `trim` comment just above). Every dimension computed
        // below (the upscale guard, `resize_dimensions`, the `auto`
        // orientation comparison) is therefore computed against the
        // trimmed-then-cropped image, not the original decoded one -
        // exactly as if the source had been that size all along.
        let img = match &params.crop {
            Some(crop) => Self::apply_crop(&img, crop),
            None => img,
        };

        let (src_width, src_height) = img.dimensions();

        Self::encode_single_image(
            img,
            src_width,
            src_height,
            params,
            icc_profile,
            exif_metadata,
            exif_orientation_applied,
            config,
            watermark_bytes,
        )
    }

    /// Resizes, filters and encodes a single already-decoded image to
    /// `params.format`. Split out of `process_image_blocking_with_limits`
    /// (#49) so it can be shared by that function's ordinary single-image
    /// path and its animated-source-but-turned-out-not-actually-animated
    /// fallback, which already has a `DynamicImage` in hand (the source's
    /// only frame) and would otherwise have to decode the source a second
    /// time to reach this same logic.
    ///
    /// `icc_profile` (#33) is threaded through explicitly rather than
    /// re-read here, since by the time this function runs the original
    /// `ImageDecoder` has already been consumed by `DynamicImage::from_decoder`.
    /// The animated-but-actually-single-frame fallback has no ICC profile to
    /// forward - `decode_animation_source` doesn't extract one from the
    /// GIF/WebP animation decoders - so it passes `None`. `exif_metadata`
    /// and `exif_orientation_applied` (#5) are threaded through the same
    /// way and for the same reason.
    ///
    /// # `strip_metadata` (#5) and the real per-format "keep" matrix
    ///
    /// `params.strip_metadata` (default `true`) gates whether `exif_metadata`
    /// is forwarded to the encoder at all - `true` (the default) drops it
    /// unconditionally, matching imgproxy's own `strip_metadata`/`sm`
    /// default. This covers EXIF only - see `ResizeQuery::strip_metadata`'s
    /// own doc comment (`src/models/params.rs`) for why the embedded ICC
    /// colour profile (`icc_profile`) is a separate, always-on concern.
    ///
    /// When metadata is kept, `exif_orientation_applied` decides whether the
    /// blob's `Orientation` tag needs neutralizing first
    /// (`Self::neutralize_exif_orientation`) - see that function's doc
    /// comment for why forwarding it unchanged after `autorotate` has
    /// already rotated the pixels would double-rotate the image in any
    /// EXIF-aware viewer.
    ///
    /// Whether the resolved EXIF bytes actually reach the output depends on
    /// what each encoder below can do with them - not uniform across
    /// formats, exactly like `icc_profile` already isn't (see that field's
    /// own comment on the JPEG/AVIF branches below):
    /// - **JPEG**: written as a raw `APP1` marker by `Self::encode_jpeg`
    ///   (mozjpeg's `CompressStarted::write_marker`, the same primitive
    ///   `write_icc_profile` already uses for `APP2`) - mozjpeg has no
    ///   higher-level EXIF API, so this crate builds the `"Exif\0\0"`-
    ///   prefixed segment by hand.
    /// - **PNG**: `image::codecs::png::PngEncoder::set_exif_metadata` -
    ///   PNG's `eXIf` chunk is a real, standard part of the format
    ///   (verified against `image-0.25.10/src/codecs/png.rs`: the decoder
    ///   already reads it too, which is where `exif_metadata` above comes
    ///   from for a PNG *source*).
    /// - **AVIF**: `crate::services::image::avif_codec::encode`, via
    ///   libavif's `avifImageSetMetadataExif` - #68 replaced the old
    ///   `image::codecs::avif::AvifEncoder` with this. EXIF is supported
    ///   even though this crate doesn't forward an ICC profile for AVIF
    ///   output - and that's not because libavif has no ICC API to call:
    ///   `avifImageSetProfileICC` exists too (see `avif_codec::encode`'s own
    ///   doc comment). It's simply not wired up, a deliberate scope cut, not
    ///   a hard capability limit the way it is for WebP/GIF below. (This
    ///   previously noted that AVIF was encode-only. It is not any more -
    ///   #67 added decode via libavif/dav1d, see
    ///   `crate::services::image::avif_codec` and the AVIF arm of
    ///   `decode_with_limits` above - so an AVIF *source*'s metadata now
    ///   reaches this path too, not only JPEG/PNG/WebP sources.)
    /// - **WebP**: **unsupported, always** - this crate's lossy WebP output
    ///   goes through the standalone `webp` crate (`Self::encode_webp`), not
    ///   `image`'s own `WebPEncoder` (see that function's own doc comment
    ///   for why); `webp` 0.3.1's `Encoder` has no EXIF/ICC API whatsoever
    ///   (verified against its public API - `new`/`from_image`/`from_rgb`/
    ///   `from_rgba`/`encode`/`encode_lossless`/`encode_simple`/
    ///   `encode_advanced`, nothing metadata-related). `sm:0` against a
    ///   `.webp` output is therefore a no-op - not a bug, a real limitation
    ///   of the encoder this crate uses, same as ICC already is for this
    ///   format.
    /// - **GIF**: **unsupported, always** - neither `image`'s `GifEncoder`
    ///   nor the GIF format itself (via this crate's decoder) has any EXIF
    ///   concept (verified: no `exif`/`Exif` hit anywhere in
    ///   `image-0.25.10/src/codecs/gif.rs`, decoder or encoder). Same
    ///   "no-op, not a bug" note as WebP.
    // #5 pushes this from 7 to 9 parameters, past clippy's default
    // `too_many_arguments` threshold (8) - already precedented in this crate
    // (`src/services/cache/handler.rs`'s `params_with_enlarge` test helper
    // carries the same allow). Bundling `exif_metadata`/
    // `exif_orientation_applied` into a struct alongside `icc_profile` was
    // considered and rejected: every one of these is a distinct, independent
    // per-call value threaded through from two different call sites (the
    // main decode path and the animated-single-frame fallback), not a
    // cohesive "options" bag a caller configures - a wrapper type here would
    // just move the same nine values one level of indirection away, without
    // making any call site more readable.
    #[allow(clippy::too_many_arguments)]
    fn encode_single_image(
        img: DynamicImage,
        src_width: u32,
        src_height: u32,
        params: &ResizeQuery,
        icc_profile: Option<Vec<u8>>,
        exif_metadata: Option<Vec<u8>>,
        exif_orientation_applied: bool,
        config: &PerformanceConfig,
        watermark_bytes: Option<&[u8]>,
    ) -> Result<(Vec<u8>, String)> {
        // #5: resolve once, up front, what (if anything) actually gets
        // forwarded to whichever format-specific encoder runs below -
        // `params.strip_metadata` gates it entirely, and a kept blob whose
        // pixels were just auto-rotated must have its `Orientation` tag
        // neutralized first (see `Self::neutralize_exif_orientation`'s doc
        // comment) or dropped outright if that can't be done safely.
        let exif_metadata: Option<Vec<u8>> = if params.strip_metadata {
            None
        } else {
            exif_metadata.and_then(|raw| {
                if exif_orientation_applied {
                    Self::neutralize_exif_orientation(&raw)
                } else {
                    Some(raw)
                }
            })
        };

        // #51: folds the #36 enlarge guard together with `zoom`, `dpr`,
        // `min-width`/`min-height` and the `rotate` axis swap into the box
        // actually fed to the resize dispatch below (`resize_box`), plus
        // the separate, not-enlarge-capped box `extend` needs
        // (`extend_box`) - see `Self::effective_resize_box`'s doc comment
        // for the full ordering rationale (it supersedes the plain "cap
        // each requested dimension to the source's" #36 guard this used to
        // be, folding that exact same rule in as one step of a larger
        // calculation).
        let sizing = Self::effective_resize_box(params, src_width, src_height);
        let (effective_width, effective_height) = sizing.resize_box;

        // Use faster resize algorithms for different scenarios
        let filter = match (effective_width, effective_height) {
            // For thumbnails, use faster Triangle filter
            (Some(w), Some(h)) if w <= 300 && h <= 300 => FilterType::Triangle,
            // For high quality, use Lanczos3
            _ => FilterType::Lanczos3,
        };

        let img = Self::resize_and_filter(
            &img,
            effective_width,
            effective_height,
            filter,
            params,
            src_width,
            src_height,
        )?;

        // #51: `extend` then `padding`, matching imgproxy's own pipeline
        // order (`mainPipeline`: `applyFilters`, then `extend` /
        // `extendAspectRatio`, then `padding`). Both enlarge the canvas via
        // a `background` fill - resolved once here so `extend` and
        // `padding` composite onto the identical colour #34/#60's
        // alpha-flatten/normalise step below would otherwise have used.
        let background = params.background.unwrap_or(DEFAULT_BACKGROUND);

        let img = match sizing.extend_box {
            // Mirrors imgproxy's own precondition: `extend` only pads
            // toward a *complete* target canvas, i.e. both axes of the
            // (zoom/dpr-scaled, not enlarge-capped) requested box must be
            // known - see `Self::effective_resize_box`'s doc comment for
            // why `extend_box` is deliberately *not* the same, enlarge-
            // capped box the resize step above used.
            Some((target_w, target_h)) if params.extend => {
                Self::apply_extend(img, target_w, target_h, background)
            }
            _ => img,
        };

        let img = match &params.padding {
            Some(padding) => Self::apply_padding(img, padding, background),
            None => img,
        };

        // #51 safety net: `padding`/`extend` can grow the canvas past what
        // the #26 output-dimension cap (already checked pre-decode against
        // the *requested* size) allows, since padding is pure pixel
        // addition, not part of the zoom/dpr-scaled request that check
        // covers. Re-check the actual, final dimensions here rather than
        // letting an unbounded `pd:` value slip past #26 entirely.
        let (final_width, final_height) = img.dimensions();
        if final_width > config.max_output_width || final_height > config.max_output_height {
            anyhow::bail!(
                "Requested output dimensions too large after extend/padding: {final_width}x{final_height} exceeds maximum {}x{}",
                config.max_output_width,
                config.max_output_height
            );
        }

        // Watermark compositing (#52). Deliberately placed here - after
        // every pixel-content transform above (resize/grayscale/blur), but
        // *before* the #34/#60 alpha-flatten/normalise stage below. Doing
        // this any later would let the watermark's own alpha slip past
        // that stage untouched (e.g. a semi-transparent watermark edge
        // encoded to JPEG would keep undefined/fringing RGB under partial
        // alpha instead of being properly flattened); doing it any earlier
        // (e.g. before resize) would resize the watermark along with the
        // base image instead of at its own requested size/scale.
        let img = match (watermark_bytes, &params.watermark) {
            (Some(watermark_bytes), Some(watermark)) => {
                Self::apply_watermark(img, watermark_bytes, watermark)?
            }
            _ => img,
        };

        // Optimize encoding based on format
        // #53: `gen_server` (OpenAPI codegen) was deleted; `ImageFormat` is
        // now hand-written in `src/models/params.rs`. #49 adds `Avif` and
        // `Gif` alongside the original three variants.
        let (output_format, content_type) = match params.format {
            crate::models::params::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            crate::models::params::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            crate::models::params::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
            crate::models::params::ImageFormat::Avif => (ImageFormat::Avif, "image/avif"),
            crate::models::params::ImageFormat::Gif => (ImageFormat::Gif, "image/gif"),
            // `crate::modules::negotiation::resolve` always resolves `Auto`
            // to a concrete format before a `ResizeQuery` is ever built
            // (`crate::modules::api::resize::handle`) - reaching this arm
            // is a bug in that call site, not a real request. Surfaced as
            // an error rather than a panic: this stage decodes/encodes
            // attacker-supplied bytes, and a panic here would take the
            // whole worker down (see `[profile.perf]`'s `panic = "unwind"`
            // note in `Cargo.toml`) for what is, from the caller's
            // perspective, an ordinary 500.
            crate::models::params::ImageFormat::Auto => {
                anyhow::bail!("internal error: unresolved Auto format reached the image encoder")
            }
        };

        // Alpha handling (#34/#60). Gated on `img.has_alpha()` so a source
        // that never carried an alpha channel (the common case) pays zero
        // extra cost - there's nothing to flatten or normalise.
        let img = if img.has_alpha() {
            let background = params.background.unwrap_or(DEFAULT_BACKGROUND);
            match output_format {
                // JPEG has no alpha channel - flatten (composite) onto
                // `background` explicitly instead of letting the encoder's
                // own to_rgb8() conversion drop the channel outright
                // (`cast_in_color_space`, a raw channel drop, not a
                // composite - see #34's issue body for the exact vendored
                // code path this replaces). Without this, transparent
                // pixels whose RGB was never meaningful (undefined/garbage
                // under alpha=0) show up as visible fringing instead of a
                // clean edge against the configured background.
                ImageFormat::Jpeg => Self::flatten_onto_background(&img, background),
                // PNG/WebP/AVIF/GIF keep their alpha channel, so this is
                // not a flatten - but a fully-transparent pixel's RGB is
                // invisible by definition, and the source frequently
                // carries undefined/noisy values there that cost real
                // encoded bytes for a region nobody can see (#60).
                // Normalising just those pixels to a constant lets
                // DEFLATE/VP8L/AV1/LZW collapse the region instead. Only
                // exactly `alpha == 0` pixels are touched - partial
                // transparency is visibly blended with whatever is behind
                // it, so rewriting its RGB would be a real (lossy) visual
                // change, not the lossless one this is meant to be.
                _ => DynamicImage::ImageRgba8(Self::normalize_transparent_pixels(
                    img.to_rgba8(),
                    background,
                )),
            }
        } else {
            img
        };

        // WebP goes through the dedicated `webp` crate (`Self::encode_webp`),
        // not `DynamicImage::write_to` - the `image` crate's own WebP
        // encoder (`image-webp`, pulled in via the `webp` cargo feature) is
        // lossless-only, which produced far larger output than intended.
        // PNG uses an explicit `PngEncoder` (rather than `write_to`'s
        // default) to pick `CompressionType::Best` over the crate's own
        // default of `Fast` (`image-0.25.10/src/codecs/png.rs`,
        // `CompressionType::default()`) - a real, dependency-free size win
        // for #60 that doesn't touch `Cargo.toml`: `PngEncoder` and its
        // `CompressionType`/`FilterType` enums are already part of the
        // `image` crate's own public API under the `png` feature this crate
        // already depends on, not a new dependency. JPEG now uses an
        // explicit `JpegEncoder::new_with_quality` (#35) instead of
        // `write_to`'s implicit default-quality construction, so
        // `params.quality`/`params.jpeg_quality` actually reach the encoder
        // - and, being an explicit `JpegEncoder` rather than whatever
        // `write_to` builds internally, it's also the only way to reach
        // `ImageEncoder::set_icc_profile` (#33), so the same encoder value
        // now carries both the requested quality and the forwarded colour
        // profile. AVIF (#49) similarly resolves an explicit quality rather
        // than relying on `write_to`'s default, but as of #68 it's handed to
        // `crate::services::image::avif_codec::encode` (libavif/AOM), not
        // `image::codecs::avif::AvifEncoder::new_with_speed_quality` - so
        // `params.quality` (the `q:` processing option) still overrides
        // `DEFAULT_AVIF_QUALITY` the same way #49 originally wired it up,
        // just against AOM's own `avifEncoder.quality`/`speed` fields now,
        // not `ravif`'s. Measured in
        // `adr/0005-avif-measurement-libavif-mozjpeg.md`, which supersedes
        // `adr/0004` (whose numbers were `ravif`'s and are void post-#68).
        // GIF is unaffected and keeps going through `write_to` exactly as
        // before.
        //
        // PNG has no quality knob in `params` to honour - `CompressionType`
        // is a fixed lossless setting, not a continuous 0-100 scale, and
        // `fq:png:N` is rejected at parse time
        // (`src/modules/url/options.rs`) rather than silently accepted and
        // ignored here.
        //
        // #33: `icc_profile`, if the source carried one, is forwarded to
        // the PNG/JPEG encoders - both support embedding it
        // (`image-0.25.10/src/codecs/{png,jpeg/encoder}.rs`). WebP and AVIF
        // are the formats this can't cover today, for two different
        // reasons: the `webp` crate (0.3.1, this service's only route to
        // *lossy* WebP encoding - see the doc comment above) has no
        // ICC-profile API at all, while `avif_codec::encode` (libavif/AOM,
        // see that function's own doc comment) simply doesn't thread one
        // through, even though libavif exposes `avifImageSetProfileICC` for
        // exactly this - a real gap, but a "not wired up" one for AVIF, not
        // a hard capability limit the way it is for WebP. Fixing WebP would
        // mean switching encoders or patching raw ICC chunks into the
        // container format by hand; fixing AVIF would mean wiring up the
        // libavif call that already exists. Neither is a small addition, so
        // both are left as follow-up rather than half-done here.
        //
        // #5: `exif_metadata` (resolved above, already `None` if
        // `params.strip_metadata` or unavailable) follows a *different*
        // per-format matrix than `icc_profile` - notably AVIF *can* carry it
        // even though it can't carry ICC. See `encode_single_image`'s own
        // doc comment for the full breakdown with citations; each branch
        // below only handles the mechanics.
        let output_bytes = match output_format {
            ImageFormat::WebP => {
                let lossless = params.webp_lossless.unwrap_or(false);
                // `quality` is meaningless (and unused by `encode_webp`)
                // when `lossless` is set - see that function's own doc
                // comment - but still resolved unconditionally here since
                // that's simpler than threading an `Option` through just to
                // skip computing a value nothing will read.
                let quality = params
                    .webp_quality
                    .or(params.quality)
                    .map(f32::from)
                    .unwrap_or(DEFAULT_WEBP_QUALITY);

                Self::encode_webp(&img, quality, lossless)
                    .context(format!("Failed to encode image to {:?}", output_format))?
            }
            ImageFormat::Png => {
                let icc_ref = icc_profile.as_deref();
                let exif_ref = exif_metadata.as_deref();

                Self::encode_png(&img, icc_ref, exif_ref)
                    .context(format!("Failed to encode image to {:?}", output_format))?
            }
            // #76: routed through `Self::encode_jpeg` (mozjpeg/libjpeg-turbo)
            // instead of `image::codecs::jpeg::JpegEncoder` so
            // `jpeg_progressive`/`jpeg_no_subsampling` - and, via
            // `encode_with_max_bytes`, `max_bytes` - actually reach the
            // encoder; see `encode_jpeg`'s own doc comment for why
            // `image`'s encoder can't do any of the three. `progressive`/
            // `no_subsampling` each fall back to this deployment's
            // configured default (`PerformanceConfig::jpeg_progressive_default`/
            // `jpeg_no_subsampling_default`) when the request's own `jpgo:`
            // segment doesn't say - both default `false`, reproducing
            // exactly this crate's pre-#76 JPEG output when neither new
            // option is used.
            ImageFormat::Jpeg => {
                let quality = params
                    .jpeg_quality
                    .or(params.quality)
                    .unwrap_or(DEFAULT_JPEG_QUALITY);
                let progressive = params
                    .jpeg_progressive
                    .unwrap_or(config.jpeg_progressive_default);
                let no_subsampling = params
                    .jpeg_no_subsampling
                    .unwrap_or(config.jpeg_no_subsampling_default);
                let icc_ref = icc_profile.as_deref();
                let exif_ref = exif_metadata.as_deref();

                let result = match params.max_bytes {
                    // `max_bytes` is only meaningful for JPEG here - see
                    // `Self::MAX_BYTES_SEARCH_ATTEMPTS`'s doc comment for
                    // the measured encode-cost reasoning behind not
                    // offering it for AVIF (roughly two orders of
                    // magnitude more expensive per encode), and PNG/GIF
                    // have no continuous quality axis to search over at
                    // all (`fq:png:N` is already rejected at parse time
                    // for the same reason).
                    Some(max_bytes) => Self::encode_with_max_bytes(max_bytes, quality, |q| {
                        Self::encode_jpeg(&img, q, progressive, no_subsampling, icc_ref, exif_ref)
                    }),
                    None => {
                        Self::encode_jpeg(&img, quality, progressive, no_subsampling, icc_ref, exif_ref)
                    }
                };

                result.context(format!("Failed to encode image to {:?}", output_format))?
            }
            // #68: routed through `avif_codec::encode` (`libavif`/AOM)
            // instead of `image::codecs::avif::AvifEncoder`
            // (`ravif`/`rav1e`, removed entirely - see `Cargo.toml`'s
            // `image` dependency comment). `DEFAULT_AVIF_SPEED` was
            // re-derived for AOM's own speed scale, not carried over
            // unchanged from `ravif` - see that constant's own doc comment
            // in this file for the real measurement behind the new value
            // and `avif_codec::encode`'s doc comment for why the two
            // encoders' `speed` knobs aren't interchangeable numbers.
            ImageFormat::Avif => {
                let quality = params.quality.unwrap_or(DEFAULT_AVIF_QUALITY);
                let exif_ref = exif_metadata.as_deref();

                // #5: `avif_codec::encode` writes EXIF via
                // `avifImageSetMetadataExif` - the ICC comment above this
                // match still applies unchanged: AVIF has no ICC route in
                // this crate (see that function's own doc comment).
                crate::services::image::avif_codec::encode(
                    &img,
                    quality,
                    DEFAULT_AVIF_SPEED,
                    exif_ref,
                )
                .context(format!("Failed to encode image to {:?}", output_format))?
            }
            // GIF (#49) is the only remaining supported format and has no
            // quality/ICC handling of its own - it keeps going through
            // `write_to` exactly as every format did before #35/#33/#49
            // carved out explicit encoders for the others. Any value the
            // match above doesn't already handle also lands here, but
            // `output_format` is only ever constructed from
            // `crate::models::params::ImageFormat` a few lines above, whose
            // variants are all covered by this match once `Auto` has been
            // resolved to a concrete format upstream, so this arm is GIF in
            // practice.
            _ => {
                // Pre-allocate buffer based on estimated size - only
                // meaningful for the `write_to` path below; the WebP path
                // above gets its output buffer from libwebp itself.
                let estimated_size = Self::estimate_output_size(&img, &output_format);
                let mut buf = Cursor::new(Vec::with_capacity(estimated_size));

                img.write_to(&mut buf, output_format)
                    .context(format!("Failed to encode image to {:?}", output_format))?;

                buf.into_inner()
            }
        };

        Ok((output_bytes, content_type.to_string()))
    }

    /// The resize-type dispatch (#59) plus the `grayscale`/`blur_sigma`
    /// filters, shared by [`Self::encode_single_image`] above and
    /// [`Self::encode_animation`] below (#49) - every frame of an animated
    /// source is resized and filtered one at a time through this exact same
    /// function, so an animated request behaves identically, frame by
    /// frame, to the single-image path for the same parameters.
    fn resize_and_filter(
        img: &DynamicImage,
        effective_width: Option<u32>,
        effective_height: Option<u32>,
        filter: FilterType,
        params: &ResizeQuery,
        src_width: u32,
        src_height: u32,
    ) -> Result<DynamicImage> {
        // Resize image with optimized logic. `effective_width`/
        // `effective_height` are already capped to the source resolution
        // per axis unless `enlarge` is set (by the caller), and every
        // branch below - `resize` (fit), `resize_to_fill` (fill/auto-as-
        // fill), `resize_exact` (force) - only ever shrinks each axis to at
        // most its capped target, so none of them can upscale past the
        // source: the #36 guard holds for every resize type, not just
        // fill. #63 stage 1: the actual resampling is done by
        // `fast_image_resize` (SIMD, pure Rust) via the `Self::fir_*`
        // helpers below instead of `DynamicImage::resize`/
        // `resize_to_fill`/`resize_exact` directly - resize was the
        // single most expensive stage in the committed benchmark baseline
        // (17.39ms lanczos3 downscale vs 6.78ms JPEG decode), not decode.
        // Every `fir_*` helper reproduces the exact target-dimension math
        // its `image`-crate counterpart uses internally (see their doc
        // comments), so only the resampling kernel changes here - the
        // fit/fill/force/auto branching below, and the #36 enlarge-guard
        // reasoning it documents, are unchanged.
        let img = match (effective_width, effective_height) {
            (Some(w), None) => Self::fir_resize(img, w, u32::MAX, filter)?,
            (None, Some(h)) => Self::fir_resize(img, u32::MAX, h, filter)?,
            (Some(w), Some(h)) => match params.resize_type {
                // Fit inside the box, preserving aspect ratio - neither
                // output dimension exceeds `w`/`h`. This is also what a
                // lone width or height already did above, so `Fit` (the
                // default, see `ResizeType`) keeps that existing behaviour
                // consistent once both dimensions are given (#59).
                ResizeType::Fit => Self::fir_resize(img, w, h, filter)?,
                // Cover the box, preserving aspect ratio, then crop the
                // overflow. `fir_resize_to_fill` crops to exactly `w x h`
                // itself (originally always centred, matching
                // `image-0.25.10`'s `DynamicImage::resize_to_fill` -
                // `image-0.25.10/src/images/dynimage.rs:943-962` - so no
                // separate manual crop step was needed, #36) - #50 replaced
                // that hardcoded centre with `params.gravity`, so which part
                // of the overflow survives is now caller-controlled instead
                // of always the middle.
                ResizeType::Fill => Self::fir_resize_to_fill(img, w, h, filter, params.gravity)?,
                // Stretch to exactly `w x h`, ignoring aspect ratio.
                ResizeType::Force => Self::fir_resize_exact(img, w, h, filter)?,
                // imgproxy's documented `auto` rule
                // (https://docs.imgproxy.net/usage/processing#resizing-type):
                // "if both source and resulting dimensions have the same
                // orientation (portrait or landscape), imgproxy will use
                // `fill`. Otherwise, it will use `fit`." A box is treated
                // as landscape-or-square when width >= height and portrait
                // otherwise, applied identically to the source and the
                // requested box so the comparison is consistent.
                ResizeType::Auto => {
                    let src_landscape = src_width >= src_height;
                    let dst_landscape = w >= h;
                    if src_landscape == dst_landscape {
                        Self::fir_resize_to_fill(img, w, h, filter, params.gravity)?
                    } else {
                        Self::fir_resize(img, w, h, filter)?
                    }
                }
            },
            (None, None) => img.clone(),
        };

        // #51: `rotate`/`flip` come right after resize, matching imgproxy's
        // own pipeline (`processing/processing.go`'s `mainPipeline`: `scale`
        // then `rotateAndFlip`, ahead of `applyFilters` i.e. grayscale/blur
        // below) - applied here, inside the function shared with
        // `Self::encode_animation`, so an animated source is rotated/
        // flipped frame-by-frame exactly like the single-image path.
        // `rotate`'s effect on the resize *box* itself (the width/height
        // swap for 90/270) already happened inside
        // `Self::effective_resize_box` before this function was called -
        // this is just the actual pixel rotation of the now-resized image.
        let img = Self::apply_rotate(img, params.rotate);
        let img = Self::apply_flip(img, params.flip_horizontal, params.flip_vertical);

        // Apply filters efficiently
        let img = if let Some(true) = params.grayscale {
            img.grayscale()
        } else {
            img
        };

        let img = if let Some(sigma) = params.blur_sigma {
            if sigma > 0.0 { img.blur(sigma) } else { img }
        } else {
            img
        };

        Ok(img)
    }

    /// Decodes every frame of an animated GIF/WebP source (#49), enforcing
    /// both the existing #26 resolution guard (checked against the
    /// decoder's header-only dimensions, before any frame is decoded) and a
    /// new frame-*count* guard (`config.max_animation_frames`) #26's
    /// resolution check doesn't cover - a many-tiny-frames animation can be
    /// individually well within the resolution cap per frame while still
    /// being a real memory/CPU amplification via frame count alone.
    ///
    /// Returns `Ok(None)` when there is nothing useful to do here: either
    /// `source_format` is neither `Gif` nor `WebP` (only those two can
    /// carry animation in this crate), or the source is a `WebP` that isn't
    /// actually animated - in the latter case the ordinary single-image
    /// path decodes it instead. That's a deliberate choice, not just an
    /// optimisation to skip: `WebPDecoder`'s `AnimationDecoder::into_frames`
    /// upgrades every frame to RGBA unconditionally, even when the source
    /// has no alpha channel at all (`image-0.25.10/src/codecs/webp/decoder.rs`),
    /// whereas the ordinary single-image decode path preserves the source's
    /// real colour type (`Rgb8` for an alpha-less WebP). This doesn't
    /// currently change the *encoded* bytes for WebP output specifically -
    /// `Self::encode_webp` already always upconverts to RGBA before handing
    /// pixels to libwebp, regardless of which path decoded them - but it
    /// does avoid the frame iterator's extra decode-path overhead and a
    /// pointless `normalize_transparent_pixels` pass over an image with no
    /// real transparency, for what is expected to be the common case (a
    /// static WebP resized to another static WebP). GIF has no equivalent
    /// concern either way - `GifDecoder`'s single-image
    /// `ImageDecoder::color_type()` is unconditionally `Rgba8` too, so
    /// there is nothing to preserve by skipping the frame-iterator path for
    /// a single-frame GIF, and this crate takes it anyway (see the caller)
    /// to avoid decoding the source twice.
    ///
    /// Otherwise returns the decoded frames plus the shared canvas
    /// dimensions every frame is composited to - both `GifDecoder` and
    /// `WebPDecoder`'s `AnimationDecoder::into_frames()` already composite
    /// each frame to the *full* canvas size internally (disposal methods
    /// resolved, GIF's own partial-frame-rectangle handling included), so
    /// every `image::Frame` returned here is a same-size, ready-to-resize
    /// RGBA image - no manual compositing needed by the caller.
    fn decode_animation_source(
        image_bytes: &[u8],
        source_format: ImageFormat,
        config: &PerformanceConfig,
    ) -> Result<Option<(Vec<image::Frame>, u32, u32)>> {
        match source_format {
            ImageFormat::Gif => {
                let mut decoder = image::codecs::gif::GifDecoder::new(Cursor::new(image_bytes))
                    .context("Failed to read GIF header")?;
                decoder
                    .set_limits(Self::build_decode_limits(config.max_src_resolution_mp))
                    .context("GIF source exceeds configured decode limits")?;
                let (src_width, src_height) = decoder.dimensions();
                Self::check_source_resolution(src_width, src_height, config.max_src_resolution_mp)?;
                let frames = Self::collect_frames_capped(
                    decoder.into_frames(),
                    config.max_animation_frames,
                )?;
                Ok(Some((frames, src_width, src_height)))
            }
            ImageFormat::WebP => {
                let mut decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(image_bytes))
                    .context("Failed to read WebP header")?;
                if !decoder.has_animation() {
                    return Ok(None);
                }
                decoder
                    .set_limits(Self::build_decode_limits(config.max_src_resolution_mp))
                    .context("WebP source exceeds configured decode limits")?;
                let (src_width, src_height) = decoder.dimensions();
                Self::check_source_resolution(src_width, src_height, config.max_src_resolution_mp)?;
                let frames = Self::collect_frames_capped(
                    decoder.into_frames(),
                    config.max_animation_frames,
                )?;
                Ok(Some((frames, src_width, src_height)))
            }
            _ => Ok(None),
        }
    }

    /// Reads at most `max_frames` frames from `frames`, failing closed the
    /// moment a `max_frames + 1`th frame is seen instead of collecting
    /// every frame first and checking the count afterwards - a
    /// frame-count-bomb source (millions of tiny frames) must not be
    /// allowed to actually allocate that many decoded frames before being
    /// rejected.
    fn collect_frames_capped(
        frames: image::Frames<'_>,
        max_frames: usize,
    ) -> Result<Vec<image::Frame>> {
        let mut out = Vec::new();
        for (index, frame) in frames.enumerate() {
            if index >= max_frames {
                anyhow::bail!("animated source exceeds the maximum of {max_frames} frames");
            }
            out.push(frame.context("Failed to decode animation frame")?);
        }
        Ok(out)
    }

    /// Resizes/filters and encodes every frame of a genuinely multi-frame
    /// animated source (#49), preserving each frame's original delay.
    /// `frames.len() > 1` is the caller's (`process_image_blocking_with_limits`)
    /// responsibility to have already checked - a single-frame source takes
    /// the ordinary [`Self::encode_single_image`] path instead, which also
    /// handles every other output format this crate supports, not just
    /// `Gif`/`Webp`.
    fn encode_animation(
        frames: Vec<image::Frame>,
        src_width: u32,
        src_height: u32,
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        // Same upscale guard (#36) and thumbnail-vs-quality filter choice
        // as `encode_single_image`, computed once against the shared
        // canvas size every frame decodes to (see
        // `decode_animation_source`'s doc comment) rather than per frame -
        // every frame in one animation shares a single source resolution
        // by construction.
        let effective_width = params
            .width
            .map(|w| if params.enlarge { w } else { w.min(src_width) });
        let effective_height = params
            .height
            .map(|h| if params.enlarge { h } else { h.min(src_height) });
        let filter = match (effective_width, effective_height) {
            (Some(w), Some(h)) if w <= 300 && h <= 300 => FilterType::Triangle,
            _ => FilterType::Lanczos3,
        };

        let background = params.background.unwrap_or(DEFAULT_BACKGROUND);

        let mut output_frames = Vec::with_capacity(frames.len());
        for frame in frames {
            let delay = frame.delay();
            let img = DynamicImage::ImageRgba8(frame.into_buffer());
            let img = Self::resize_and_filter(
                &img,
                effective_width,
                effective_height,
                filter,
                params,
                src_width,
                src_height,
            )?;
            // Every animation frame keeps its alpha channel end to end
            // (GIF/WebP frames decode as RGBA here unconditionally - see
            // `decode_animation_source`) - only the #60
            // fully-transparent-pixel normalisation applies, never the
            // JPEG flatten branch `encode_single_image` also has, since
            // neither animated output format is JPEG.
            let rgba = Self::normalize_transparent_pixels(img.to_rgba8(), background);
            output_frames.push(image::Frame::from_parts(rgba, 0, 0, delay));
        }

        match params.format {
            crate::models::params::ImageFormat::Gif => Self::encode_animated_gif(output_frames),
            crate::models::params::ImageFormat::Webp => {
                Self::encode_animated_webp(output_frames, params)
            }
            // `process_image_blocking_with_limits` only ever calls this
            // function when `params.format` is `Gif` or `Webp` (see its
            // `wants_animatable_output` check) - reaching any other arm
            // would be a bug there, surfaced as an error rather than a
            // panic for the same reason `encode_single_image`'s `Auto` arm
            // is.
            _ => anyhow::bail!(
                "internal error: encode_animation reached with a non-animatable format"
            ),
        }
    }

    /// Encodes `frames` as an animated GIF via
    /// `image::codecs::gif::GifEncoder` (pure Rust, already part of this
    /// crate's `image` dependency's default features - see `ImageFormat`'s
    /// doc comment in `src/models/params.rs`). Each frame is independently
    /// palette-quantized by the underlying `gif` crate
    /// (`Frame::from_rgba_speed`, invoked internally by
    /// `GifEncoder::encode_frame`) - GIF has no truecolor mode at all, so
    /// per-frame quantization loss is inherent to the format, not something
    /// this crate's encoder choice introduces.
    fn encode_animated_gif(frames: Vec<image::Frame>) -> Result<(Vec<u8>, String)> {
        let mut buf = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut buf);
            encoder
                .set_repeat(image::codecs::gif::Repeat::Infinite)
                .context("Failed to set GIF repeat behaviour")?;
            encoder
                .encode_frames(frames)
                .context("Failed to encode animated GIF")?;
        }
        Ok((buf, "image/gif".to_string()))
    }

    /// Encodes `frames` as an animated WebP via the `webp` crate's
    /// `AnimEncoder` (real libwebp, `WebPAnimEncoder` FFI - already a
    /// dependency for this crate's static lossy-WebP path,
    /// [`Self::encode_webp`]).
    ///
    /// Verified rather than assumed: #49 explicitly calls out not to take
    /// "the `webp` crate has no animation encoding API" on faith, and it
    /// turns out that premise is wrong. `webp 0.3.1`'s
    /// `animation_encoder.rs` exposes `AnimEncoder`/`AnimFrame`
    /// unconditionally (no extra cargo feature beyond the crate's own
    /// `default = ["img"]`, already enabled here), wrapping libwebp's own
    /// `WebPAnimEncoderXxx` C functions directly - real animated WebP
    /// output *is* possible through the dependency this crate already has,
    /// no new one needed. What is *not* possible is animated WebP through
    /// `image`'s own bundled encoder (`image-webp 0.2.4`'s
    /// `WebPEncoder::encode` only ever writes a single `VP8L` chunk, no
    /// `ANIM`/`ANMF` support) - that half of the original assumption does
    /// hold, which is why this goes through the `webp` crate instead, not
    /// `image::codecs::webp::WebPEncoder`.
    fn encode_animated_webp(
        frames: Vec<image::Frame>,
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        if frames.is_empty() {
            anyhow::bail!("cannot encode an animated WebP with zero frames");
        }

        // Rebuilds each frame's cumulative *end* timestamp (milliseconds)
        // from its individual delay - `AnimFrame::from_rgba`'s `timestamp`
        // argument is a point on libwebp's animation timeline, not a
        // per-frame duration the way `image::Frame::delay` is.
        let mut timestamp_ms: i64 = 0;
        let mut timestamps = Vec::with_capacity(frames.len());
        let mut buffers = Vec::with_capacity(frames.len());
        for frame in frames {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let delay_ms = if denom == 0 {
                0
            } else {
                i64::from(numer) / i64::from(denom)
            };
            timestamp_ms = timestamp_ms.saturating_add(delay_ms);
            timestamps.push(i32::try_from(timestamp_ms).unwrap_or(i32::MAX));
            buffers.push(frame.into_buffer());
        }

        let (width, height) = buffers[0].dimensions();

        let mut webp_config = webp::WebPConfig::new()
            .map_err(|_| anyhow::anyhow!("failed to initialize libwebp animation encoder config"))?;
        // Same default quality as the static lossy-WebP path
        // (`Self::encode_webp`'s `DEFAULT_WEBP_QUALITY` call site) unless
        // overridden by `params.quality` - this crate doesn't currently
        // wire `quality` into the *static* WebP path either (a separate,
        // pre-existing gap noted in `DEFAULT_WEBP_QUALITY`'s own doc
        // comment), so keeping the animated path consistent with that
        // rather than silently diverging.
        let quality = params.quality.map(f32::from).unwrap_or(DEFAULT_WEBP_QUALITY);
        webp_config.quality = quality;
        webp_config.alpha_quality = quality as i32;

        let mut encoder = webp::AnimEncoder::new(width, height, &webp_config);
        encoder.set_loop_count(0); // loop forever, matching `Repeat::Infinite` in the GIF path above

        for (buffer, timestamp) in buffers.iter().zip(timestamps) {
            encoder.add_frame(webp::AnimFrame::from_rgba(
                buffer.as_raw(),
                buffer.width(),
                buffer.height(),
                timestamp,
            ));
        }

        let memory = encoder
            .try_encode()
            .map_err(|e| anyhow::anyhow!("Failed to encode animated WebP: {e:?}"))?;

        Ok((memory.to_vec(), "image/webp".to_string()))
    }

    /// Reproduces `image` crate's own aspect-ratio scaling formula exactly
    /// (image-0.25.10 `src/math/utils.rs::resize_dimensions` - `pub(crate)`
    /// there, so not callable directly) so the `fast_image_resize`-backed
    /// helpers below compute the *identical* target size
    /// `DynamicImage::resize`/`resize_to_fill` would have, before #63
    /// stage 1 swapped out only the resampling kernel underneath them.
    /// `fill=false` is the "fit inside the box" ratio (`ResizeType::Fit`,
    /// `min(wratio, hratio)`); `fill=true` is the "cover the box" ratio
    /// used as the intermediate step before `resize_to_fill`'s crop
    /// (`max(wratio, hratio)`).
    fn resize_dimensions(
        width: u32,
        height: u32,
        nwidth: u32,
        nheight: u32,
        fill: bool,
    ) -> (u32, u32) {
        let wratio = f64::from(nwidth) / f64::from(width);
        let hratio = f64::from(nheight) / f64::from(height);

        let ratio = if fill {
            f64::max(wratio, hratio)
        } else {
            f64::min(wratio, hratio)
        };

        let nw = (f64::from(width) * ratio).round().max(1.0) as u64;
        let nh = (f64::from(height) * ratio).round().max(1.0) as u64;

        if nw > u64::from(u32::MAX) {
            let ratio = f64::from(u32::MAX) / f64::from(width);
            (
                u32::MAX,
                (f64::from(height) * ratio).round().max(1.0) as u32,
            )
        } else if nh > u64::from(u32::MAX) {
            let ratio = f64::from(u32::MAX) / f64::from(height);
            ((f64::from(width) * ratio).round().max(1.0) as u32, u32::MAX)
        } else {
            (nw as u32, nh as u32)
        }
    }

    /// Maps this service's existing filter heuristic (Triangle for
    /// thumbnails, Lanczos3 otherwise - see the call site above) onto its
    /// `fast_image_resize` equivalent (#63 stage 1). `Triangle` ->
    /// `Bilinear` is a naming difference, not a quality one: both are the
    /// same linear/tent kernel, just named after the interpolation
    /// (`fast_image_resize`) rather than the kernel shape (`image`).
    /// `Lanczos3` matches by name exactly and is `fast_image_resize`'s own
    /// default algorithm. `Nearest`/`CatmullRom`/`Gaussian` are mapped for
    /// completeness even though the heuristic above never selects them
    /// today, so this stays a total function instead of needing a
    /// `_ => unreachable!()` a future heuristic change could silently
    /// falsify.
    fn fir_resize_alg(filter: FilterType) -> fir::ResizeAlg {
        match filter {
            FilterType::Nearest => fir::ResizeAlg::Nearest,
            FilterType::Triangle => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
            FilterType::CatmullRom => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
            FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
            FilterType::Lanczos3 => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
        }
    }

    /// Resizes `img` to exactly `nwidth x nheight` via `fast_image_resize`
    /// (SIMD-accelerated) instead of the `image` crate's own scalar
    /// resampler - the #63 stage-1 change, targeting the single most
    /// expensive stage in the committed benchmark baseline (17.39ms
    /// lanczos3 downscale vs 6.78ms JPEG decode).
    ///
    /// `fast_image_resize::Resizer::resize` writes directly into a
    /// `DynamicImage` destination - it implements `IntoImageViewMut` for
    /// every colour-type variant this service ever produces (the `image`
    /// cargo feature on `fast_image_resize`) - so no manual pixel-buffer
    /// reinterpretation is needed in either direction; the destination is
    /// allocated via `DynamicImage::new` with `img.color()`, so it is
    /// always the same colour-type variant as the source.
    ///
    /// `ResizeOptions::mul_div_alpha` defaults to `true`, so a source with
    /// an alpha channel (`Rgba8`/`LumaA8`/...) is premultiplied before
    /// resampling and un-premultiplied after, internally - this is what
    /// keeps #34/#60's alpha handling from regressing into
    /// fringing/halos at transparent edges (verified empirically with
    /// DSSIM against the `alpha_fringe_rgba` fixture, not just asserted
    /// from reading the option's default).
    fn fir_resize_exact(
        img: &DynamicImage,
        nwidth: u32,
        nheight: u32,
        filter: FilterType,
    ) -> Result<DynamicImage> {
        // `fast_image_resize` rejects a zero-sized destination outright.
        // Floor at 1px/axis - the same floor `resize_dimensions` above
        // already applies to every *computed* fit/fill target - so a
        // `Force` request naming 0 explicitly still produces a
        // (degenerate) image instead of a hard error, matching the
        // pre-existing `image`-crate behaviour for that same edge case.
        let nwidth = nwidth.max(1);
        let nheight = nheight.max(1);

        if (nwidth, nheight) == img.dimensions() {
            // Mirrors `image::imageops::resize`'s own no-op short-circuit
            // (image-0.25.10 `src/imageops/sample.rs`): nothing to
            // resample, so skip the round trip through
            // `fast_image_resize` entirely.
            return Ok(img.clone());
        }

        let mut dst = DynamicImage::new(nwidth, nheight, img.color());
        let mut resizer = fir::Resizer::new();
        let options = fir::ResizeOptions::new().resize_alg(Self::fir_resize_alg(filter));
        resizer
            .resize(img, &mut dst, &options)
            .map_err(|e| anyhow::anyhow!("fast_image_resize failed for {nwidth}x{nheight}: {e}"))?;
        Ok(dst)
    }

    /// `fast_image_resize`-backed equivalent of `DynamicImage::resize`
    /// (fit inside `nwidth x nheight`, preserving aspect ratio) - same
    /// `resize_dimensions(..., fill: false)` target-size math as before,
    /// only the resampling kernel itself is swapped (#63 stage 1).
    fn fir_resize(
        img: &DynamicImage,
        nwidth: u32,
        nheight: u32,
        filter: FilterType,
    ) -> Result<DynamicImage> {
        if (nwidth, nheight) == img.dimensions() {
            return Ok(img.clone());
        }
        let (width2, height2) =
            Self::resize_dimensions(img.width(), img.height(), nwidth, nheight, false);
        Self::fir_resize_exact(img, width2, height2, filter)
    }

    /// `fast_image_resize`-backed equivalent of
    /// `DynamicImage::resize_to_fill` (cover `nwidth x nheight`, preserving
    /// aspect ratio, then crop the overflow) - originally always
    /// centre-cropped (image-0.25.10 `src/images/dynimage.rs:943-962`,
    /// #63 stage 1 ported that exact arithmetic byte-for-byte). #50 replaces
    /// the hardcoded centre with `gravity`-anchored cropping via
    /// `Self::gravity_anchor`: `Gravity::Center` reproduces the exact same
    /// offsets `resize_to_fill` always used (see `gravity_anchor`'s doc
    /// comment for why), so this is a strict generalisation, not a
    /// behaviour change for the pre-#50 default.
    fn fir_resize_to_fill(
        img: &DynamicImage,
        nwidth: u32,
        nheight: u32,
        filter: FilterType,
        gravity: Gravity,
    ) -> Result<DynamicImage> {
        let (width2, height2) =
            Self::resize_dimensions(img.width(), img.height(), nwidth, nheight, true);
        let intermediate = Self::fir_resize_exact(img, width2, height2, filter)?;
        let (iwidth, iheight) = intermediate.dimensions();
        let (x, y) = Self::gravity_anchor(gravity, iwidth, iheight, nwidth, nheight);
        Ok(intermediate.crop_imm(x, y, nwidth, nheight))
    }

    /// Resolves `gravity` into a top-left `(x, y)` pixel offset for a
    /// `box_w x box_h` crop region inside a `container_w x container_h`
    /// container (#50) - shared by `fir_resize_to_fill`'s cover-crop and
    /// `apply_crop`'s explicit `c:` crop, since both are "anchor a smaller
    /// box inside a larger one" problems with identical gravity semantics.
    ///
    /// The directional/corner/`Center` variants place the box against the
    /// named edge/corner of the container, with `Center` splitting the
    /// leftover space evenly on both axes via floor division - this
    /// reproduces `resize_to_fill`'s original always-centred arithmetic
    /// (`(container - box) / 2` on each axis, integer/floor division)
    /// exactly, byte-for-byte, for the pre-#50 default gravity.
    ///
    /// `FocusPoint { x, y }` is different in kind: `x`/`y` (each in
    /// `[0, 1]`, clamped defensively even though the URL parser already
    /// range-checks them) name a point *within the container* - `(0, 0)` is
    /// the top-left corner, `(1, 1)` the bottom-right - and the box is
    /// centred on that point, then clamped so it never crosses a container
    /// edge. Because `Fill`'s cover-resize scales both axes by the same
    /// factor, a focus point expressed as a fraction of the *source* image
    /// lands on the same fractional position in the resized intermediate,
    /// so callers can pass the same `Gravity::FocusPoint` straight through
    /// from a `ResizeQuery` without re-deriving it per container.
    ///
    /// `box_w`/`box_h` are clamped to the container's own size first, so a
    /// pathological call with a box larger than its container can't
    /// underflow the `container - box` subtraction.
    fn gravity_anchor(
        gravity: Gravity,
        container_w: u32,
        container_h: u32,
        box_w: u32,
        box_h: u32,
    ) -> (u32, u32) {
        let box_w = box_w.min(container_w);
        let box_h = box_h.min(container_h);
        let max_x = f64::from(container_w - box_w);
        let max_y = f64::from(container_h - box_h);

        let (x, y) = match gravity {
            Gravity::Center => (max_x / 2.0, max_y / 2.0),
            Gravity::North => (max_x / 2.0, 0.0),
            Gravity::South => (max_x / 2.0, max_y),
            Gravity::West => (0.0, max_y / 2.0),
            Gravity::East => (max_x, max_y / 2.0),
            Gravity::NorthWest => (0.0, 0.0),
            Gravity::NorthEast => (max_x, 0.0),
            Gravity::SouthWest => (0.0, max_y),
            Gravity::SouthEast => (max_x, max_y),
            Gravity::FocusPoint { x, y } => {
                let center_x = f64::from(container_w) * x.clamp(0.0, 1.0);
                let center_y = f64::from(container_h) * y.clamp(0.0, 1.0);
                (
                    (center_x - f64::from(box_w) / 2.0).clamp(0.0, max_x),
                    (center_y - f64::from(box_h) / 2.0).clamp(0.0, max_y),
                )
            }
        };

        (x.floor() as u32, y.floor() as u32)
    }

    /// Applies an explicit `c:` crop (#50) to the decoded source image,
    /// before any resize math runs - see the call site's comment and
    /// [`crate::models::params::Crop`]'s doc comment for the ordering
    /// rationale. Resolves `crop.width`/`crop.height` against the source's
    /// actual dimensions (`Full`/`Absolute`/`Relative`, see
    /// [`CropDimension`]), clamps the resolved box to at least 1px and at
    /// most the source's own size per axis (a `Relative` fraction is
    /// already `<= 1` by construction, but an `Absolute` value from the URL
    /// is caller-controlled and could exceed the source), then anchors it
    /// with `Self::gravity_anchor`.
    fn apply_crop(img: &DynamicImage, crop: &Crop) -> DynamicImage {
        let (src_width, src_height) = img.dimensions();

        let resolve = |dim: CropDimension, source: u32| -> u32 {
            let resolved = match dim {
                CropDimension::Full => source,
                CropDimension::Absolute(v) => v,
                CropDimension::Relative(fraction) => (f64::from(source) * fraction).round() as u32,
            };
            resolved.clamp(1, source.max(1))
        };

        let crop_width = resolve(crop.width, src_width);
        let crop_height = resolve(crop.height, src_height);
        let (x, y) =
            Self::gravity_anchor(crop.gravity, src_width, src_height, crop_width, crop_height);

        img.crop_imm(x, y, crop_width, crop_height)
    }

    /// Composites `img` onto an opaque `background` colour, producing a
    /// plain RGB image with no alpha channel at all (#34). Only meaningful
    /// as a pre-encode step for a format with no alpha channel (JPEG) - see
    /// the call site's comment for why this exists instead of letting the
    /// encoder itself drop the channel.
    ///
    /// Standard "over" alpha compositing per channel:
    /// `out = src * alpha + background * (1 - alpha)`, computed in floating
    /// point and rounded (not truncated) so a fully-opaque source pixel
    /// (`alpha == 255`) round-trips to itself exactly, and the `alpha == 0`
    /// case is special-cased to exactly `background` rather than relying on
    /// the general formula to reduce to it (it does, but the special case
    /// avoids float rounding surprises on the boundary the golden-image
    /// tests assert pixel-exact equality against).
    fn flatten_onto_background(img: &DynamicImage, background: [u8; 3]) -> DynamicImage {
        let rgba = img.to_rgba8();
        let mut out = image::RgbImage::new(rgba.width(), rgba.height());

        for (src, dst) in rgba.pixels().zip(out.pixels_mut()) {
            let [r, g, b, a] = src.0;
            *dst = image::Rgb(match a {
                255 => [r, g, b],
                0 => background,
                _ => {
                    let alpha = f32::from(a) / 255.0;
                    let blend = |c: u8, bg: u8| -> u8 {
                        (f32::from(c) * alpha + f32::from(bg) * (1.0 - alpha)).round() as u8
                    };
                    [
                        blend(r, background[0]),
                        blend(g, background[1]),
                        blend(b, background[2]),
                    ]
                }
            });
        }

        DynamicImage::ImageRgb8(out)
    }

    /// Rewrites the RGB of every fully-transparent (`alpha == 0`) pixel to
    /// `background`, leaving every other pixel - including partially
    /// transparent ones - byte-for-byte untouched (#60).
    ///
    /// Safe by construction: a pixel with `alpha == 0` is invisible
    /// regardless of its RGB, so this cannot change what any viewer sees -
    /// only what bytes the encoder has to spend compressing an invisible
    /// region (a solid-colour region compresses far better than whatever
    /// noise the source had there). Partial transparency is deliberately
    /// left alone: its RGB is visible (blended with whatever ends up
    /// behind it), so rewriting it would be a real, lossy visual change,
    /// not the lossless one this is meant to be.
    fn normalize_transparent_pixels(
        mut rgba: image::RgbaImage,
        background: [u8; 3],
    ) -> image::RgbaImage {
        for pixel in rgba.pixels_mut() {
            if pixel.0[3] == 0 {
                pixel.0[0] = background[0];
                pixel.0[1] = background[1];
                pixel.0[2] = background[2];
            }
        }
        rgba
    }

    /// Composites a watermark onto `base` (#52): decodes `watermark_bytes`,
    /// applies `wm`'s size/scale, rotation and shadow, then alpha-blends it
    /// over `base` at the resolved position with `wm.opacity` honoured.
    ///
    /// Order within this function mirrors imgproxy's own documented
    /// pipeline: resize/scale the watermark first, *then* rotate it (a
    /// rotated image's own bounding box is what gets positioned), draw the
    /// shadow (if any) at the same position as the final watermark, then
    /// composite the watermark itself on top.
    fn apply_watermark(
        base: DynamicImage,
        watermark_bytes: &[u8],
        wm: &WatermarkQuery,
    ) -> Result<DynamicImage> {
        let watermark_img =
            image::load_from_memory(watermark_bytes).context("Failed to decode watermark image")?;

        let (base_width, base_height) = base.dimensions();

        // Size/scale (`wms:`/`wm:`'s scale slot). imgproxy always resizes
        // watermarks with `fit` semantics (preserving the watermark's own
        // aspect ratio, enlarging when needed) - never a stretch - so every
        // branch below goes through `Self::fir_resize` (fit), never
        // `fir_resize_exact`.
        let watermark_img = if let Some((w, h)) = wm.size.filter(|(w, h)| *w > 0 || *h > 0) {
            let (target_w, target_h) = match (w, h) {
                (0, h) => (u32::MAX, h),
                (w, 0) => (w, u32::MAX),
                (w, h) => (w, h),
            };
            Self::fir_resize(&watermark_img, target_w, target_h, FilterType::Lanczos3)?
        } else if wm.scale > 0.0 {
            let target_w = ((base_width as f64) * f64::from(wm.scale)).round().max(1.0) as u32;
            let target_h = ((base_height as f64) * f64::from(wm.scale)).round().max(1.0) as u32;
            Self::fir_resize(&watermark_img, target_w, target_h, FilterType::Lanczos3)?
        } else {
            watermark_img
        };

        let mut watermark_rgba = watermark_img.to_rgba8();

        // Rotation (`wmr:`, clockwise degrees).
        if wm.rotate != 0.0 {
            watermark_rgba = Self::rotate_rgba(&watermark_rgba, wm.rotate);
        }

        let (watermark_width, watermark_height) = watermark_rgba.dimensions();
        let (x, y) = Self::watermark_position(
            wm.position,
            wm.x_offset,
            wm.y_offset,
            base_width,
            base_height,
            watermark_width,
            watermark_height,
        );

        let mut canvas = base.to_rgba8();
        let opacity = wm.opacity.clamp(0.0, 1.0);

        // Shadow (`wmsh:`), drawn first so the watermark composites on top
        // of it, at the same position.
        if let Some(sigma) = wm.shadow.filter(|s| *s > 0.0) {
            let shadow = Self::build_shadow_layer(&watermark_rgba, sigma);
            Self::composite_over(&mut canvas, &shadow, x, y, 1.0);
        }

        Self::composite_over(&mut canvas, &watermark_rgba, x, y, opacity);

        Ok(DynamicImage::ImageRgba8(canvas))
    }

    /// Resolves the top-left `(x, y)` pixel coordinate (relative to the
    /// base image's own origin, may be negative or extend past the base
    /// image's far edge - `composite_over` clips) at which a
    /// `watermark_width x watermark_height` watermark should be drawn,
    /// given `position`'s anchor and the `x_offset`/`y_offset` modifiers
    /// (#52).
    ///
    /// Offset convention (imgproxy's own): a magnitude `>= 1.0` is treated
    /// as an absolute pixel count; anything smaller is a fraction of the
    /// corresponding base image dimension. Positive `x_offset` moves the
    /// watermark right, positive `y_offset` moves it down, regardless of
    /// which anchor `position` is - i.e. offsets always nudge in the same
    /// screen-space direction rather than "further from the anchor".
    fn watermark_position(
        position: WatermarkPosition,
        x_offset: f32,
        y_offset: f32,
        base_width: u32,
        base_height: u32,
        watermark_width: u32,
        watermark_height: u32,
    ) -> (i64, i64) {
        let resolve_offset = |offset: f32, dimension: u32| -> i64 {
            if offset.abs() >= 1.0 {
                offset.round() as i64
            } else {
                (f64::from(offset) * f64::from(dimension)).round() as i64
            }
        };

        let dx = resolve_offset(x_offset, base_width);
        let dy = resolve_offset(y_offset, base_height);

        let bw = i64::from(base_width);
        let bh = i64::from(base_height);
        let ww = i64::from(watermark_width);
        let wh = i64::from(watermark_height);

        let (base_x, base_y) = match position {
            WatermarkPosition::Center => ((bw - ww) / 2, (bh - wh) / 2),
            WatermarkPosition::North => ((bw - ww) / 2, 0),
            WatermarkPosition::South => ((bw - ww) / 2, bh - wh),
            WatermarkPosition::East => (bw - ww, (bh - wh) / 2),
            WatermarkPosition::West => (0, (bh - wh) / 2),
            WatermarkPosition::NorthEast => (bw - ww, 0),
            WatermarkPosition::NorthWest => (0, 0),
            WatermarkPosition::SouthEast => (bw - ww, bh - wh),
            WatermarkPosition::SouthWest => (0, bh - wh),
        };

        (base_x + dx, base_y + dy)
    }

    /// Alpha-composites `overlay` onto `canvas` at top-left `(x, y)`
    /// (`canvas`'s coordinate space; may be negative or extend past its far
    /// edge - pixels outside `canvas`'s bounds are silently clipped),
    /// scaling `overlay`'s own per-pixel alpha by `opacity` first.
    ///
    /// Standard Porter-Duff "source over", in straight (non-premultiplied)
    /// alpha: `out_a = src_a + dst_a * (1 - src_a)`,
    /// `out_rgb = (src_rgb * src_a + dst_rgb * dst_a * (1 - src_a)) / out_a`.
    /// Used for both the watermark itself (`opacity` = `wm.opacity`,
    /// clamped to `[0, 1]`) and its shadow layer (`opacity = 1.0`, since
    /// the shadow's own alpha - already scaled by the blur - is the only
    /// opacity control it needs).
    fn composite_over(
        canvas: &mut image::RgbaImage,
        overlay: &image::RgbaImage,
        x: i64,
        y: i64,
        opacity: f32,
    ) {
        if opacity <= 0.0 {
            return;
        }

        let (canvas_width, canvas_height) = canvas.dimensions();
        let (overlay_width, overlay_height) = overlay.dimensions();

        for overlay_y in 0..overlay_height {
            let canvas_y = y + i64::from(overlay_y);
            if canvas_y < 0 || canvas_y >= i64::from(canvas_height) {
                continue;
            }
            for overlay_x in 0..overlay_width {
                let canvas_x = x + i64::from(overlay_x);
                if canvas_x < 0 || canvas_x >= i64::from(canvas_width) {
                    continue;
                }

                let src = overlay.get_pixel(overlay_x, overlay_y).0;
                let src_a = (f32::from(src[3]) / 255.0) * opacity;
                if src_a <= 0.0 {
                    continue;
                }

                let dst = canvas.get_pixel_mut(canvas_x as u32, canvas_y as u32);
                let dst_a = f32::from(dst[3]) / 255.0;
                let out_a = src_a + dst_a * (1.0 - src_a);

                if out_a <= 0.0 {
                    *dst = image::Rgba([0, 0, 0, 0]);
                    continue;
                }

                let blend = |s: u8, d: u8| -> u8 {
                    (((f32::from(s) * src_a) + (f32::from(d) * dst_a * (1.0 - src_a))) / out_a)
                        .round()
                        .clamp(0.0, 255.0) as u8
                };

                *dst = image::Rgba([
                    blend(src[0], dst[0]),
                    blend(src[1], dst[1]),
                    blend(src[2], dst[2]),
                    (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                ]);
            }
        }
    }

    /// Rotates `img` clockwise by `degrees` around its own centre (#52,
    /// `wmr:`), expanding the canvas to the rotated bounding box and
    /// filling every pixel the rotated source doesn't cover with fully
    /// transparent (`alpha = 0`). Nearest-neighbour sampling (nothing in
    /// this crate's existing resize/rotate code averages source pixels for
    /// anything other than downscaling, and a watermark is typically small
    /// enough that this is not a visible quality gap).
    ///
    /// Pixel *centres* (not corners) are used as the sampling coordinate
    /// space throughout, so a destination pixel maps back to the source
    /// pixel that actually covers its centre point rather than being off
    /// by half a pixel at every angle.
    fn rotate_rgba(img: &image::RgbaImage, degrees: f32) -> image::RgbaImage {
        let (width, height) = img.dimensions();
        if width == 0 || height == 0 {
            return img.clone();
        }

        let normalized_degrees = f64::from(degrees).rem_euclid(360.0);
        if normalized_degrees == 0.0 {
            return img.clone();
        }

        let theta = normalized_degrees.to_radians();
        let (sin_t, cos_t) = theta.sin_cos();

        let (fw, fh) = (f64::from(width), f64::from(height));
        let (cx, cy) = (fw / 2.0, fh / 2.0);

        // Bounding box of the rotated source, via its four corners -
        // forward rotation `R(theta) = [[cos, -sin], [sin, cos]]`.
        let corners = [(0.0, 0.0), (fw, 0.0), (0.0, fh), (fw, fh)];
        let mut min_x = f64::MAX;
        let mut max_x = f64::MIN;
        let mut min_y = f64::MAX;
        let mut max_y = f64::MIN;
        for (px, py) in corners {
            let (dx, dy) = (px - cx, py - cy);
            let rx = dx * cos_t - dy * sin_t;
            let ry = dx * sin_t + dy * cos_t;
            min_x = min_x.min(rx);
            max_x = max_x.max(rx);
            min_y = min_y.min(ry);
            max_y = max_y.max(ry);
        }

        // A tiny epsilon before `ceil` absorbs floating-point noise at
        // exact multiples of 90 degrees (`sin`/`cos` of a value near a
        // multiple of pi are not exactly 0/1 in f64), where the true
        // bounding-box width/height is an exact integer but the computed
        // value lands a few ULPs above it (e.g. `5.0000000000000009`) -
        // without this, `ceil` would round that up to 6 instead of 5.
        const EPSILON: f64 = 1e-9;
        let new_width = (max_x - min_x - EPSILON).ceil().max(1.0) as u32;
        let new_height = (max_y - min_y - EPSILON).ceil().max(1.0) as u32;
        let (new_cx, new_cy) = (f64::from(new_width) / 2.0, f64::from(new_height) / 2.0);

        let mut out = image::RgbaImage::new(new_width, new_height);
        // Every pixel starts fully transparent (`RgbaImage::new` zero-fills),
        // so a destination pixel whose inverse-mapped source coordinate
        // falls outside `img`'s bounds is simply left untouched below.
        for out_y in 0..new_height {
            for out_x in 0..new_width {
                // Inverse rotation (dest -> src), `R(-theta) = R(theta)^T`:
                // `src = R(theta)^T * (dst - new_centre) + centre`.
                let dx = (f64::from(out_x) + 0.5) - new_cx;
                let dy = (f64::from(out_y) + 0.5) - new_cy;
                let src_x = dx * cos_t + dy * sin_t + cx;
                let src_y = -dx * sin_t + dy * cos_t + cy;

                let src_ix = src_x.floor();
                let src_iy = src_y.floor();
                if src_ix >= 0.0 && src_iy >= 0.0 && src_ix < fw && src_iy < fh {
                    let pixel = img.get_pixel(src_ix as u32, src_iy as u32);
                    out.put_pixel(out_x, out_y, *pixel);
                }
            }
        }

        out
    }

    /// Builds a soft drop-shadow layer (#52, `wmsh:`) the same size as
    /// `watermark`: a black silhouette of `watermark`'s own alpha channel,
    /// Gaussian-blurred by `sigma`. Composited at the same position as
    /// `watermark` itself (by the caller), so the blur naturally spreads a
    /// soft dark halo just past the watermark's own opaque edges.
    fn build_shadow_layer(watermark: &image::RgbaImage, sigma: f32) -> image::RgbaImage {
        let (width, height) = watermark.dimensions();
        let mut silhouette = image::RgbaImage::new(width, height);
        for (src, dst) in watermark.pixels().zip(silhouette.pixels_mut()) {
            *dst = image::Rgba([0, 0, 0, src.0[3]]);
        }
        DynamicImage::ImageRgba8(silhouette).blur(sigma).to_rgba8()
    }

    /// Encodes `img` to WebP via the `webp` crate directly (libwebp
    /// bindings), rather than through `DynamicImage::write_to` - the
    /// `image` crate's own WebP encoder is lossless-only, which is exactly
    /// the bug this exists to fix. `quality` (0.0-100.0) is used only when
    /// `lossless` is `false`.
    ///
    /// `pub` (rather than private) for two reasons: so `benches/encode.rs`
    /// can benchmark the exact lossy path production uses instead of
    /// duplicating it against the raw `image` crate encoder, and so a real
    /// `quality`/`lossless` field added to the request-parameter surface
    /// later can be threaded straight through without changing this
    /// function's signature.
    ///
    /// Normalizes `img` to `Rgba8` before handing it to
    /// `webp::Encoder::from_rgba` rather than using
    /// `webp::Encoder::from_image` directly: `from_image` only supports the
    /// `Rgb8`/`Rgba8` `DynamicImage` variants and returns `Err` for
    /// everything else, including `Luma8`/`LumaA8` - exactly what
    /// `DynamicImage::grayscale()` (the `params.grayscale` filter applied
    /// above) produces. Normalizing up front means a `grayscale=true` WebP
    /// request encodes correctly instead of failing.
    pub fn encode_webp(img: &DynamicImage, quality: f32, lossless: bool) -> Result<Vec<u8>> {
        let rgba = img.to_rgba8();
        let encoder = webp::Encoder::from_rgba(rgba.as_raw(), rgba.width(), rgba.height());

        let memory = if lossless {
            encoder.encode_lossless()
        } else {
            encoder.encode(quality)
        };

        Ok(memory.to_vec())
    }

    /// Encodes `img` to PNG via an explicit `PngEncoder::new_with_quality`
    /// rather than `DynamicImage::write_to`'s default `CompressionType`
    /// (`Fast` - `image-0.25.10/src/codecs/png.rs`'s
    /// `CompressionType::default()`): `CompressionType::Best` +
    /// `FilterType::Adaptive` instead, a real, dependency-free size win for
    /// #60 that doesn't touch `Cargo.toml` - `PngEncoder` and its
    /// `CompressionType`/`FilterType` enums are already part of the `image`
    /// crate's own public API under the `png` feature this crate already
    /// depends on, not a new dependency. PNG has no quality knob in
    /// `ResizeQuery` to honour - `CompressionType` is a fixed lossless
    /// setting, not a continuous 0-100 scale, and `fq:png:N` is rejected at
    /// parse time (`src/modules/url/options.rs`) rather than silently
    /// accepted and ignored here.
    ///
    /// `icc_profile`/`exif_metadata` are both best-effort: `set_icc_profile`/
    /// `set_exif_metadata` only fail for an encoder that doesn't support
    /// them at all, never for `PngEncoder` itself, so a failure here is
    /// silently ignored rather than turned into a hard request error - same
    /// spirit as `encode_jpeg`'s `write_icc_profile` call below.
    ///
    /// `pub` (like `encode_webp` above) so `benches/encode.rs` can benchmark
    /// the exact path production uses, instead of duplicating the
    /// `CompressionType::Best`/`FilterType::Adaptive` settings by hand - that
    /// duplication is exactly what let the bench drift to `write_to`'s
    /// default `Fast` compression for months, recording every historical
    /// `encode/png` number roughly 56x too fast (see `benches/encode.rs`'s
    /// own module doc comment for the discovery and fix).
    pub fn encode_png(
        img: &DynamicImage,
        icc_profile: Option<&[u8]>,
        exif_metadata: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let estimated_size = Self::estimate_output_size(img, &ImageFormat::Png);
        let mut buf = Cursor::new(Vec::with_capacity(estimated_size));

        let mut encoder = image::codecs::png::PngEncoder::new_with_quality(
            &mut buf,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        );
        if let Some(icc) = icc_profile {
            // `PngEncoder::set_icc_profile` only fails for genuinely
            // unsupported encoders, never for `PngEncoder` itself - ignore
            // failure rather than turn a best-effort colour-fidelity
            // improvement into a hard request error.
            let _ = encoder.set_icc_profile(icc.to_vec());
        }
        if let Some(exif) = exif_metadata {
            // #5: same best-effort spirit as `set_icc_profile` just above -
            // `PngEncoder::set_exif_metadata` only fails for an encoder that
            // doesn't support it at all, never for `PngEncoder` itself.
            let _ = encoder.set_exif_metadata(exif.to_vec());
        }
        img.write_with_encoder(encoder)
            .context("Failed to encode image to Png")?;

        Ok(buf.into_inner())
    }

    /// Encodes `img` to JPEG via `mozjpeg::Compress`/libjpeg-turbo instead
    /// of `image::codecs::jpeg::JpegEncoder` (#76). The `image` crate's own
    /// JPEG encoder has no progressive-mode switch and hardcodes 4:2:2
    /// chroma subsampling - verified against its actual public API
    /// (`image-0.25.10/src/codecs/jpeg/encoder.rs`: exactly `new`,
    /// `new_with_quality`, `set_pixel_density`, `encode`, `encode_image`,
    /// no subsampling or progressive-mode knob at all) - so neither
    /// `progressive` nor `no_subsampling` below could be threaded through
    /// it. `mozjpeg` was already a dependency (added for DCT-scaled decode,
    /// #63 stage 2, see `mozjpeg_decode` below) and its `Compress` type
    /// exposes both directly, so this is a matter of routing JPEG *encode*
    /// through the same crate rather than adding a new dependency.
    ///
    /// `no_subsampling: false` (the default, imgproxy's `jpgo:` `no_subsample`
    /// slot left unset) reproduces 4:2:2 -
    /// `set_chroma_sampling_pixel_sizes((2, 1), (2, 1))`, matching exactly
    /// what `image`'s encoder always did - so a request that never touches
    /// `jpgo:` gets byte-shape-equivalent chroma handling to before #76,
    /// satisfying this issue's own "existing behaviour must not change
    /// when unset" requirement. `no_subsampling: true` selects 4:4:4
    /// (`(1, 1), (1, 1)`) - full chroma resolution, imgproxy's
    /// `IMGPROXY_JPEG_NO_SUBSAMPLING`.
    ///
    /// `pub` (like `encode_webp` above) so `benches/encode.rs` can
    /// benchmark the exact path production uses.
    ///
    /// Wrapped in `catch_unwind`, same reasoning as `mozjpeg_decode` below:
    /// mozjpeg's error manager unwinds (panics) on a libjpeg-level error
    /// rather than returning one, so a real encode failure must be caught
    /// here instead of taking the whole worker thread down. `AssertUnwindSafe`
    /// is sound for the same reason it is in `mozjpeg_decode`: every
    /// captured value (`rgb`, `icc_owned`, `exif_owned`, the `Copy` scalars)
    /// is either freshly-owned local data or `Copy`, none of it shared/
    /// interior-mutable state that could be left torn by an unwind.
    ///
    /// `exif_metadata` (#5) is written the same way `icc_profile` already is.
    /// See `encode_jpeg_inner`'s own comment at the `write_marker` call for
    /// why mozjpeg needs the raw `APP1` bytes built by hand, unlike PNG/AVIF
    /// which have a dedicated `ImageEncoder::set_exif_metadata`.
    pub fn encode_jpeg(
        img: &DynamicImage,
        quality: u8,
        progressive: bool,
        no_subsampling: bool,
        icc_profile: Option<&[u8]>,
        exif_metadata: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let rgb = img.to_rgb8();
        let icc_owned = icc_profile.map(<[u8]>::to_vec);
        let exif_owned = exif_metadata.map(<[u8]>::to_vec);

        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::encode_jpeg_inner(
                &rgb,
                quality,
                progressive,
                no_subsampling,
                icc_owned.as_deref(),
                exif_owned.as_deref(),
            )
        }))
        .unwrap_or_else(|payload| {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "mozjpeg panicked with a non-string payload".to_string());
            Err(anyhow::anyhow!("mozjpeg encode panicked: {msg}"))
        })
    }

    fn encode_jpeg_inner(
        rgb: &RgbImage,
        quality: u8,
        progressive: bool,
        no_subsampling: bool,
        icc_profile: Option<&[u8]>,
        exif_metadata: Option<&[u8]>,
    ) -> Result<Vec<u8>> {
        let (width, height) = rgb.dimensions();

        let mut compress = mozjpeg::Compress::new(mozjpeg::ColorSpace::JCS_RGB);
        compress.set_size(width as usize, height as usize);

        // Non-progressive path only: drop to mozjpeg's `JCP_FASTEST`
        // profile (`set_fastest_defaults`, "reset to libjpeg v6 settings ...
        // identical with libjpeg-turbo") before anything else is
        // configured, so the size/quality-affecting calls below apply on
        // top of it rather than being clobbered by it - `set_fastest_defaults`
        // re-runs `jpeg_set_defaults` internally, which resets quality,
        // colour-space-derived sampling factors, and everything else this
        // function sets afterward (`mozjpeg-sys-2.2.3/vendor/jcparam.c`'s
        // `jpeg_set_defaults`).
        //
        // Why: mozjpeg's *default* profile from `Compress::new` is
        // `JCP_MAX_COMPRESSION`, which turns on trellis quantisation
        // (`master->trellis_quant = true`) unconditionally at
        // `jpeg_set_defaults` time - regardless of the progressive/
        // `set_optimize_scans` setting below, since that's a separate,
        // later, dynamic bool param that never touches `trellis_quant`
        // (confirmed by reading `jcparam.c` directly: `trellis_quant =
        // (compress_profile == JCP_MAX_COMPRESSION)`, set once, no public
        // `mozjpeg` crate API to toggle it independently - the only two
        // profiles the crate exposes are `JCP_MAX_COMPRESSION` and
        // `JCP_FASTEST`). Trellis is real CPU, not free: `encode/jpeg_baseline`
        // measured 9.61ms with it on vs 3.07ms for the old `image`-crate
        // encoder it replaced (#76's own regression, ~3x, would fail this
        // repo's 15% bench gate as-is).
        //
        // Measured on the Kodak True Color corpus (24 real photos, same
        // corpus/DSSIM method as `adr/0003-webp-measurement.md`) whether
        // that CPU actually buys smaller files at *matched* DSSIM (not
        // matched nominal quality number - see that ADR for why nominal
        // comparisons are invalid here): current `JCP_MAX_COMPRESSION`
        // default is ~12% smaller than the old `image`-crate encoder
        // (median ratio 0.881) but costs 3-8x its encode time. Dropping to
        // `JCP_FASTEST` (this branch) is still ~5% smaller than the old
        // `image`-crate encoder (median ratio 0.946) while being 3-4x
        // *faster* than that encoder too (mozjpeg/libjpeg-turbo's SIMD C
        // beats `image`'s pure-Rust baseline encoder even before mozjpeg's
        // own extensions enter the picture) - a strict win on both axes
        // over the pre-#76 baseline, and at the *same* nominal quality
        // number `JCP_FASTEST` even scores a lower (better) mean DSSIM
        // than the current `JCP_MAX_COMPRESSION` default (0.001787 vs
        // 0.002236 at quality 75) - trellis trades some visual fidelity
        // for extra size reduction at a fixed quality number, so removing
        // it does not make default output worse, only smaller-but-less-so.
        // Full measurement table in this change's own report.
        //
        // Progressive output (explicit `jpgo:progressive` request) keeps
        // the full `JCP_MAX_COMPRESSION` profile below - progressive is
        // already an opt-in, pay-for-what-you-use path, so its extra cost
        // is the caller's choice, not a default everyone pays.
        if !progressive {
            compress.set_fastest_defaults();
        }

        // 4:2:2 == `(2, 1)` for both Cb and Cr - this crate's pre-#76
        // default, matching `image::codecs::jpeg::JpegEncoder`'s hardcoded
        // subsampling exactly (see this function's own doc comment above).
        // 4:4:4 == `(1, 1)` - full chroma resolution. Set before
        // `set_progressive_mode` below so mozjpeg's default progressive
        // scan script (`jpeg_simple_progression`) is built against the
        // sampling factors actually in effect, not libjpeg's own defaults.
        let subsampling_px_size = if no_subsampling { (1, 1) } else { (2, 1) };
        compress.set_chroma_sampling_pixel_sizes(subsampling_px_size, subsampling_px_size);

        compress.set_quality(f32::from(quality));

        // mozjpeg's own default (`Compress::new`'s `jpeg_set_defaults`,
        // under its default `JCP_MAX_COMPRESSION` compress profile)
        // already builds and installs a progressive scan script - real
        // MozJPEG's whole "smaller by default" premise, confirmed against
        // `mozjpeg-sys-2.2.3/vendor/jcparam.c`'s `jpeg_set_defaults`: it
        // sets `master->optimize_scans = TRUE` and calls
        // `jpeg_simple_progression(cinfo)` unconditionally at construction
        // time. So an *unset* `progressive` here must actively opt back
        // *out* of that default to reproduce this crate's pre-#76 baseline
        // (non-progressive) output - it is not, as `set_progressive_mode`'s
        // own "you can only turn it on" doc comment might suggest, already
        // the starting point. `set_optimize_scans(false)` is what
        // `jcmaster.c`'s `jinit_c_master_control` actually keys off of at
        // `start_compress` time (`cinfo->master->optimize_scans` forces
        // `progressive_mode = TRUE` in `validate_script` regardless of
        // `scan_info`'s contents) - and, on the `false` path, also nulls
        // `cinfo->scan_info`, which `jinit_c_master_control` separately
        // checks (`scan_info == NULL` -> `progressive_mode = FALSE,
        // num_scans = 1`) - so `set_optimize_scans(false)` is the one call
        // that actually forces real baseline sequential. Verified
        // empirically against this exact mozjpeg version: without it, a
        // "baseline" and a "progressive" encode of the same pixels at the
        // same quality produced byte-identical output (both already
        // progressive) - this is not merely a doc-derived belief.
        if progressive {
            compress.set_progressive_mode();
        } else {
            compress.set_optimize_scans(false);
        }

        let estimated_size = (width as usize).saturating_mul(height as usize) / 2;
        let mut started = compress
            .start_compress(Vec::with_capacity(estimated_size))
            .context("mozjpeg: failed to start compression")?;

        if let Some(icc) = icc_profile.filter(|icc| !icc.is_empty()) {
            // Same best-effort spirit as the PNG/pre-#76 JPEG branches'
            // `set_icc_profile` calls elsewhere in this file: a source ICC
            // profile is a colour-fidelity nicety, not something worth
            // failing the whole request over. `write_icc_profile` itself
            // has no fallible return (any failure surfaces as a libjpeg
            // error caught by this function's `catch_unwind` wrapper), so
            // there's nothing to ignore here beyond the empty-profile
            // guard above (`write_icc_profile` panics on empty input).
            started.write_icc_profile(icc);
        }

        if let Some(exif) = exif_metadata.filter(|exif| !exif.is_empty()) {
            // #5: mozjpeg has no dedicated EXIF API (unlike its
            // `write_icc_profile` for `APP2`/ICC just above), so the `APP1`
            // segment is built by hand: a 6-byte `"Exif\0\0"` marker
            // (JPEG/EXIF's own container convention, JEITA CP-3451) followed
            // by the raw TIFF-formatted bytes `ImageDecoder::exif_metadata`
            // returns (that trait's own doc comment: "the payload...
            // starting at the TIFF header", i.e. *without* the marker
            // prefix - confirmed empirically: `image::codecs::jpeg::encoder`'s
            // `write_exif` prepends the identical constant before handing
            // its own `self.exif` to `write_segment`, so this reproduces
            // that encoder's on-the-wire format exactly).
            //
            // Capped at the same `MAX_DATA_BYTES_IN_MARKER` mozjpeg itself
            // uses inside `write_icc_profile` (65533 total marker bytes,
            // minus this crate's own 6-byte prefix rather than
            // `write_icc_profile`'s 14-byte `ICC_PROFILE\0` + chunk-index
            // overhead) - unlike ICC, EXIF has no standard multi-segment
            // chunking convention to fall back on for an oversized blob, so
            // an over-limit blob is silently dropped (best-effort, same
            // spirit as the ICC branch above) rather than writing a
            // corrupt/truncated marker or panicking.
            const MAX_EXIF_MARKER_BYTES: usize = 65533 - b"Exif\0\0".len();
            if exif.len() <= MAX_EXIF_MARKER_BYTES {
                let mut app1 = Vec::with_capacity(6 + exif.len());
                app1.extend_from_slice(b"Exif\0\0");
                app1.extend_from_slice(exif);
                started.write_marker(mozjpeg::Marker::APP(1), &app1);
            }
        }

        started
            .write_scanlines(rgb.as_raw())
            .context("mozjpeg: failed to write scanlines")?;

        started
            .finish()
            .context("mozjpeg: failed to finish compression")
    }

    /// Rewrites the EXIF `Orientation` tag (0x0112) inside a raw,
    /// prefix-less TIFF-formatted EXIF blob (the shape
    /// `ImageDecoder::exif_metadata`/`ImageEncoder::set_exif_metadata` both
    /// use - no leading `"Exif\0\0"`, see `encode_jpeg_inner`'s comment at
    /// its `write_marker` call) to `1` (`Orientation::Normal`), in place.
    ///
    /// # Why this exists (#5)
    ///
    /// `params.autorotate` (on by default, #33) applies the source's EXIF
    /// `Orientation` tag to the *pixels* via `DynamicImage::apply_orientation`
    /// and the corrected `DynamicImage` carries no memory of the original
    /// tag afterward. If a caller also asks to *keep* metadata (`sm:0`), the
    /// *original*, now-stale `raw` EXIF blob read straight off the source
    /// decoder would still say "rotate me" - forwarding it unchanged would
    /// tell any EXIF-aware viewer (which auto-rotates on display, exactly
    /// like this crate just did) to rotate the *already-upright* pixels a
    /// second time. This function is what stops that: it finds the one tag
    /// responsible and overwrites its value to `1` (no transform), leaving
    /// every other field (GPS, camera make/model, timestamps - the actual
    /// content a caller asked to keep) byte-for-byte untouched.
    ///
    /// Only called when `encode_single_image` already knows autorotation
    /// actually changed the pixels (`exif_orientation_applied`) - see that
    /// function's own resolution step. When autorotate is off, the pixels
    /// were never touched, so the original tag is still an accurate
    /// instruction and this function is not called at all.
    ///
    /// # Deliberately conservative: fails closed
    ///
    /// This is a minimal, bounds-checked IFD0 walk - not a general TIFF
    /// parser - covering exactly the shape a well-formed Exif `Orientation`
    /// entry takes (TIFF 6.0 / Exif 2.3: tag `0x0112`, type `3`/SHORT, count
    /// `1`, value in the first 2 bytes of the entry's 4-byte value field).
    /// If the blob doesn't parse as a well-formed TIFF header + IFD0 at all,
    /// or no Orientation tag is found there, this returns `None` rather than
    /// guessing or returning the input unchanged - `encode_single_image`'s
    /// caller treats `None` as "drop the metadata entirely for this
    /// response" (see its own resolution step), which is the only choice
    /// that can't possibly leave a stale, un-neutralized Orientation tag in
    /// forwarded output. In practice this fallback essentially never
    /// triggers for real photos: `decoder.orientation()` (what
    /// `exif_orientation_applied` is derived from) only ever reports a
    /// non-`NoTransforms` value in the first place by successfully parsing
    /// this exact tag out of this exact blob shape via `image`'s own
    /// `Orientation::from_exif_chunk` - so the tag this function looks for
    /// is, barring a very unusual multi-IFD/sub-IFD layout, already known to
    /// be there.
    fn neutralize_exif_orientation(exif: &[u8]) -> Option<Vec<u8>> {
        const ORIENTATION_TAG: u16 = 0x0112;
        const TYPE_SHORT: u16 = 3;
        const TIFF_MAGIC: u16 = 42;
        const IFD_ENTRY_LEN: usize = 12;

        let little_endian = match exif.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let read_u16 = |b: &[u8]| -> u16 {
            let bytes = [b[0], b[1]];
            if little_endian {
                u16::from_le_bytes(bytes)
            } else {
                u16::from_be_bytes(bytes)
            }
        };
        let read_u32 = |b: &[u8]| -> u32 {
            let bytes = [b[0], b[1], b[2], b[3]];
            if little_endian {
                u32::from_le_bytes(bytes)
            } else {
                u32::from_be_bytes(bytes)
            }
        };

        if read_u16(exif.get(2..4)?) != TIFF_MAGIC {
            return None;
        }

        let ifd0_offset = read_u32(exif.get(4..8)?) as usize;
        let entries_start = ifd0_offset.checked_add(2)?;
        let entry_count = usize::from(read_u16(exif.get(ifd0_offset..entries_start)?));
        let entries_len = entry_count.checked_mul(IFD_ENTRY_LEN)?;
        let entries_end = entries_start.checked_add(entries_len)?;
        if entries_end > exif.len() {
            return None;
        }

        for i in 0..entry_count {
            let entry_start = entries_start + i * IFD_ENTRY_LEN;
            let tag = read_u16(exif.get(entry_start..entry_start + 2)?);
            if tag != ORIENTATION_TAG {
                continue;
            }

            let field_type = read_u16(exif.get(entry_start + 2..entry_start + 4)?);
            let count = read_u32(exif.get(entry_start + 4..entry_start + 8)?);
            // A well-formed Orientation entry is always exactly one SHORT -
            // anything else is a shape this function doesn't recognise, so
            // fail closed (see doc comment) rather than patch a field that
            // might not mean what it's assumed to mean.
            if field_type != TYPE_SHORT || count != 1 {
                return None;
            }

            let value_start = entry_start + 8;
            let mut patched = exif.to_vec();
            let normal: u16 = 1; // `Orientation::Normal` / "no transform needed"
            let normal_bytes = if little_endian {
                normal.to_le_bytes()
            } else {
                normal.to_be_bytes()
            };
            patched[value_start..value_start + 2].copy_from_slice(&normal_bytes);
            return Some(patched);
        }

        None
    }

    /// Maximum number of extra encode attempts `encode_with_max_bytes`'s
    /// quality search performs beyond the caller's own first choice of
    /// quality (#76, imgproxy's `max_bytes`/`mb:{bytes}`). Bounds real,
    /// measured encode cost - `benches/encode.rs`'s `jpeg_baseline`/
    /// `jpeg_progressive` benchmarks put a single JPEG encode at a few
    /// milliseconds for a resized (post-pipeline) image, so a handful of
    /// extra full encodes per request is a small, predictable tax. This is
    /// exactly why `max_bytes` is only wired up for JPEG output below
    /// (`encode_single_image`'s JPEG branch) and not, say, AVIF: AVIF
    /// encoding is roughly two orders of magnitude more expensive per the
    /// `ravif`/AVIF measurements in `adr/0001-image-engine.md` (150-300ms
    /// per encode at typical quality/speed settings), so even this same
    /// small attempt cap would turn one request into a multi-second tail
    /// latency spike - a cost this crate has no way to hide from the
    /// caller. A binary search over the `1..=255` `u8` quality range
    /// converges in at most 8 comparisons; capping at 6 trades the last
    /// step or two of precision (the resulting quality can land within a
    /// few units of the tightest possible fit) for a firm, small upper
    /// bound on total request cost.
    const MAX_BYTES_SEARCH_ATTEMPTS: u32 = 6;

    /// Iteratively lowers `encode_at`'s quality argument via binary search
    /// until the encoded output is at or under `max_bytes`, or
    /// [`Self::MAX_BYTES_SEARCH_ATTEMPTS`] is exhausted (#76, imgproxy's
    /// `max_bytes`/`mb:{bytes}`). Mirrors imgproxy's own documented
    /// best-effort behaviour ("automatically degrades the quality... until
    /// the image size is under the specified amount of bytes") - including
    /// what happens when even the lowest quality tried doesn't fit: the
    /// smallest output found across every attempt is returned rather than
    /// erroring the request out, since an unreachable byte budget is a
    /// caller configuration choice, not a failure this crate can recover
    /// from by trying harder.
    ///
    /// `initial_quality` is both the search's upper bound and the quality
    /// tried first (the caller's originally-requested quality, `q:`/`fq:`
    /// resolved) - if it already fits, this returns after that single
    /// encode with no extra attempts spent, so a `max_bytes` budget that's
    /// already satisfied by the ordinary request costs nothing extra.
    fn encode_with_max_bytes(
        max_bytes: u64,
        initial_quality: u8,
        mut encode_at: impl FnMut(u8) -> Result<Vec<u8>>,
    ) -> Result<Vec<u8>> {
        let first = encode_at(initial_quality)?;
        if first.len() as u64 <= max_bytes || initial_quality <= 1 {
            return Ok(first);
        }

        let mut best = first;
        let mut low: u8 = 1;
        let mut high: u8 = initial_quality - 1;

        for _ in 0..Self::MAX_BYTES_SEARCH_ATTEMPTS {
            if low > high {
                break;
            }
            let mid = low + (high - low) / 2;
            let candidate = encode_at(mid)?;

            if candidate.len() as u64 <= max_bytes {
                // Fits within budget - remember it (a higher quality is
                // always preferred among fitting candidates) and try a
                // higher quality next to see if it still fits.
                best = candidate;
                if mid == u8::MAX {
                    break;
                }
                low = mid + 1;
            } else {
                // Doesn't fit - if nothing has fit yet, keep whichever
                // over-budget candidate is smallest as the best-effort
                // fallback; either way, try a lower quality next.
                if best.len() as u64 > max_bytes && candidate.len() < best.len() {
                    best = candidate;
                }
                if mid == 0 {
                    break;
                }
                high = mid - 1;
            }
        }

        Ok(best)
    }

    /// Rejects a request whose requested output width/height exceed the
    /// configured maximum, independent of whatever the generated OpenAPI
    /// layer does or does not validate upstream (#26).
    ///
    /// #51: `zoom`, `dpr` and `min-width`/`min-height` can all inflate the
    /// *effective* requested size past the plain `width`/`height` value, so
    /// this now checks a conservative upper bound that folds them in too -
    /// otherwise `dpr:100` would trivially bypass #26's whole point. This
    /// runs before decode (no source dimensions are available yet), so it
    /// deliberately ignores the #36 enlarge guard (which can only ever
    /// *shrink* the effective request, never grow it) and instead checks
    /// the largest size the request could possibly resolve to.
    ///
    /// No `rotate`-driven axis swap is needed here, unlike
    /// `Self::effective_resize_box`: `params.width`/`height`/`min_width`/
    /// `min_height` already describe the *final*, post-rotation image (see
    /// that function's doc comment for the full reasoning imgproxy's own
    /// `ExtractGeometry` establishes), and `max_output_width`/
    /// `max_output_height` are configured in that same final-output sense
    /// - so `width` is always compared against `max_output_width`
    /// regardless of `rotate`, with no swap in between.
    fn check_output_dimensions(params: &ResizeQuery, config: &PerformanceConfig) -> Result<()> {
        Self::check_axis_bound(
            params.width,
            params.min_width,
            params.zoom_x,
            params.dpr,
            config.max_output_width,
            "width",
        )?;
        Self::check_axis_bound(
            params.height,
            params.min_height,
            params.zoom_y,
            params.dpr,
            config.max_output_height,
            "height",
        )?;

        Ok(())
    }

    /// One axis of [`Self::check_output_dimensions`]'s conservative upper
    /// bound: `max(explicit * max(zoom, 1) * max(dpr, 1), min)`. Only
    /// `zoom`/`dpr` values greater than `1.0` can grow the request past
    /// `max` (values `<= 1.0` only ever shrink it, since both are
    /// validated as strictly positive at parse time), and `min-width`/
    /// `min-height` are never themselves scaled by `zoom`/`dpr` (matching
    /// `Self::effective_resize_box`), so `min` is compared against the
    /// scaled `explicit` value directly rather than also being scaled.
    fn check_axis_bound(
        explicit: Option<u32>,
        min: Option<u32>,
        zoom: f32,
        dpr: f32,
        max: u32,
        axis_name: &str,
    ) -> Result<()> {
        let multiplier = f64::from(zoom.max(1.0)) * f64::from(dpr.max(1.0));
        let scaled_explicit = explicit.map(|v| (f64::from(v) * multiplier).ceil() as u64);

        let bound = match (scaled_explicit, min) {
            (Some(a), Some(b)) => a.max(u64::from(b)),
            (Some(a), None) => a,
            (None, Some(b)) => u64::from(b),
            (None, None) => return Ok(()),
        };

        if bound > u64::from(max) {
            anyhow::bail!(
                "Requested output dimensions too large: {axis_name} {bound} exceeds maximum {max}"
            );
        }

        Ok(())
    }

    /// `true` when a `rotate` angle (already normalised to `0..360` by the
    /// URL parser) swaps width and height - i.e. 90 or 270 degrees. Shared
    /// by `Self::check_output_dimensions` (pre-decode) and
    /// `Self::effective_resize_box` (post-decode) so both apply the exact
    /// same axis-swap rule.
    fn rotate_swaps_axes(degrees: i32) -> bool {
        (degrees.rem_euclid(360) / 90) % 2 == 1
    }


    /// Computes the boxes #51's sizing options (`zoom`, `dpr`, `min-width`/
    /// `min-height`, `rotate`) feed into the resize step and `extend`,
    /// folding in the #36 enlarge guard, in the order imgproxy itself
    /// applies them (verified against `prepare.go`'s `ExtractGeometry`/
    /// `calcScale`/`calcSizes` and `scale.go` in the `imgproxy/imgproxy` v4
    /// source at the time of writing).
    ///
    /// ## The key insight: do the math in "final-orientation" space
    ///
    /// `params.width`/`height`/`min_width`/`min_height` all describe the
    /// *final*, post-rotation image the caller wants back - that's the
    /// whole point of naming them (a caller writes `w:800/h:600/rot:90`
    /// expecting an 800x600 result, not an image that's 800x600 *before*
    /// being rotated into 600x800). imgproxy's own `ExtractGeometry`
    /// (`prepare.go`) reflects this directly: it swaps `SrcWidth`/
    /// `SrcHeight` into final orientation once, right at the top
    /// (`if (angle+baseAngle)%180 != 0 { width, height = height, width }`),
    /// and *every* subsequent calculation - `calcScale`'s enlarge guard,
    /// `calcSizes`'s `TargetWidth`/`TargetHeight` extend uses - runs
    /// entirely in that final-orientation space using the raw,
    /// **unswapped** `po.Width()`/`po.Height()`/`po.MinWidth()`/
    /// `po.MinHeight()`. Only right at the end, inside the `scale()`
    /// pipeline step itself (`if (c.Angle+c.PO.Rotate())%180==90 {
    /// wscale, hscale = hscale, wscale }`), does imgproxy translate the
    /// final-orientation result back into the actual pixel buffer's axes -
    /// because that pixel buffer hasn't been rotated yet at the point
    /// `scale()` runs (`rotateAndFlip` is the *next* pipeline step).
    ///
    /// This function mirrors that structure exactly:
    /// 1. Swap `src_width`/`src_height` into final orientation *once*, up
    ///    front (`final_src_w`/`final_src_h`) - this is the only swap that
    ///    happens before the main calculation.
    /// 2. Do zoom -> dpr -> enlarge-guard -> min-floor entirely in
    ///    final-orientation space, using `params.width`/`height`/
    ///    `min_width`/`min_height` completely unswapped (they already mean
    ///    "final"), compared against `final_src_w`/`final_src_h`.
    /// 3. `extend`'s target (`extend_box`) is read directly off this
    ///    final-orientation result too, *before* the enlarge guard/min
    ///    floor (steps within (2)) - no swap needed at all, since `extend`
    ///    runs *after* `Self::apply_rotate` in the pipeline (see that
    ///    function's call site), by which point the image is already in
    ///    final orientation. This is what lets `extend` synthesize the
    ///    originally-requested canvas size via background padding even
    ///    when `enlarge=false` refused to upscale the actual resized
    ///    pixels - `extend`'s whole purpose is "pad if the image ends up
    ///    smaller than requested", precisely the `enlarge=false` +
    ///    small-source case.
    /// 4. Only the very last step - producing `resize_box`, the box handed
    ///    to the actual pixel-buffer resize call below, which still runs
    ///    *before* rotation - swaps the final-orientation result back into
    ///    physical (pre-rotation) axes.
    ///
    /// Getting steps 1 and 4 backwards (swapping the *target* box against
    /// an *unswapped* source, instead of swapping the *source* once up
    /// front and the *result* once at the end) silently pairs the wrong
    /// axes against each other in the enlarge guard/min-floor math - a bug
    /// this function's implementation had during development, caught by
    /// working through imgproxy's actual `ExtractGeometry` source rather
    /// than guessing at the shape of the swap.
    ///
    /// `zoom`/`dpr` are also a deliberate narrowing of imgproxy: they only
    /// multiply an axis that already has an explicit `width`/`height` set
    /// (imgproxy can also scale the "natural" source size with no explicit
    /// width/height at all, via its shared shrink-factor machinery) - #51's
    /// brief specifically calls out the `dpr` + explicit-width responsive-
    /// image pattern as the case that matters, not zoom-alone. The #36
    /// enlarge guard is also a deliberate simplification: imgproxy's own
    /// `dpr`/enlarge interaction (`prepare.go`'s `DprScale` dance) lets
    /// `dpr` alone nudge slightly past what plain `enlarge=false` would
    /// allow in some edge cases; this crate uses one uniform rule instead -
    /// every option that can inflate the effective target size (`width`,
    /// `height`, zoom- and dpr-scaled alike) is gated by the same flag,
    /// except `min-width`/`min-height`, which - matching imgproxy's
    /// `calcScale` exactly - are **not** gated by `enlarge` at all: they
    /// can force upscaling past the source even with `enlarge=false`
    /// (`prepare.go`: the min-width/min-height block runs unconditionally,
    /// after the `!po.Enlarge()`-gated block).
    fn effective_resize_box(params: &ResizeQuery, src_width: u32, src_height: u32) -> EffectiveSizing {
        let sizing_active = params.width.is_some()
            || params.height.is_some()
            || params.min_width.is_some()
            || params.min_height.is_some();

        if !sizing_active {
            return EffectiveSizing {
                resize_box: (None, None),
                extend_box: None,
            };
        }

        let swap = Self::rotate_swaps_axes(params.rotate);

        // Step 1: source dimensions, swapped into final orientation once.
        let (final_src_w, final_src_h) = if swap {
            (src_height, src_width)
        } else {
            (src_width, src_height)
        };

        // Step 2a: zoom then dpr - entirely in final-orientation space,
        // `params.width`/`height` used directly (they already mean
        // "final"), only on axes with an explicit width/height.
        let scale_axis = |value: Option<u32>, zoom: f32| {
            value.map(|v| {
                (f64::from(v) * f64::from(zoom) * f64::from(params.dpr))
                    .round()
                    .max(1.0) as u32
            })
        };
        let mut final_width = scale_axis(params.width, params.zoom_x);
        let mut final_height = scale_axis(params.height, params.zoom_y);

        // Step 3: `extend`'s target, captured here (final-orientation,
        // pre-enlarge-guard, pre-min-floor) - no swap, see the doc comment.
        let extend_box = match (final_width, final_height) {
            (Some(w), Some(h)) => Some((w, h)),
            _ => None,
        };

        // Step 2b: enlarge guard (#36), final-orientation space.
        final_width = final_width.map(|w| if params.enlarge { w } else { w.min(final_src_w) });
        final_height = final_height.map(|h| if params.enlarge { h } else { h.min(final_src_h) });

        // Step 2c: min-width/min-height floor, final-orientation space,
        // deliberately bypassing `enlarge` (see the doc comment above).
        if let Some(mw) = params.min_width {
            final_width = Some(final_width.unwrap_or(final_src_w).max(mw));
        }
        if let Some(mh) = params.min_height {
            final_height = Some(final_height.unwrap_or(final_src_h).max(mh));
        }

        // Step 4: swap the final-orientation result back into physical
        // (pre-rotation) axes for the actual pixel-buffer resize call.
        let (resize_width, resize_height) = if swap {
            (final_height, final_width)
        } else {
            (final_width, final_height)
        };

        EffectiveSizing {
            resize_box: (resize_width, resize_height),
            extend_box,
        }
    }

    /// imgproxy's `trim`/`t` processing option (#51) - removes uniform-
    /// colour borders. Always the first geometry operation applied (see
    /// `ResizeQuery::trim`'s doc comment).
    ///
    /// The target colour is `trim.color` if given, otherwise the image's
    /// own top-left corner pixel - a deliberately simpler stand-in for
    /// imgproxy's own multi-corner "smart" background detection. A pixel
    /// counts as "background" when its maximum per-channel (Chebyshev)
    /// distance from that colour is within `trim.threshold` - simpler and
    /// more predictable than imgproxy's own perceptual metric, and easy to
    /// assert pixel-exactly in tests.
    ///
    /// Each edge is scanned independently inward until a non-background
    /// row/column is found; `equal_hor`/`equal_ver` then clamp the two
    /// opposing trim amounts to their minimum, so only a symmetric amount
    /// is cut. Degenerate guard: if a threshold wide enough (or a fully
    /// uniform image) would trim away the *entire* image on either axis,
    /// this returns the image unchanged instead of producing a 0-pixel
    /// dimension.
    fn apply_trim(img: &DynamicImage, trim: &TrimOptions) -> DynamicImage {
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        if width == 0 || height == 0 {
            return img.clone();
        }

        let target = trim.color.unwrap_or_else(|| {
            let p = rgba.get_pixel(0, 0);
            [p[0], p[1], p[2]]
        });

        let is_background = |x: u32, y: u32| -> bool {
            let p = rgba.get_pixel(x, y);
            let dr = (f32::from(p[0]) - f32::from(target[0])).abs();
            let dg = (f32::from(p[1]) - f32::from(target[1])).abs();
            let db = (f32::from(p[2]) - f32::from(target[2])).abs();
            dr.max(dg).max(db) <= trim.threshold
        };
        let row_is_background = |y: u32| (0..width).all(|x| is_background(x, y));
        let col_is_background = |x: u32| (0..height).all(|y| is_background(x, y));

        let mut top = 0u32;
        while top < height && row_is_background(top) {
            top += 1;
        }
        let mut bottom = 0u32;
        while bottom < height - top && row_is_background(height - 1 - bottom) {
            bottom += 1;
        }
        let mut left = 0u32;
        while left < width && col_is_background(left) {
            left += 1;
        }
        let mut right = 0u32;
        while right < width - left && col_is_background(width - 1 - right) {
            right += 1;
        }

        if trim.equal_hor {
            let m = left.min(right);
            left = m;
            right = m;
        }
        if trim.equal_ver {
            let m = top.min(bottom);
            top = m;
            bottom = m;
        }

        if top + bottom >= height || left + right >= width {
            return img.clone();
        }

        img.crop_imm(left, top, width - left - right, height - top - bottom)
    }

    /// imgproxy's `rotate`/`rot` processing option (#51). `degrees` is
    /// always one of `0`/`90`/`180`/`270` by the time it reaches here - the
    /// URL parser (`crate::modules::url::options::parse_rotate_angle`)
    /// rejects anything else at parse time and normalises negative angles
    /// into that range.
    fn apply_rotate(img: DynamicImage, degrees: i32) -> DynamicImage {
        match degrees.rem_euclid(360) {
            90 => img.rotate90(),
            180 => img.rotate180(),
            270 => img.rotate270(),
            _ => img,
        }
    }

    /// imgproxy's `flip`/`fl` processing option (#51) - mirrors along
    /// either or both axes. Order between the two mirrors doesn't matter
    /// (they're independent, commuting operations).
    fn apply_flip(img: DynamicImage, horizontal: bool, vertical: bool) -> DynamicImage {
        let img = if horizontal { img.fliph() } else { img };
        if vertical { img.flipv() } else { img }
    }

    /// Places `img` onto a new `new_w x new_h` canvas filled with
    /// `background`, at offset `(off_x, off_y)` - the shared primitive
    /// behind both `Self::apply_extend` and `Self::apply_padding` (#51).
    /// New pixels are fully opaque (`alpha = 255` when the image carries an
    /// alpha channel), so the pre-encode alpha stage (#34/#60) below - which
    /// only ever touches `alpha == 0` pixels - leaves them untouched, the
    /// same order imgproxy's own pipeline implies (`extend`/`padding` both
    /// run ahead of `flatten` there too).
    fn embed_on_background(
        img: &DynamicImage,
        new_w: u32,
        new_h: u32,
        off_x: u32,
        off_y: u32,
        background: [u8; 3],
    ) -> DynamicImage {
        if img.has_alpha() {
            let mut canvas = RgbaImage::from_pixel(
                new_w,
                new_h,
                Rgba([background[0], background[1], background[2], 255]),
            );
            image::imageops::overlay(&mut canvas, &img.to_rgba8(), i64::from(off_x), i64::from(off_y));
            DynamicImage::ImageRgba8(canvas)
        } else {
            let mut canvas = RgbImage::from_pixel(new_w, new_h, Rgb(background));
            image::imageops::overlay(&mut canvas, &img.to_rgb8(), i64::from(off_x), i64::from(off_y));
            DynamicImage::ImageRgb8(canvas)
        }
    }

    /// imgproxy's `extend`/`ex` processing option (#51) - pads the image up
    /// to `target_w x target_h`, centring the original within the new
    /// canvas, if (and only if) it's currently smaller than that box on
    /// either axis. A no-op otherwise. `target_w`/`target_h` come from
    /// `EffectiveSizing::extend_box` (`Self::effective_resize_box`), not
    /// the enlarge-capped resize box, which is what lets this pad out to
    /// the originally-requested size even when `enlarge=false` capped the
    /// actual resize.
    fn apply_extend(img: DynamicImage, target_w: u32, target_h: u32, background: [u8; 3]) -> DynamicImage {
        let (w, h) = img.dimensions();
        if w >= target_w && h >= target_h {
            return img;
        }

        let new_w = w.max(target_w);
        let new_h = h.max(target_h);
        let off_x = (new_w - w) / 2;
        let off_y = (new_h - h) / 2;

        Self::embed_on_background(&img, new_w, new_h, off_x, off_y, background)
    }

    /// imgproxy's `padding`/`pd` processing option (#51) - always enlarges
    /// the canvas by the given amount on each side, background-filled.
    /// Unlike imgproxy, padding values here are *not* scaled by `dpr` - a
    /// deliberate simplification (`pd:`+`dpr:` is a rare enough combination
    /// that literal pixel counts are more predictable/debuggable than a
    /// dpr-scaled equivalent). `u32::saturating_add` guards the new-canvas
    /// arithmetic; the #26 post-padding dimension check at this function's
    /// call site is what actually rejects an unreasonably large result.
    fn apply_padding(img: DynamicImage, padding: &Padding, background: [u8; 3]) -> DynamicImage {
        let (w, h) = img.dimensions();
        let new_w = w.saturating_add(padding.left).saturating_add(padding.right);
        let new_h = h.saturating_add(padding.top).saturating_add(padding.bottom);

        Self::embed_on_background(&img, new_w, new_h, padding.left, padding.top, background)
    }

    /// Rejects a decoded *source* resolution above `max_src_resolution_mp`
    /// megapixels. `width`/`height` here come from a header-only peek, not
    /// a full decode - see `peek_dimensions`.
    ///
    /// `pub(crate)`, not private: `crate::services::image::avif_codec`
    /// (#67) re-runs this exact same check as defense in depth around its
    /// own `avifDecoderParse` header read, rather than duplicating the
    /// megapixel-overflow-checked formula in a second module - see that
    /// module's `decode_inner` doc comment.
    pub(crate) fn check_source_resolution(
        width: u32,
        height: u32,
        max_src_resolution_mp: u64,
    ) -> Result<()> {
        let pixels = (width as u64)
            .checked_mul(height as u64)
            .context("Source image dimensions overflow while checking resolution")?;
        let megapixels = pixels / 1_000_000;

        if megapixels > max_src_resolution_mp {
            anyhow::bail!(
                "Source image resolution too large: {width}x{height} ({megapixels} MP, max {max_src_resolution_mp} MP)"
            );
        }

        Ok(())
    }

    /// Builds an `image::ImageReader` over `image_bytes`, using `format` as
    /// a hint when known (avoids re-sniffing magic bytes the caller already
    /// identified) and falling back to format guessing otherwise.
    fn make_reader(
        image_bytes: &[u8],
        format: Option<ImageFormat>,
    ) -> Result<image::ImageReader<Cursor<&[u8]>>> {
        match format {
            Some(format) => Ok(image::ImageReader::with_format(
                Cursor::new(image_bytes),
                format,
            )),
            None => image::ImageReader::new(Cursor::new(image_bytes))
                .with_guessed_format()
                .context("Failed to detect image format"),
        }
    }

    /// Reads only the image header to get its dimensions, without decoding
    /// pixel data - what makes it safe to call on a potential
    /// decompression-bomb source ahead of the resolution check.
    ///
    /// AVIF (#67) is special-cased: `image::ImageReader` can't parse an
    /// AVIF header at all without the `avif-native` feature this crate
    /// doesn't enable (see `avif_decode`'s doc comment), so
    /// `avif_codec::peek_dimensions` reads it via `libavif`'s own
    /// `avifDecoderParse` instead - which, like `into_dimensions` below for
    /// every other format, reads container/header structure only and never
    /// touches the actual AV1-coded pixel payload.
    fn peek_dimensions(image_bytes: &[u8], format: Option<ImageFormat>) -> Result<(u32, u32)> {
        if format == Some(ImageFormat::Avif) {
            return crate::services::image::avif_codec::peek_dimensions(image_bytes);
        }

        Self::make_reader(image_bytes, format)?
            .into_dimensions()
            .context("Failed to read image dimensions")
    }

    /// Decodes `image_bytes`, enforcing every guard `decode_with_image_crate`
    /// carries, with one addition (#63 stage 2, extended by #67): for JPEG,
    /// tries a decode through mozjpeg/libjpeg-turbo first - DCT-scaled when
    /// a resize makes a smaller decode safe (decoding directly at a
    /// fraction of the source resolution instead of always decoding
    /// full-size and discarding most of the data during resize), full-size
    /// otherwise. #67 measured mozjpeg's full-size decode ~1.5x faster than
    /// the `image`-crate/zune-jpeg path too, not just the scaled case #63
    /// stage 2 covered - see `decode_jpeg_scaled`'s doc comment for the
    /// measured win either way. PNG and WebP are untouched, always going
    /// straight to `decode_with_image_crate`.
    ///
    /// If the mozjpeg path fails for any reason - including a caught panic,
    /// see `mozjpeg_decode` - this falls back to `decode_with_image_crate`
    /// (the exact same full-decode path every non-JPEG format already uses)
    /// rather than failing the request outright (#4), logging a warning so a
    /// real regression in the mozjpeg path stays visible instead of quietly
    /// becoming the normal path for every request.
    ///
    /// #88: `params.strip_metadata` is resolved to a plain `want_metadata`
    /// bool exactly once, here, and threaded into both decode paths below,
    /// so neither one has to ask "is this wanted?" itself - see
    /// `decode_with_image_crate`'s doc comment for why that's a bare `bool`
    /// rather than the full `params`.
    fn decode_with_limits(
        image_bytes: &[u8],
        format: Option<ImageFormat>,
        max_src_resolution_mp: u64,
        params: &ResizeQuery,
    ) -> Result<DecodedImage> {
        if format == Some(ImageFormat::Jpeg) {
            match Self::decode_jpeg_scaled(image_bytes, max_src_resolution_mp, params) {
                Ok(result) => return Ok(result),
                Err(err) => {
                    warn!(
                        error = %err,
                        "mozjpeg scaled JPEG decode failed; falling back to full image-crate decode"
                    );
                }
            }
        }

        // #66: libwebp instead of `image-webp`'s pure-Rust decoder - see
        // `decode_webp_libwebp`'s own doc comment for the measured win and
        // every guard preserved. Same graceful-fallback spirit as JPEG
        // above: a real libwebp failure falls back to
        // `decode_with_image_crate` (the exact pre-#66 WebP decode path)
        // rather than failing the request outright.
        if format == Some(ImageFormat::WebP) {
            match Self::decode_webp_libwebp(image_bytes, max_src_resolution_mp) {
                Ok(result) => return Ok(result),
                Err(err) => {
                    warn!(
                        error = %err,
                        "libwebp decode failed; falling back to full image-crate decode"
                    );
                }
            }
        }

        // #67 (AVIF decode): the only AVIF decode path this crate has -
        // `image`'s own decoder needs the separate `avif-native` feature,
        // not enabled here (see `avif_decode`'s doc comment for why one
        // dependency, `libavif-sys`, covers both AVIF directions). No
        // fallback decoder exists for this format, unlike JPEG/WebP above,
        // so a failure here is returned directly.
        if format == Some(ImageFormat::Avif) {
            return crate::services::image::avif_codec::decode(image_bytes, max_src_resolution_mp);
        }

        Self::decode_with_image_crate(
            image_bytes,
            format,
            max_src_resolution_mp,
            !params.strip_metadata,
        )
    }

    /// Decodes `image_bytes` with explicit `image::Limits` derived from
    /// `max_src_resolution_mp`, instead of inheriting the crate's
    /// accidental 512MiB `max_alloc` default (#26). This is defense in
    /// depth behind `check_source_resolution`'s header-only check, not a
    /// replacement for it.
    ///
    /// Also returns the source's EXIF `Orientation` (#33), embedded ICC
    /// colour profile, and raw EXIF metadata blob (#5), if any - all three
    /// must be read off the `ImageDecoder` before it's consumed by
    /// `DynamicImage::from_decoder`, which is why this goes through
    /// `ImageReader::into_decoder` rather than the simpler
    /// `ImageReader::decode` it used to call directly. `orientation`
    /// defaults to `Orientation::NoTransforms` (rather than failing the
    /// whole request) if it can't be read - a source with malformed EXIF
    /// should still decode and process, just without autorotation; the raw
    /// EXIF blob (used only when `!params.strip_metadata`) similarly
    /// defaults to `None` rather than failing the request.
    ///
    /// This is the only decode path for PNG/WebP, and the fallback path for
    /// JPEG when `decode_jpeg_scaled` (#63 stage 2) fails - see
    /// `decode_with_limits`.
    ///
    /// #88: `want_metadata` (`!params.strip_metadata`, resolved once by the
    /// caller) gates whether `exif_metadata()` is even called - previously
    /// it always was, and the discard decision was made later, in
    /// `encode_single_image` (`params.strip_metadata`), after the blob had
    /// already been extracted. A bare `bool` rather than threading the full
    /// `&ResizeQuery` through: this function uses nothing else from it, and
    /// `decode_jpeg_scaled` (which does need more of `params`, for
    /// `autorotate`/`effective_resize_target`) already takes it directly -
    /// no need to widen this one's dependency surface to match.
    fn decode_with_image_crate(
        image_bytes: &[u8],
        format: Option<ImageFormat>,
        max_src_resolution_mp: u64,
        want_metadata: bool,
    ) -> Result<DecodedImage> {
        let mut reader = Self::make_reader(image_bytes, format)?;
        let limits = Self::build_decode_limits(max_src_resolution_mp);
        reader.limits(limits.clone());

        let mut decoder = reader
            .into_decoder()
            .context("Failed to construct image decoder")?;

        // `ImageReader::decode` applies one extra allocation guard beyond
        // `set_limits`'s dimension check: it reserves `decoder.total_bytes()`
        // against `max_alloc` before any pixel data is decoded.
        // `into_decoder` alone (needed here so orientation/ICC can be read
        // off the decoder before it's consumed) skips that step, so it's
        // replicated by hand to keep the exact same allocation guard #26
        // relies on.
        let mut reserved_limits = limits;
        reserved_limits
            .reserve(decoder.total_bytes())
            .context("Failed to decode image")?;
        decoder
            .set_limits(reserved_limits)
            .context("Failed to decode image")?;

        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let icc_profile = decoder.icc_profile().ok().flatten();
        // #5, narrowed by #88: read alongside `icc_profile`/`orientation`,
        // for the same reason - the `ImageDecoder` is consumed by
        // `from_decoder` right below, so anything not read off it now is
        // gone. But only when `want_metadata` says the caller will actually
        // keep it (`!params.strip_metadata`, the default is `false` here) -
        // otherwise this is skipped entirely rather than extracting a blob
        // just to throw it away in `encode_single_image`. `orientation()`
        // just above is unaffected by this and always runs: for the PNG/
        // WebP decoders reached through this function it's cheap (PNG: a
        // clone of an already-parsed header field; WebP: same double-call
        // shape as JPEG's own decoder, see `decode_jpeg_scaled`'s
        // `exif_metadata` comment) and, more importantly, `autorotate`
        // depends on it regardless of `strip_metadata`.
        //
        // `Ok(None)` is `ImageDecoder::exif_metadata`'s own default for
        // formats/decoders that don't implement it
        // (`image-0.25.10/src/io/decoder.rs`) - a source with no EXIF
        // segment at all (or a format this crate doesn't extract EXIF from)
        // is indistinguishable from "read failed", and both mean "nothing
        // to forward", so `.ok().flatten()` treats them the same, mirroring
        // `icc_profile` immediately above.
        let exif_metadata = if want_metadata {
            decoder.exif_metadata().ok().flatten()
        } else {
            None
        };

        let img = image::DynamicImage::from_decoder(decoder).context("Failed to decode image")?;
        Ok((img, orientation, icc_profile, exif_metadata))
    }

    /// WebP-only pixel decode via `libwebp` (real libwebp, through the
    /// `webp` crate's `Decoder`) instead of `image-webp`'s pure-Rust VP8/
    /// VP8L decoder - measured 2.5-2.8x faster on the Kodak corpus (24 real
    /// photos, darwin/arm64: 11.10ms -> 4.53ms median at native resolution)
    /// with DSSIM delta 0.000000 against `image-webp`'s output on every
    /// image (pixel-identical), reproduced against this exact code path -
    /// see this change's own report for the measurement, not just the
    /// prior survey it was based on.
    ///
    /// Mirrors `decode_jpeg_scaled`'s structure exactly: an `image`-crate
    /// `WebPDecoder` is opened first, purely for the header-derived data
    /// libwebp's one-shot `WebPDecodeRGB(A)` API has no equivalent for -
    /// #33's EXIF `Orientation` (WebP has no EXIF-orientation convention in
    /// practice, but `WebPDecoder::orientation()` is called for the same
    /// "read every metadata field the trait offers, uniformly" reason every
    /// other decode path in this file does) and #5's ICC profile / raw EXIF
    /// blob - all three must be read off the `image`-crate decoder before
    /// it's dropped, since libwebp's decode call never sees them at all.
    /// #26's allocation guard (`Limits::reserve` against
    /// `decoder.total_bytes()`) is applied to this throwaway decoder before
    /// any pixel data is decoded through *either* decoder, same as
    /// `decode_jpeg_scaled`.
    ///
    /// The actual pixel decode is `Self::libwebp_decode`, wrapped in
    /// `catch_unwind` - see that function's own doc comment for why, even
    /// though libwebp's C API itself returns null/`None` on failure rather
    /// than unwinding the way mozjpeg's error manager does; the guard here
    /// is defensive-in-depth against a debug assertion or buffer-shape
    /// mismatch inside the `webp` crate's own Rust wrapper code (e.g.
    /// `WebPImage::to_image`'s `.expect(..)`, which this function avoids
    /// calling directly for exactly that reason - see `libwebp_decode`'s
    /// own doc comment), not because libwebp's C decode functions are
    /// expected to panic.
    ///
    /// Every guard `decode_with_image_crate` carries - the #26 resolution
    /// cap (checked by the caller, `decode_with_limits`'s caller, against
    /// header-peeked dimensions *before* this function is ever reached) and
    /// the allocation-reservation guard above - is preserved here too.
    /// libwebp's one-shot decode API takes no `image::Limits`-equivalent
    /// parameter of its own, so there is nothing further to configure on
    /// that side; the header-peek-before-decode ordering is what actually
    /// keeps a WebP decompression bomb from reaching this function with an
    /// unchecked resolution in the first place (verified by
    /// `webp_decompression_bomb_is_rejected_before_full_decode` below).
    ///
    /// Animated WebP is untouched: `process_image_blocking_with_limits_and_watermark`'s
    /// `wants_animatable_output` branch already intercepts any WebP source
    /// with more than one frame via `decode_animation_source`'s own
    /// `WebPDecoder::has_animation()` check, *before* `decode_with_limits`
    /// (and therefore this function) is ever reached for that source - see
    /// `decode_animation_source`'s doc comment. `libwebp_decode`'s own
    /// `webp::Decoder::decode()` call additionally refuses an animated
    /// bitstream on its own (`features.has_animation()` -> `None`,
    /// `webp-0.3.1/src/decoder.rs`), so a genuinely-animated WebP reaching
    /// this function by some other path would fail closed (falling back to
    /// `decode_with_image_crate` via `decode_with_limits`'s own fallback,
    /// same as any other libwebp failure) rather than silently decoding
    /// only its first frame as if it were a still image.
    fn decode_webp_libwebp(image_bytes: &[u8], max_src_resolution_mp: u64) -> Result<DecodedImage> {
        let mut reader = Self::make_reader(image_bytes, Some(ImageFormat::WebP))?;
        let limits = Self::build_decode_limits(max_src_resolution_mp);
        reader.limits(limits.clone());

        let mut decoder = reader
            .into_decoder()
            .context("Failed to construct WebP decoder for header read")?;

        let mut reserved_limits = limits;
        reserved_limits
            .reserve(decoder.total_bytes())
            .context("Failed to decode image")?;
        decoder
            .set_limits(reserved_limits)
            .context("Failed to decode image")?;

        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let icc_profile = decoder.icc_profile().ok().flatten();
        let exif_metadata = decoder.exif_metadata().ok().flatten();
        drop(decoder);

        let img = Self::libwebp_decode(image_bytes)?;
        Ok((img, orientation, icc_profile, exif_metadata))
    }

    /// Runs the actual libwebp pixel decode, producing an `Rgb8`/`Rgba8`
    /// `DynamicImage` depending on whether the source has an alpha channel.
    ///
    /// Wrapped in `catch_unwind`, same defensive spirit as
    /// `mozjpeg_decode`: nothing in the `webp` crate or `libwebp-sys` is
    /// known to panic on malformed input (unlike mozjpeg's documented
    /// unwind-on-error design), but this is attacker-supplied input
    /// reaching a C library through an FFI boundary, and `Cargo.toml`'s
    /// `panic = "unwind"` (kept for #29) is what makes any such panic
    /// catchable at all rather than aborting the process - the same
    /// invariant every other codec entry point in this file relies on.
    /// `AssertUnwindSafe` is sound here for the same reason it is in
    /// `mozjpeg_decode`: `image_bytes` is a shared `&[u8]` with no interior
    /// mutability to leave torn.
    ///
    /// Deliberately builds the `image::RgbImage`/`RgbaImage` by hand via
    /// `from_raw` (which returns `Option`, i.e. a normal `Err` on a length
    /// mismatch) rather than calling `WebPImage::to_image()` - that method
    /// exists on the `webp` crate's own `WebPImage` type but calls
    /// `.expect("ImageBuffer couldn't be created")` internally
    /// (`webp-0.3.1/src/shared.rs`), which would turn a shape mismatch into
    /// an uncatchable-by-design panic path instead of a graceful `Result`.
    ///
    /// `pub` (like `mozjpeg_decode`/`encode_webp`/`encode_jpeg` above) so
    /// `benches/decode.rs` can benchmark the exact WebP decode path
    /// production uses (#66) instead of the `image::load_from_memory_with_format`
    /// call that was representative before this change but now only
    /// reflects the pre-#66 decoder.
    pub fn libwebp_decode(image_bytes: &[u8]) -> Result<DynamicImage> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::libwebp_decode_inner(image_bytes)
        }))
        .unwrap_or_else(|payload| {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "libwebp decode panicked with a non-string payload".to_string());
            Err(anyhow::anyhow!("libwebp decode panicked: {msg}"))
        })
    }

    fn libwebp_decode_inner(image_bytes: &[u8]) -> Result<DynamicImage> {
        let decoder = webp::Decoder::new(image_bytes);
        let image = decoder
            .decode()
            .ok_or_else(|| anyhow::anyhow!("libwebp: failed to decode WebP image"))?;
        let (width, height) = (image.width(), image.height());

        if image.is_alpha() {
            let buf = image::RgbaImage::from_raw(width, height, image.to_vec())
                .context("libwebp: decoded RGBA buffer size mismatch")?;
            Ok(DynamicImage::ImageRgba8(buf))
        } else {
            let buf = image::RgbImage::from_raw(width, height, image.to_vec())
                .context("libwebp: decoded RGB buffer size mismatch")?;
            Ok(DynamicImage::ImageRgb8(buf))
        }
    }

    /// JPEG-only DCT-scaled decode via mozjpeg/libjpeg-turbo (#63 stage 2):
    /// decodes directly at a reduced resolution using libjpeg's native
    /// `scale_num/8` support, instead of always decoding at full resolution
    /// and discarding most of the data during resize. This is the large-
    /// downscale path that drove the p90/p99 gap against imgproxy (see the
    /// #63 issue thread) - measured 58.03ms (full decode + resize) vs
    /// 26.21ms (1/8-scale decode + resize) for a 4K source to a 200x113
    /// thumbnail, 2.21x.
    ///
    /// Every guard `decode_with_image_crate` carries is preserved here too.
    /// An `image`-crate `ImageDecoder` is always opened first, purely to
    /// read the header:
    /// - #26's allocation guard (`Limits::reserve` against
    ///   `decoder.total_bytes()`) is applied to it before any pixel data is
    ///   decoded through *either* decoder below. (#26's *resolution* check
    ///   itself already ran in the caller, against the header-peeked
    ///   dimensions, before `decode_with_limits` is ever reached - not
    ///   repeated here.)
    /// - #33's EXIF orientation and ICC profile are read off it too, and so
    ///   (#5) is the raw EXIF metadata blob.
    ///
    /// Every JPEG decode - DCT-scaled or not - hands off to mozjpeg for the
    /// actual pixel decode (#67): `scale_num == 8` just means
    /// `mozjpeg_decode` is called with no DCT reduction, rather than the
    /// `image`-crate/zune-jpeg decoder being reused for that case as it was
    /// before #67. See the retired-rationale comment at the `drop(decoder)`
    /// call site below for why, and `select_jpeg_dct_scale` for how the
    /// scale factor itself is chosen - the "never decode smaller than the
    /// target" requirement lives there.
    fn decode_jpeg_scaled(
        image_bytes: &[u8],
        max_src_resolution_mp: u64,
        params: &ResizeQuery,
    ) -> Result<DecodedImage> {
        let mut reader = Self::make_reader(image_bytes, Some(ImageFormat::Jpeg))?;
        let limits = Self::build_decode_limits(max_src_resolution_mp);
        reader.limits(limits.clone());

        let mut decoder = reader
            .into_decoder()
            .context("Failed to construct image decoder for header read")?;

        // Same allocation guard as `decode_with_image_crate` - see that
        // function's doc comment. Applied here even though this decoder
        // never decodes a pixel: it's the cheapest possible place to keep
        // the guard, and keeps this path's defense-in-depth identical to
        // the fallback's.
        let mut reserved_limits = limits;
        reserved_limits
            .reserve(decoder.total_bytes())
            .context("Failed to decode image")?;
        decoder
            .set_limits(reserved_limits)
            .context("Failed to decode image")?;

        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let icc_profile = decoder.icc_profile().ok().flatten();
        // #5, narrowed by #88: same "read off the `ImageDecoder` before
        // it's dropped" reasoning as `decode_with_image_crate` - see that
        // function's doc comment for the `want_metadata` gating this
        // mirrors (`decode_with_limits` already has `params` in scope here,
        // so this checks `params.strip_metadata` directly instead of
        // needing a separate bool threaded in).
        //
        // Correction to what used to be claimed here: `decoder.orientation()`
        // above does *not* make this call cheap. Its own doc comment says
        // "`exif_metadata` caches the orientation, so call it if orientation
        // hasn't been set yet" - i.e. `orientation()` calls *into*
        // `exif_metadata()` (image-0.25.10's `JpegDecoder::orientation`,
        // `src/codecs/jpeg/decoder.rs:140-146`) to compute and cache the
        // `Orientation`, then discards the blob that call produced
        // (`let _ = self.exif_metadata()?;`). Only the orientation is
        // cached - `JpegDecoder::exif_metadata` itself
        // (`src/codecs/jpeg/decoder.rs:97-138`) caches nothing and redoes
        // the full parse from scratch on every call: a fresh
        // `zune_jpeg::JpegDecoder`, `decode_headers()`, and
        // `exif().cloned()`. So before this change, a JPEG going through
        // this path paid for that parse *twice* per request regardless of
        // `strip_metadata` - once inside `orientation()` (result thrown
        // away) and once here. Gating this call on `!params.strip_metadata`
        // both skips the unwanted copy on the default path and removes the
        // redundant second parse on the `sm:0` path (`orientation()` still
        // runs the first one, since autorotate needs it unconditionally).
        let exif_metadata = if params.strip_metadata {
            None
        } else {
            decoder.exif_metadata().ok().flatten()
        };
        let (header_width, header_height) = decoder.dimensions();

        // `Orientation::Rotate90`/`Rotate270`/`Rotate90FlipH`/`Rotate270FlipH`
        // swap width and height when applied (see `DynamicImage::apply_orientation`,
        // image-0.25.10 `src/images/dynimage.rs:1161-1179`) - mirrored here
        // via `axis_swap` so every dimension computed below is in the same
        // axis space the *real* resize stage will see. Gated on
        // `params.autorotate`: when it's off, orientation is never applied
        // (see the call site in `process_image_blocking_with_limits`), so
        // the image stays in its raw axes regardless of what the EXIF tag
        // says.
        let axis_swap = params.autorotate
            && matches!(
                orientation,
                Orientation::Rotate90
                    | Orientation::Rotate270
                    | Orientation::Rotate90FlipH
                    | Orientation::Rotate270FlipH
            );

        // The dimensions the resize stage will actually see, i.e. what
        // `img.dimensions()` reports right after `apply_orientation` in
        // `process_image_blocking_with_limits` (that call site's own
        // `src_width`/`src_height`).
        let (post_orientation_width, post_orientation_height) = if axis_swap {
            (header_height, header_width)
        } else {
            (header_width, header_height)
        };

        let (target_width, target_height) = Self::effective_resize_target(
            post_orientation_width,
            post_orientation_height,
            params,
        );

        // Map the target back into the JPEG's raw (pre-rotation) axis space:
        // mozjpeg's `scale()` operates on the raw raster as stored in the
        // file, before any EXIF rotation - that correction happens
        // afterwards, in Rust, exactly as it already did before this change
        // (`img.apply_orientation(orientation)` in
        // `process_image_blocking_with_limits`).
        let (raw_target_width, raw_target_height) = if axis_swap {
            (target_height, target_width)
        } else {
            (target_width, target_height)
        };

        let scale_num = Self::select_jpeg_dct_scale(
            header_width,
            header_height,
            raw_target_width,
            raw_target_height,
        );

        // RETIRED (#67), keeping the history because the reasoning was
        // real, not a mistake: this used to special-case `scale_num == 8`
        // (no DCT reduction is safe/useful for this request - either no
        // resize was requested at all, or the requested output is close
        // enough to source resolution that even the gentlest 1/2 scale
        // would decode below target) by reusing the already-open
        // `image`-crate decoder instead of opening mozjpeg, on the theory
        // that mozjpeg would buy nothing for a full-size decode and that
        // doing so kept output byte-for-byte identical to
        // `decode_with_image_crate` rather than introducing a second JPEG
        // decoder's rounding into a path that was never going to benefit.
        //
        // Both premises turned out wrong once actually measured (#67, not
        // assumed): `image` 0.25.10 pulled in `zune-jpeg` 0.5.x, whose
        // generic `ZByteReaderTrait` redesign made the per-byte
        // end-of-stream check in its Huffman bit-refill loop fallible
        // (`reader.eof()?`) where 0.4.x's was an infallible, immutable
        // `bool` - a real cost paid on every byte of entropy-coded data,
        // even for zune-jpeg's own in-memory reader. Measured on 36 real
        // photographs (24-image Kodak corpus + 3 picsum.photos sources x 4
        // resolutions, 640x360 through 3840x2160 - not the synthetic
        // gradient-plus-noise fixture `benches/fixtures.rs` uses elsewhere,
        // per the same reasoning `adr/0003`/`adr/0004`/`adr/0005` all
        // document - 0004's numbers are superseded but its method, which
        // is what is cited here, is not), mozjpeg's
        // full-size (`scale(8)`) decode is consistently ~1.5x faster than
        // the `image`-crate path across every resolution bucket - see this
        // change's own report for the full table. "Byte-for-byte identical"
        // was also never a real requirement, just a convenient side effect
        // of the old code path: mozjpeg's IDCT/chroma-upsampling rounding
        // differs from zune-jpeg's by at most 3/255 per channel across that
        // same 36-photo corpus, DSSIM median 0.0000081 / max 0.0000140 -
        // imperceptible by the same DSSIM bar this project already accepts
        // elsewhere (the `fast_image_resize` kernel swap at
        // 0.0000047-0.0000093 and the alpha-fringe fix at 0.0000035, both
        // in `.bench-baseline/BASELINE.md`'s "Current baseline" section).
        // Every JPEG decode now goes through the one decoder instead of two
        // decoders whose selection depended on whether the request
        // happened to downscale.
        drop(decoder);

        let img = Self::mozjpeg_decode(image_bytes, scale_num)?;

        Ok((img, orientation, icc_profile, exif_metadata))
    }

    /// Predicts the exact target dimensions the resize stage in
    /// `encode_single_image` (`Self::effective_resize_box`'s `resize_box`
    /// output, and the `Fit`/`Fill`/`Force`/`Auto` match right after it in
    /// `Self::resize_and_filter`) will compute for a decoded (and, if
    /// `params.autorotate` is set, already-rotated) image of
    /// `src_width x src_height` - every resize-type branch is reproduced
    /// verbatim from that call site, sharing the same `resize_dimensions`
    /// aspect-ratio helper so the two can't drift on that part of the
    /// arithmetic.
    ///
    /// `effective_width`/`effective_height` themselves come from
    /// `Self::effective_resize_box` (#51) rather than a second, hand-rolled
    /// copy of its `zoom`/`dpr`/enlarge-guard/`min-width`/`min-height`/
    /// `rotate`-axis-swap math: an earlier version of this function only
    /// applied the #36 enlarge guard to `params.width`/`params.height`
    /// directly, silently ignoring `zoom`/`dpr` (which can make the real
    /// target *larger* than `params.width`/`height` alone) and `min-width`/
    /// `min-height` (which can force a floor past even that) - for a
    /// request combining any of those with a JPEG source, that under-
    /// estimated target could make `select_jpeg_dct_scale` below pick a
    /// DCT scale that decodes *smaller* than the real resize step needs,
    /// which is exactly the "never decode smaller than target" invariant
    /// this function exists to uphold. Delegating to the one real
    /// `effective_resize_box` implementation instead of a parallel copy
    /// closes that gap structurally, not just for the cases caught so far.
    ///
    /// Exists so `decode_jpeg_scaled` (#63 stage 2) can pick a DCT scale
    /// *before* decoding, without duplicating the real resize call itself.
    fn effective_resize_target(src_width: u32, src_height: u32, params: &ResizeQuery) -> (u32, u32) {
        let sizing = Self::effective_resize_box(params, src_width, src_height);
        let (effective_width, effective_height) = sizing.resize_box;

        match (effective_width, effective_height) {
            (Some(w), None) => Self::resize_dimensions(src_width, src_height, w, u32::MAX, false),
            (None, Some(h)) => Self::resize_dimensions(src_width, src_height, u32::MAX, h, false),
            (Some(w), Some(h)) => match params.resize_type {
                ResizeType::Fit => Self::resize_dimensions(src_width, src_height, w, h, false),
                // `Fill` resizes to the *intermediate* (pre-crop) cover size
                // before cropping down to exactly `w x h` (see
                // `fir_resize_to_fill`) - that intermediate size, not the
                // final `w x h` box, is the real minimum decode requirement:
                // decoding only as small as the post-crop box would force
                // the intermediate resize step to upscale back up.
                ResizeType::Fill => Self::resize_dimensions(src_width, src_height, w, h, true),
                ResizeType::Force => (w.max(1), h.max(1)),
                ResizeType::Auto => {
                    let src_landscape = src_width >= src_height;
                    let dst_landscape = w >= h;
                    Self::resize_dimensions(
                        src_width,
                        src_height,
                        w,
                        h,
                        src_landscape == dst_landscape,
                    )
                }
            },
            // No resize requested at all - the decoded image is the output,
            // so nothing smaller than full resolution is safe.
            (None, None) => (src_width, src_height),
        }
    }

    /// Picks the most aggressive libjpeg DCT scale (`scale_num/8`, see
    /// `mozjpeg::Decompress::scale`) whose decoded output is still `>=` the
    /// requested target in *both* dimensions - i.e. the smallest safe
    /// decode. Never returns a scale that would decode smaller than
    /// `target_width`/`target_height`: doing so would force
    /// `fast_image_resize` to upscale back up before its final downscale,
    /// destroying quality for no benefit (the whole point of this function).
    ///
    /// libjpeg-turbo supports any `scale_num` in 1..=16 (any N/8), but only
    /// the well-known fractions are tried here - 1/8, 1/4, 1/2, and 8/8 (no
    /// scaling) - the useful ratios for shrink-on-load per #63 stage 2.
    /// Tried from most to least aggressive, so the first match is the
    /// smallest valid decode; falls through to 8 (full resolution, i.e. no
    /// DCT scaling at all) if even 1/2 would decode below target - which is
    /// always the outcome when no resize was requested at all, since
    /// `effective_resize_target`'s `(None, None)` branch sets the target to
    /// the full source size.
    fn select_jpeg_dct_scale(
        raw_width: u32,
        raw_height: u32,
        target_width: u32,
        target_height: u32,
    ) -> u8 {
        for scale_num in [1u8, 2, 4] {
            let scaled_width = Self::mozjpeg_scaled_dimension(raw_width, scale_num);
            let scaled_height = Self::mozjpeg_scaled_dimension(raw_height, scale_num);
            if scaled_width >= target_width && scaled_height >= target_height {
                return scale_num;
            }
        }
        8
    }

    /// Reproduces libjpeg's own output-size formula for a scaled decode -
    /// `jdiv_round_up(dim * scale_num, 8)` (`jdiv_round_up` in libjpeg-turbo's
    /// `jutils.c`, used by `jdmaster.c`'s `jinit_master_decompress` to set
    /// `output_width`/`output_height`) - exactly, so this prediction matches
    /// what `Decompress::scale` will actually produce.
    fn mozjpeg_scaled_dimension(dim: u32, scale_num: u8) -> u32 {
        (u64::from(dim) * u64::from(scale_num)).div_ceil(8) as u32
    }

    /// Runs the actual mozjpeg/libjpeg-turbo pixel decode at `scale_num/8`
    /// scale, producing an `Rgb8` `DynamicImage`.
    ///
    /// Wrapped in `catch_unwind`: mozjpeg's error manager (see the
    /// `mozjpeg` crate's `errormgr.rs::unwind_error_exit`, which this crate
    /// vendors as a dependency but does not modify) deliberately *unwinds*
    /// on a fatal libjpeg error rather than returning an `Err` - upstream's
    /// documented behaviour, not a bug. Left uncaught, that panic would
    /// bypass `decode_jpeg_scaled`'s `Result`-based error handling entirely
    /// and skip straight past the graceful-fallback path #4 requires.
    /// `catch_unwind` turns it into a normal `Err` here so a malformed or
    /// hostile JPEG that trips it falls back to `decode_with_image_crate`
    /// exactly like any other mozjpeg failure - `Cargo.toml`'s
    /// `panic = "unwind"` (kept specifically for #29, decoding untrusted
    /// input) is what makes this catchable at all, rather than aborting the
    /// process. `AssertUnwindSafe` is sound here: `image_bytes` is a shared
    /// `&[u8]` with no interior mutability to leave torn, and `scale_num` is
    /// `Copy`.
    ///
    /// `pub` (like `encode_webp`/`encode_jpeg` above) so `benches/decode.rs`
    /// can benchmark the exact path production uses for JPEG (#67) instead
    /// of the raw `image::load_from_memory_with_format` call that used to
    /// be representative but, after #67, only reflects PNG/WebP.
    pub fn mozjpeg_decode(image_bytes: &[u8], scale_num: u8) -> Result<DynamicImage> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::mozjpeg_decode_inner(image_bytes, scale_num)
        }))
        .unwrap_or_else(|payload| {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                .unwrap_or_else(|| "mozjpeg panicked with a non-string payload".to_string());
            Err(anyhow::anyhow!("mozjpeg decode panicked: {msg}"))
        })
    }

    fn mozjpeg_decode_inner(image_bytes: &[u8], scale_num: u8) -> Result<DynamicImage> {
        let mut decompress = mozjpeg::Decompress::new_mem(image_bytes)
            .context("mozjpeg: failed to read JPEG header")?;
        decompress.scale(scale_num);

        let mut started = decompress
            .rgb()
            .context("mozjpeg: failed to start decompression")?;
        let width = started.width() as u32;
        let height = started.height() as u32;
        let pixels: Vec<u8> = started
            .read_scanlines()
            .context("mozjpeg: failed to read scanlines")?;
        started
            .finish()
            .context("mozjpeg: failed to finish decompression")?;

        image::RgbImage::from_raw(width, height, pixels)
            .map(DynamicImage::ImageRgb8)
            .context("mozjpeg: decoded pixel buffer size did not match reported dimensions")
    }

    /// Explicit decode limits derived from the configured max source
    /// resolution: a generous, aspect-ratio-independent per-axis ceiling
    /// (real resolution enforcement is the megapixel check, run against
    /// the header before this is ever reached), and a `max_alloc` sized to
    /// the worst-case (RGBA8) decoded buffer for that resolution, with
    /// headroom for intermediate buffers some codecs need during decode.
    fn build_decode_limits(max_src_resolution_mp: u64) -> image::Limits {
        let mut limits = image::Limits::default();

        limits.max_image_width = Some(65_535);
        limits.max_image_height = Some(65_535);

        let max_pixels = max_src_resolution_mp.saturating_mul(1_000_000);
        let max_bytes = max_pixels.saturating_mul(4).saturating_mul(2);
        limits.max_alloc = Some(max_bytes.max(64 * 1024 * 1024));

        limits
    }

    /// Detect image format from magic bytes for faster decoding
    fn detect_format_from_bytes(bytes: &[u8]) -> Option<ImageFormat> {
        if bytes.len() < 12 {
            return None;
        }

        match &bytes[0..4] {
            [0xFF, 0xD8, 0xFF, _] => Some(ImageFormat::Jpeg),
            [0x89, 0x50, 0x4E, 0x47] => Some(ImageFormat::Png),
            // #49: `GIF87a`/`GIF89a` both start `GIF8` - checking only the
            // first 4 bytes (rather than all 6) is enough to disambiguate
            // from every other format hinted here, and is what lets
            // `process_image_blocking_with_limits` recognise a GIF source
            // as animatable in the first place (without this, every GIF
            // request - animated or not - would silently take the
            // guessed-format fallback path and never reach the animated
            // encode branch at all).
            [b'G', b'I', b'F', b'8'] => Some(ImageFormat::Gif),
            _ => {
                // Check for WebP
                if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                    Some(ImageFormat::WebP)
                } else if Self::is_avif(bytes) {
                    // #67: AVIF's own ISOBMFF `ftyp` box, not a 4-byte magic
                    // prefix like the formats above - see `is_avif`'s own
                    // doc comment for why this needs its own helper instead
                    // of a `match` arm on `bytes[0..4]`.
                    Some(ImageFormat::Avif)
                } else {
                    None
                }
            }
        }
    }

    /// Detects an AVIF source by its ISOBMFF `ftyp` box, mirroring
    /// `libavif`'s own `avifPeekCompatibleFileType` check (#67): bytes
    /// 4..8 must be the literal ASCII `"ftyp"` box type, and the box's
    /// major brand (bytes 8..12) or one of its compatible brands (every
    /// 4 bytes from offset 16 onward, per ISOBMFF's `FileTypeBox` layout)
    /// must be `"avif"` (still image) or `"avis"` (image sequence - not
    /// decoded any differently by `avif_decode`, which only ever reads the
    /// first image, but still recognised as AVIF rather than falling
    /// through to "unknown format").
    ///
    /// Unlike JPEG/PNG/GIF/WebP above, AVIF has no fixed-offset magic
    /// prefix at bytes `0..4` - the box's own 4-byte big-endian *size*
    /// field occupies that position instead, which varies per file - so
    /// this can't be folded into the `match &bytes[0..4]` above the way
    /// every other format is.
    /// `pub(crate)`: `avif_codec`'s own test module cross-checks this
    /// against libavif's `avifPeekCompatibleFileType` on real encoded AVIF
    /// bytes - see that module's `handler_is_avif_agrees_with_libavif_peek_compatible_file_type`.
    pub(crate) fn is_avif(bytes: &[u8]) -> bool {
        if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
            return false;
        }

        // Box size (bytes 0..4, big-endian) bounds how many compatible-brand
        // slots (4 bytes each, from offset 16) can actually be present -
        // reading past it would read into whatever data follows the ftyp
        // box in the file, not brand bytes.
        let box_size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let scan_end = box_size.min(bytes.len());

        if &bytes[8..12] == b"avif" || &bytes[8..12] == b"avis" {
            return true;
        }

        // Compatible brands: 4-byte entries starting at offset 16 (after
        // major_brand at 8..12 and minor_version at 12..16), continuing to
        // the end of the box.
        let mut offset = 16;
        while offset + 4 <= scan_end {
            if &bytes[offset..offset + 4] == b"avif" || &bytes[offset..offset + 4] == b"avis" {
                return true;
            }
            offset += 4;
        }

        false
    }

    /// Estimate output buffer size to reduce allocations
    fn estimate_output_size(img: &image::DynamicImage, format: &ImageFormat) -> usize {
        let (width, height) = img.dimensions();
        // `width * height` as plain `u32` arithmetic wraps silently in
        // release builds on overflow; casting to `usize` first and using
        // `checked_mul` avoids that regardless of how large the (now
        // separately capped) output dimensions are.
        let pixels = (width as usize)
            .checked_mul(height as usize)
            .unwrap_or(usize::MAX);

        match format {
            ImageFormat::Jpeg => pixels / 2, // Rough estimate for JPEG compression
            ImageFormat::Png => pixels.checked_mul(4).unwrap_or(usize::MAX), // RGBA
            ImageFormat::WebP => pixels / 3, // WebP compression estimate
            _ => pixels.checked_mul(3).unwrap_or(usize::MAX), // Default RGB
        }
    }
}

impl Default for ImageService {
    fn default() -> Self {
        Self::new().expect("Failed to create default ImageService")
    }
}

// Shared deterministic fixture corpus (see benches/fixtures.rs's own doc
// comment) - includes the `bomb` fixture used by the resolution-limit test
// below: tiny on disk, decodes to 10000x10000. Declared at this module's
// top level (rather than nested inside `mod tests`) because `#[path]`
// resolution for a module nested inside an inline `mod` block is relative
// to a virtual per-module-name directory that doesn't exist on disk here.
#[cfg(test)]
#[path = "../../../benches/fixtures.rs"]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    // #53: `gen_server` (OpenAPI codegen) was deleted; `ImageFormat` is now
    // hand-written in `src/models/params.rs`. Mechanical import change
    // only - no logic here changed.
    use crate::models::params::ImageFormat as ApiImageFormat;

    fn query(width: Option<u32>, height: Option<u32>) -> ResizeQuery {
        query_with_type(width, height, ResizeType::Fit)
    }

    fn query_with_type(
        width: Option<u32>,
        height: Option<u32>,
        resize_type: ResizeType,
    ) -> ResizeQuery {
        ResizeQuery {
            url: "https://images.example.com/photo.jpg".to_string(),
            width,
            height,
            resize_type,
            format: ApiImageFormat::Jpg,
            ..Default::default()
        }
    }

    #[test]
    fn output_dimensions_within_limits_pass() {
        let config = PerformanceConfig::default();
        assert!(
            ImageService::check_output_dimensions(&query(Some(800), Some(600)), &config).is_ok()
        );
        assert!(ImageService::check_output_dimensions(&query(None, None), &config).is_ok());
    }

    #[test]
    fn output_width_over_limit_is_rejected() {
        let config = PerformanceConfig::default();
        let err =
            ImageService::check_output_dimensions(&query(Some(5000), None), &config).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("too large"));
    }

    #[test]
    fn output_height_over_limit_is_rejected() {
        let config = PerformanceConfig::default();
        let err =
            ImageService::check_output_dimensions(&query(None, Some(5000)), &config).unwrap_err();
        assert!(err.to_string().to_lowercase().contains("too large"));
    }

    #[test]
    fn source_resolution_within_limit_passes() {
        assert!(ImageService::check_source_resolution(1920, 1080, 50).is_ok());
    }

    #[test]
    fn source_resolution_over_limit_is_rejected() {
        // 10000x10000 == 100 MP, over the default 50 MP cap - this is
        // exactly the shape of the `bomb` fixture in benches/fixtures.rs.
        let err = ImageService::check_source_resolution(10_000, 10_000, 50).unwrap_err();
        let msg = err.to_string().to_lowercase();
        assert!(msg.contains("too large"));
    }

    #[test]
    fn decompression_bomb_fixture_is_rejected_before_full_decode() {
        let bytes = fixtures::bomb();
        let config = PerformanceConfig::default(); // 50 MP cap
        let params = query(Some(100), Some(100));

        let result = ImageService::process_image_blocking_with_limits(&bytes, &params, &config);
        let err = result.expect_err("10000x10000 source should be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("too large"),
            "expected a resolution-too-large error, got: {msg}"
        );
    }

    #[test]
    fn estimate_output_size_basic_correctness() {
        // `estimate_output_size` now runs its width*height multiplication
        // via `checked_mul` on `usize` (not plain `u32`, which wrapped
        // silently on overflow in release builds) - this just pins down
        // that the change didn't alter the estimate for an ordinary,
        // nowhere-near-overflow image. Deliberately not exercised at
        // overflow-triggering dimensions here: doing so would require
        // actually allocating a multi-gigabyte `DynamicImage` buffer,
        // which is exactly the kind of unbounded allocation #26 exists to
        // prevent - the checked_mul path itself is reviewable at the call
        // site instead.
        let img = image::DynamicImage::new_rgb8(1, 1);
        let size = ImageService::estimate_output_size(&img, &ImageFormat::Png);
        assert_eq!(size, 4);
    }

    // ---- #63 stage 2: mozjpeg DCT-scaled decode ----

    /// `mozjpeg_scaled_dimension` must reproduce libjpeg's own
    /// `jdiv_round_up(dim * scale_num, 8)` ceiling formula exactly, or the
    /// scale planner in `select_jpeg_dct_scale` could mispredict what
    /// `Decompress::scale` actually produces.
    #[test]
    fn mozjpeg_scaled_dimension_matches_libjpeg_ceil_formula() {
        // 1/8 scale of 3840 is an exact 480, no rounding involved.
        assert_eq!(ImageService::mozjpeg_scaled_dimension(3840, 1), 480);
        // 1/4 and 1/2 of the same source.
        assert_eq!(ImageService::mozjpeg_scaled_dimension(3840, 2), 960);
        assert_eq!(ImageService::mozjpeg_scaled_dimension(3840, 4), 1920);
        // Full resolution (8/8) is a no-op.
        assert_eq!(ImageService::mozjpeg_scaled_dimension(3840, 8), 3840);
        // A dimension libjpeg's ceiling rounds up rather than truncates:
        // 1000 * 1 / 8 = 125.0 exactly, but 1001 * 1 / 8 = 125.125, which
        // must round *up* to 126, not truncate to 125.
        assert_eq!(ImageService::mozjpeg_scaled_dimension(1001, 1), 126);
        // Degenerate 1px source must never round down to 0.
        assert_eq!(ImageService::mozjpeg_scaled_dimension(1, 1), 1);
    }

    /// For a large source and a small target, `select_jpeg_dct_scale` must
    /// pick the most aggressive (smallest numerator) scale that still meets
    /// the target - this is the entire performance win of #63 stage 2.
    #[test]
    fn select_jpeg_dct_scale_picks_most_aggressive_safe_reduction() {
        // 3840x2160 (4K) -> 200x113 thumbnail: even 1/8 scale (480x270)
        // comfortably covers the target, so the most aggressive scale wins.
        assert_eq!(ImageService::select_jpeg_dct_scale(3840, 2160, 200, 113), 1);

        // A target that only 1/4 scale (960x540), not 1/8 (480x270), can
        // cover.
        assert_eq!(ImageService::select_jpeg_dct_scale(3840, 2160, 700, 400), 2);

        // A target only 1/2 scale (1920x1080) can cover.
        assert_eq!(
            ImageService::select_jpeg_dct_scale(3840, 2160, 1800, 1000),
            4
        );

        // A target close to full source resolution - no scale below full
        // (8) is safe.
        assert_eq!(
            ImageService::select_jpeg_dct_scale(3840, 2160, 3800, 2140),
            8
        );

        // No resize at all (target == source, `effective_resize_target`'s
        // `(None, None)` case) must never scale down.
        assert_eq!(
            ImageService::select_jpeg_dct_scale(3840, 2160, 3840, 2160),
            8
        );
    }

    /// Property check standing in for an exhaustive one: across a spread of
    /// source/target combinations, the scale `select_jpeg_dct_scale` picks
    /// must never decode smaller than the target in either axis - the exact
    /// safety requirement #63 stage 2 depends on (decoding below target
    /// would force `fast_image_resize` to upscale back up before its final
    /// downscale, destroying quality for no benefit).
    #[test]
    fn select_jpeg_dct_scale_never_decodes_below_target() {
        let sources = [(320, 240), (1920, 1080), (3840, 2160), (7000, 5000)];
        let target_fractions = [1, 2, 3, 5, 7, 8];

        for (raw_w, raw_h) in sources {
            for &num in &target_fractions {
                let target_w = raw_w * num / 8;
                let target_h = raw_h * num / 8;
                let scale = ImageService::select_jpeg_dct_scale(raw_w, raw_h, target_w, target_h);

                let scaled_w = ImageService::mozjpeg_scaled_dimension(raw_w, scale);
                let scaled_h = ImageService::mozjpeg_scaled_dimension(raw_h, scale);

                assert!(
                    scaled_w >= target_w && scaled_h >= target_h,
                    "source {raw_w}x{raw_h}, target {target_w}x{target_h}: chosen scale {scale}/8 \
                     decoded to {scaled_w}x{scaled_h}, which is smaller than the target in at \
                     least one axis"
                );
            }
        }
    }

    /// `effective_resize_target` must mirror the real resize stage's own
    /// `Fit`/`Fill` target math (`resize_dimensions` with `fill: false`/
    /// `true` respectively) - this pins the two together so they can't
    /// silently drift, which is the entire correctness argument for using
    /// this function to plan a DCT scale ahead of decode.
    #[test]
    fn effective_resize_target_matches_resize_dimensions_for_fit_and_fill() {
        let params_fit = query_with_type(Some(800), Some(600), ResizeType::Fit);
        assert_eq!(
            ImageService::effective_resize_target(1920, 1080, &params_fit),
            ImageService::resize_dimensions(1920, 1080, 800, 600, false)
        );

        let params_fill = query_with_type(Some(800), Some(600), ResizeType::Fill);
        assert_eq!(
            ImageService::effective_resize_target(1920, 1080, &params_fill),
            ImageService::resize_dimensions(1920, 1080, 800, 600, true)
        );

        // `Force` ignores aspect ratio entirely - exact target, not a
        // `resize_dimensions` call.
        let params_force = query_with_type(Some(800), Some(600), ResizeType::Force);
        assert_eq!(
            ImageService::effective_resize_target(1920, 1080, &params_force),
            (800, 600)
        );

        // No resize requested - the full source is the target, so no DCT
        // scale should ever be selected against it.
        let params_none = query(None, None);
        assert_eq!(
            ImageService::effective_resize_target(1920, 1080, &params_none),
            (1920, 1080)
        );
    }

    /// The #36 upscale guard must still apply inside the DCT-scale planner:
    /// requesting an output larger than the source, with `enlarge` left at
    /// its default `false`, must predict a target capped at the source
    /// resolution - never a target that would make `select_jpeg_dct_scale`
    /// think a smaller-than-source decode is unsafe when it's actually fine.
    #[test]
    fn effective_resize_target_honours_the_upscale_guard() {
        let params = query(Some(5000), Some(5000));
        assert_eq!(
            ImageService::effective_resize_target(1920, 1080, &params),
            (1920, 1080),
            "expected the target capped at the source resolution, matching the #36 guard"
        );
    }

    /// `mozjpeg_decode` wraps the actual libjpeg-turbo call in
    /// `catch_unwind` because mozjpeg's error manager unwinds (panics) on a
    /// fatal libjpeg error rather than returning `Err` (see that function's
    /// doc comment) - bytes with no valid JPEG SOI marker at all trip
    /// exactly this path (libjpeg's `read_markers` calls `ERREXIT` for "Not
    /// a JPEG file" unconditionally, not gated behind the `require_image`
    /// flag `Decompress::read_header` otherwise relies on). This must come
    /// back as a normal `Err`, not tear down the test process - proving the
    /// safety net #4 depends on (graceful fallback rather than a panic
    /// escaping past `decode_jpeg_scaled`) actually works, not just that it
    /// compiles.
    #[test]
    fn mozjpeg_decode_returns_err_instead_of_panicking_on_non_jpeg_bytes() {
        let garbage = vec![0u8; 256];
        let result = ImageService::mozjpeg_decode(&garbage, 8);
        assert!(
            result.is_err(),
            "expected a graceful Err for non-JPEG bytes, not a panic or success"
        );
    }

    /// End-to-end: a corrupt-but-JPEG-tagged source (valid SOI marker so
    /// `detect_format_from_bytes` picks the JPEG path, garbage after it)
    /// must still surface as a clean `Err` from the full pipeline entry
    /// point - covering both `decode_jpeg_scaled`'s own early failure (its
    /// header-only `image`-crate read also can't parse this) and, via
    /// `decode_with_limits`, the fallback to `decode_with_image_crate`,
    /// which fails the same way. Neither failure should panic the calling
    /// thread.
    #[test]
    fn corrupt_jpeg_tagged_source_fails_cleanly_through_the_full_pipeline() {
        let mut bytes = vec![0xFFu8, 0xD8, 0xFF, 0xE0]; // valid JPEG SOI + APP0 marker start
        bytes.extend(std::iter::repeat_n(0u8, 64)); // garbage instead of a real header
        let config = PerformanceConfig::default();
        let params = query(Some(100), Some(100));

        let result = ImageService::process_image_blocking_with_limits(&bytes, &params, &config);
        assert!(
            result.is_err(),
            "expected a clean error for a corrupt JPEG-tagged source, not a panic or success"
        );
    }

    /// #63 stage 2's actual target scenario: a large (4K) source downscaled
    /// to a small thumbnail. This is exactly the shape of request that
    /// should select an aggressive DCT scale (see
    /// `select_jpeg_dct_scale_picks_most_aggressive_safe_reduction`) and
    /// decode through mozjpeg rather than the full-resolution fallback -
    /// asserted here indirectly, through the one thing that must never be
    /// wrong regardless of which decoder produced the pixels: the final
    /// output dimensions, which must match what `resize_dimensions` (the
    /// same aspect-ratio math the non-scaled path already uses) predicts.
    #[test]
    fn large_downscale_through_mozjpeg_produces_correct_output_dimensions() {
        let bytes = fixtures::photo_like_sized(3840, 2160, ImageFormat::Jpeg);
        let config = PerformanceConfig::default();
        let params = query_with_type(Some(200), Some(113), ResizeType::Fit);

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing a large downscale should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        let expected = ImageService::resize_dimensions(3840, 2160, 200, 113, false);
        assert_eq!(
            decoded.dimensions(),
            expected,
            "expected the same Fit target dimensions the non-scaled path would produce"
        );
    }

    /// #36: requesting a much larger output than a tiny source, with
    /// `enlarge` left at its default (`false`), must not upscale - the
    /// per-axis guard in `process_image_blocking_with_limits` caps the
    /// effective target dimensions at the source's, so the decoded output
    /// dimensions must never exceed `fixtures::TINY_SIZE`.
    #[test]
    fn upscale_refused_by_default() {
        let bytes = fixtures::tiny(); // 64x64
        let config = PerformanceConfig::default();
        let params = query(Some(1000), Some(1000));
        assert!(!params.enlarge, "test assumes enlarge defaults to false");

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing a valid small source should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        let (width, height) = decoded.dimensions();
        assert!(
            width <= fixtures::TINY_SIZE && height <= fixtures::TINY_SIZE,
            "expected output capped at the {0}x{0} source, got {width}x{height}",
            fixtures::TINY_SIZE
        );
    }

    /// #36: the same oversized request against the same tiny source, but
    /// with `enlarge: true`, must be allowed to upscale to the requested
    /// size - proving the guard is an opt-in gate, not an unconditional cap.
    #[test]
    fn upscale_allowed_with_enlarge() {
        let bytes = fixtures::tiny(); // 64x64
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            enlarge: true,
            ..query(Some(200), Some(200))
        };

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing with enlarge=true should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        assert_eq!(
            decoded.dimensions(),
            (200, 200),
            "enlarge=true should honor the requested (larger than source) output size"
        );
    }

    /// #59: `fit` scales to fit *inside* the box, preserving aspect ratio -
    /// neither output dimension exceeds the requested one. 1920x1080
    /// (16:9) fitted into 800x600 is width-constrained
    /// (800/1920 < 600/1080), so the result is 800x450, not 800x600 -
    /// exactly the case #59's bug report measured against imgproxy
    /// (`rs:fit:800:600` was silently cropped to 800x600 before this fix).
    #[test]
    fn fit_scales_to_fit_inside_the_box_preserving_aspect_ratio() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let params = query_with_type(Some(800), Some(600), ResizeType::Fit);

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        assert_eq!(decoded.dimensions(), (800, 450));
    }

    /// #59: `fill` scales to *cover* the box, preserving aspect ratio, then
    /// crops the overflow - output is always exactly the requested box.
    /// This was (and remains) the crate's pre-#59 always-on behaviour for
    /// `(Some(w), Some(h))`.
    #[test]
    fn fill_crops_to_exactly_the_requested_box() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let params = query_with_type(Some(800), Some(600), ResizeType::Fill);

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        assert_eq!(decoded.dimensions(), (800, 600));
    }

    /// #59: `force` stretches to exactly the requested box, ignoring
    /// aspect ratio entirely - same output *dimensions* as `fill` for this
    /// box, but different pixel content (uniform stretch vs. crop), so the
    /// encoded bytes must differ even though both decode to 800x600.
    #[test]
    fn force_stretches_ignoring_aspect_ratio_and_differs_from_fill() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let fill_params = query_with_type(Some(800), Some(600), ResizeType::Fill);
        let force_params = query_with_type(Some(800), Some(600), ResizeType::Force);

        let (fill_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &fill_params, &config)
                .expect("fill processing should succeed");
        let (force_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &force_params, &config)
                .expect("force processing should succeed");

        assert_eq!(
            image::load_from_memory(&fill_output)
                .expect("fill output should decode")
                .dimensions(),
            (800, 600)
        );
        assert_eq!(
            image::load_from_memory(&force_output)
                .expect("force output should decode")
                .dimensions(),
            (800, 600)
        );
        assert_ne!(
            fill_output, force_output,
            "force (uniform stretch) must produce different bytes than fill (crop), \
             even though both decode to the same 800x600 dimensions"
        );
    }

    /// #59: the same four resize types through a portrait (9:16) source
    /// rather than the landscape fixture above, guarding against an
    /// implementation that only handles the width-constrained axis
    /// correctly. `auto` here takes the `fill` path because both the
    /// 1080x1920 source and the 600x800 box are portrait (same
    /// orientation) - see
    /// <https://docs.imgproxy.net/usage/processing#resizing-type>.
    #[test]
    fn portrait_source_through_every_resize_type() {
        let bytes = fixtures::photo_like_sized(1080, 1920, ImageFormat::Jpeg); // portrait 9:16
        let config = PerformanceConfig::default();

        let process = |resize_type: ResizeType| {
            let params = query_with_type(Some(600), Some(800), resize_type);
            let (output, _content_type) =
                ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                    .unwrap_or_else(|e| panic!("{resize_type:?} processing should succeed: {e}"));
            image::load_from_memory(&output)
                .expect("output should decode")
                .dimensions()
        };

        assert_eq!(
            process(ResizeType::Fit),
            (450, 800),
            "fit: height-constrained fit into 600x800"
        );
        assert_eq!(
            process(ResizeType::Fill),
            (600, 800),
            "fill: crop to exactly the box"
        );
        assert_eq!(
            process(ResizeType::Force),
            (600, 800),
            "force: stretch to exactly the box"
        );
        assert_eq!(
            process(ResizeType::Auto),
            (600, 800),
            "auto: same orientation as source -> fill"
        );
    }

    /// #59: imgproxy's documented `auto` rule - "if both source and
    /// resulting dimensions have the same orientation (portrait or
    /// landscape), imgproxy will use `fill`. Otherwise, it will use
    /// `fit`." (<https://docs.imgproxy.net/usage/processing#resizing-type>).
    /// Both branches, proven against the same landscape source: a
    /// landscape box (matching orientation) takes the `fill` path; a
    /// portrait box (mismatched orientation) takes the `fit` path.
    #[test]
    fn auto_uses_fill_or_fit_depending_on_matching_orientation() {
        let bytes = fixtures::photo_like(); // 1920x1080, landscape
        let config = PerformanceConfig::default();

        // Landscape box, same orientation as the source -> fill (crop to
        // exactly the box).
        let same_orientation = query_with_type(Some(800), Some(600), ResizeType::Auto);
        let (output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &same_orientation, &config)
                .expect("processing should succeed");
        assert_eq!(
            image::load_from_memory(&output)
                .expect("output should decode")
                .dimensions(),
            (800, 600),
            "auto with matching (landscape) orientation should behave like fill"
        );

        // Portrait box, mismatched orientation -> fit (scale to fit
        // inside, no crop).
        let mismatched_orientation = query_with_type(Some(480), Some(800), ResizeType::Auto);
        let (output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &mismatched_orientation,
            &config,
        )
        .expect("processing should succeed");
        assert_eq!(
            image::load_from_memory(&output)
                .expect("output should decode")
                .dimensions(),
            (480, 270),
            "auto with mismatched orientation should behave like fit"
        );
    }

    /// #59: a lone width or height must keep resizing aspect-ratio-
    /// preserving exactly as before #59, regardless of `resize_type` - the
    /// type only matters once both dimensions are present.
    #[test]
    fn single_dimension_resize_is_unaffected_by_resize_type() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();

        for resize_type in [
            ResizeType::Fit,
            ResizeType::Fill,
            ResizeType::Force,
            ResizeType::Auto,
        ] {
            let params = query_with_type(Some(800), None, resize_type);
            let (output, _) =
                ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                    .unwrap_or_else(|e| panic!("{resize_type:?} should succeed: {e}"));
            let dims = image::load_from_memory(&output)
                .expect("output should decode")
                .dimensions();
            assert_eq!(
                dims,
                (800, 450),
                "width-only resize with type {resize_type:?}"
            );
        }
    }

    /// #59/#36: the upscale guard must hold for every resize type, not
    /// just the historical always-fill behaviour - each per-axis effective
    /// dimension is capped at the source's own regardless of which resize
    /// type ultimately consumes it, so none of them can enlarge past the
    /// source unless `enlarge` is set.
    #[test]
    fn upscale_refused_by_default_for_every_resize_type() {
        let bytes = fixtures::tiny(); // 64x64
        let config = PerformanceConfig::default();

        for resize_type in [
            ResizeType::Fit,
            ResizeType::Fill,
            ResizeType::Force,
            ResizeType::Auto,
        ] {
            let params = query_with_type(Some(1000), Some(1000), resize_type);
            let (output, _) =
                ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                    .unwrap_or_else(|e| panic!("{resize_type:?} should succeed: {e}"));
            let (width, height) = image::load_from_memory(&output)
                .expect("output should decode")
                .dimensions();
            assert!(
                width <= fixtures::TINY_SIZE && height <= fixtures::TINY_SIZE,
                "{resize_type:?}: expected output capped at the {0}x{0} source, got {width}x{height}",
                fixtures::TINY_SIZE
            );
        }
    }

    /// #30: with the processing semaphore fully saturated (zero permits),
    /// every call must be shed immediately with an error naming "permit" -
    /// the exact substring `AppError::classify_resize_error`
    /// (`src/modules/utils/err.rs`, owned separately) maps to `503 Service
    /// Unavailable` - rather than queueing behind the empty pool
    /// indefinitely. Deterministic (no timing dependency): zero permits
    /// means `try_acquire_owned` fails on every call, unconditionally.
    #[tokio::test]
    async fn processing_saturated_at_zero_permits_sheds_every_call() {
        let config = PerformanceConfig {
            max_concurrent_processing: 0,
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();
        let bytes = Bytes::from(fixtures::tiny());
        let params = query(Some(32), Some(32));

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.process_image(&bytes, &params),
        )
        .await
        .expect("a shed call must return immediately, not hang");

        let err = result.expect_err("zero permits must reject every processing call");
        assert!(
            err.to_string().to_lowercase().contains("permit"),
            "expected a 'permit' error (maps to 503), got: {err}"
        );
    }

    /// #30: under genuine concurrent load with a single permit, some calls
    /// must succeed and at least one must be shed rather than every call
    /// queueing up and eventually succeeding - proving the semaphore
    /// actually bounds concurrency instead of merely being threaded through
    /// unused (the state before this change).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_limit_sheds_excess_requests_under_real_load() {
        let config = PerformanceConfig {
            max_concurrent_processing: 1,
            ..PerformanceConfig::default()
        };
        let service = Arc::new(ImageService::with_config(config).unwrap());
        // A real-sized image so processing takes long enough for concurrent
        // callers to actually contend on the single permit, rather than
        // each finishing before the next one is even scheduled.
        let bytes = Bytes::from(fixtures::photo_like());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let service = Arc::clone(&service);
            let bytes = bytes.clone();
            handles.push(tokio::spawn(async move {
                let params = query(Some(640), Some(480));
                service.process_image(&bytes, &params).await
            }));
        }

        let mut successes = 0;
        let mut shed = 0;
        for handle in handles {
            match handle.await.expect("spawned task should not panic") {
                Ok(_) => successes += 1,
                Err(err) if err.to_string().to_lowercase().contains("permit") => shed += 1,
                Err(err) => panic!("unexpected error: {err}"),
            }
        }

        assert!(successes >= 1, "expected at least one request to succeed");
        assert!(
            shed >= 1,
            "expected at least one of 8 concurrent requests to be shed with only 1 permit \
             available (got {successes} successes, {shed} shed) - the semaphore does not \
             appear to be bounding concurrency"
        );
    }

    /// #31: the `Bytes`-threaded path (`ImageService::process_image`, used
    /// in production) must produce byte-identical output to the plain
    /// `&[u8]` path (`ImageService::process_image_blocking`, used by
    /// benches) for the same input - proving the `Vec<u8>` -> `Bytes`
    /// return-type change and the `Bytes::copy_from_slice` removal didn't
    /// alter what gets encoded.
    #[tokio::test]
    async fn bytes_path_produces_identical_output_to_slice_path() {
        let raw = fixtures::photo_like();
        let params = query(Some(300), Some(200));

        let (expected_bytes, expected_content_type) =
            ImageService::process_image_blocking(&raw, &params).expect("slice path");

        let service = ImageService::with_config(PerformanceConfig::default()).unwrap();
        let bytes = Bytes::from(raw);
        let (actual_bytes, actual_content_type) = service
            .process_image(&bytes, &params)
            .await
            .expect("bytes path");

        assert_eq!(actual_content_type, expected_content_type);
        assert_eq!(
            actual_bytes, expected_bytes,
            "Bytes-threaded path must produce identical output to the slice path"
        );
    }

    /// #22: a chunked-transfer-encoded response (no `Content-Length` at
    /// all, which is what the previous `content_length()`-only check
    /// missed entirely) offering far more than `max_image_size` must be
    /// rejected once the running total crosses the cap, not after the
    /// whole body is read. Proven two ways: the returned error, and a tight
    /// wall-clock bound - the origin trickles far more data than the cap
    /// with a small delay per chunk, so a client that (incorrectly)
    /// buffered the whole body first would take far longer than this
    /// bound to return.
    #[tokio::test]
    async fn streaming_cap_aborts_on_chunked_oversized_body_without_content_length() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };

            // Don't bother parsing the request properly - just drain until
            // the end of the request headers.
            let mut buf = [0u8; 1024];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") => break,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }

            let header = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/octet-stream\r\n\r\n";
            if socket.write_all(header.as_bytes()).await.is_err() {
                return;
            }

            // Deliberately no Content-Length header above - that's exactly
            // the bypass #22 closes. Offer ~8MB total, far more than the
            // 64KB cap configured below, with a small per-chunk delay: a
            // client that streams-and-aborts returns almost immediately, a
            // client that buffers the whole body first would take seconds.
            let chunk = vec![0u8; 4096];
            for _ in 0..2000 {
                let hex_len = format!("{:x}\r\n", chunk.len());
                if socket.write_all(hex_len.as_bytes()).await.is_err() {
                    return;
                }
                if socket.write_all(&chunk).await.is_err() {
                    return;
                }
                if socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });

        let config = PerformanceConfig {
            max_image_size: 64 * 1024,             // 64KB - far under the ~8MB on offer
            allow_loopback_source_addresses: true, // this test's origin is 127.0.0.1
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let url = format!("http://{addr}/big.bin");
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image(&url),
        )
        .await
        .expect("download_image should not hang");
        let elapsed = started.elapsed();

        let err = result.expect_err("oversized chunked body must be rejected");
        assert!(
            err.to_string().to_lowercase().contains("too large"),
            "unexpected error: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(800),
            "expected an early abort around the cap, took {elapsed:?} instead \
             (looks like the whole body was buffered before the cap was checked)"
        );

        server.abort();
    }

    /// #21's textbook bypass: an *allowed* origin (loopback, explicitly
    /// permitted here) 302-redirects to the cloud metadata endpoint. An
    /// allowlist or a single scheme/host check on the original URL alone
    /// would not catch this - the guard must re-validate the resolved
    /// address on every hop, including this one.
    #[tokio::test]
    async fn redirect_to_metadata_endpoint_is_rejected_even_when_origin_is_allowed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };

            let mut buf = [0u8; 1024];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") => break,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }

            let response = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let config = PerformanceConfig {
            // The *origin* is loopback, and explicitly allowed - the
            // interesting part of this test is that the redirect target
            // gets independently blocked anyway.
            allow_loopback_source_addresses: true,
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let url = format!("http://{addr}/redirect-me");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image(&url),
        )
        .await
        .expect("download_image should not hang");

        let err = result.expect_err("redirect to a blocked address must be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("blocked") || msg.contains("169.254"),
            "expected the redirect target to be rejected as blocked, got: {msg}"
        );

        server.abort();
    }

    /// #57: with no `ALLOWED_SOURCES` configured, an RFC1918 private
    /// literal is still rejected by the guard itself - the pre-#57
    /// behavior, unchanged when no allowlist opts a host in.
    #[tokio::test]
    async fn non_allowlisted_private_origin_is_still_refused() {
        let service = ImageService::with_config(PerformanceConfig::default()).unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image("http://10.255.255.1/img.png"),
        )
        .await
        .expect("must not hang");

        let err = result.expect_err("non-allowlisted RFC1918 address must be rejected");
        let rejected = err
            .downcast_ref::<source_guard::SourceRejected>()
            .expect("must be rejected by the guard itself (typed SourceRejected), not fail later");
        assert!(
            matches!(
                rejected,
                source_guard::SourceRejected::BlockedIpLiteral { .. }
            ),
            "unexpected rejection variant: {rejected:?}"
        );
    }

    /// Same as above, but with an `ALLOWED_SOURCES` configured that does
    /// *not* match this host - must still be refused, and specifically as
    /// `NotAllowlisted` (fails before the private-range check is even
    /// reached), not silently let through.
    #[tokio::test]
    async fn private_origin_not_matching_configured_allowlist_is_still_refused() {
        let config = PerformanceConfig {
            allowed_sources: Some(vec!["https://trusted.example.com/".to_string()]),
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image("http://10.255.255.1/img.png"),
        )
        .await
        .expect("must not hang");

        let err = result.expect_err("RFC1918 address not matching the allowlist must be rejected");
        let rejected = err
            .downcast_ref::<source_guard::SourceRejected>()
            .expect("must downcast to SourceRejected");
        assert!(
            matches!(
                rejected,
                source_guard::SourceRejected::NotAllowlisted { .. }
            ),
            "unexpected rejection variant: {rejected:?}"
        );
    }

    /// #57's actual fix: once `ALLOWED_SOURCES` names this exact host, the
    /// SSRF guard must let the request through instead of rejecting it at
    /// the private-range check. Proven by absence of a `SourceRejected`
    /// error rather than a live private-network connection (10.255.255.1
    /// isn't reachable from this sandbox, and shouldn't need to be for
    /// this to be a meaningful test - the guard's decision is what's under
    /// test, not the TCP layer beneath it) - before this fix,
    /// `fetch_validated` would reject this before ever attempting the
    /// network call; after the fix, any failure here is a *connection*
    /// failure, not a guard rejection.
    #[tokio::test]
    async fn allowlisted_private_origin_passes_the_guard_instead_of_being_blocked() {
        let config = PerformanceConfig {
            allowed_sources: Some(vec!["http://10.255.255.1/".to_string()]),
            http_timeout: std::time::Duration::from_millis(500),
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image("http://10.255.255.1/img.png"),
        )
        .await
        .expect("must not hang");

        let err = result.expect_err(
            "connection to an address this sandbox can't route to should still fail overall",
        );
        assert!(
            err.downcast_ref::<source_guard::SourceRejected>().is_none(),
            "expected the request to pass the SSRF guard and fail at the network/connect layer \
             instead of being rejected by the guard, got: {err}"
        );
    }

    /// #57 requirement: an allowlisted origin that redirects to a
    /// *different*, non-allowlisted private address must still be
    /// refused. The allowlist match is re-evaluated per hop
    /// (`fetch_validated` recomputes it from `current` every loop
    /// iteration) - it does not "stick" once granted for the first hop.
    #[tokio::test]
    async fn allowlisted_origin_redirecting_to_non_allowlisted_private_ip_is_refused() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };

            let mut buf = [0u8; 1024];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") => break,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }

            // Redirects to an RFC1918 address that is NOT in this test's
            // ALLOWED_SOURCES below.
            let response = "HTTP/1.1 302 Found\r\nLocation: http://10.0.0.9/secret\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let config = PerformanceConfig {
            // Only the origin itself is allowlisted - the redirect target
            // (10.0.0.9) is a different host and must not inherit the
            // origin's private-range bypass.
            allowed_sources: Some(vec![format!("http://{addr}/")]),
            allow_loopback_source_addresses: true, // the origin is loopback, unrelated to the private-range bypass under test
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let url = format!("http://{addr}/redirect-me");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image(&url),
        )
        .await
        .expect("download_image should not hang");

        let err =
            result.expect_err("redirect to a non-allowlisted private address must be rejected");
        let rejected = err
            .downcast_ref::<source_guard::SourceRejected>()
            .expect("must downcast to SourceRejected");
        assert!(
            matches!(
                rejected,
                source_guard::SourceRejected::NotAllowlisted { .. }
            ),
            "unexpected rejection variant: {rejected:?}"
        );

        server.abort();
    }

    /// #57 requirement, strongest form: even if an operator's
    /// `ALLOWED_SOURCES` happens to *also* list the metadata endpoint
    /// itself (so the allowlist match on that hop succeeds), the
    /// private-range bypass must never reach it - link-local stays gated
    /// behind its own separate `allow_link_local_source_addresses` flag,
    /// which is untouched here. This is the exact bypass #21 closed;
    /// #57's allowlist change must not reopen it.
    #[tokio::test]
    async fn redirect_to_metadata_endpoint_is_rejected_even_when_it_matches_the_allowlist() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };

            let mut buf = [0u8; 1024];
            loop {
                match socket.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") => break,
                    Ok(_) => continue,
                    Err(_) => return,
                }
            }

            let response = "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/latest/meta-data/\r\nContent-Length: 0\r\n\r\n";
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let config = PerformanceConfig {
            // Deliberately allowlists BOTH the origin AND the metadata
            // endpoint itself - even so, the metadata endpoint must stay
            // blocked, because allow_private never lifts the link-local
            // check.
            allowed_sources: Some(vec![
                format!("http://{addr}/"),
                "http://169.254.169.254/".to_string(),
            ]),
            allow_loopback_source_addresses: true,
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let url = format!("http://{addr}/redirect-me");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image(&url),
        )
        .await
        .expect("download_image should not hang");

        let err = result.expect_err("redirect to the metadata endpoint must be rejected");
        let rejected = err
            .downcast_ref::<source_guard::SourceRejected>()
            .expect("must downcast to SourceRejected");
        assert!(
            matches!(
                rejected,
                source_guard::SourceRejected::BlockedIpLiteral { .. }
            ),
            "unexpected rejection variant: {rejected:?}"
        );

        server.abort();
    }

    /// A same-host redirect loop must be cut off at `max_redirects`,
    /// rather than following forever.
    #[tokio::test]
    async fn redirect_loop_is_cut_off_at_max_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            // Every request gets redirected right back to the same path -
            // an infinite loop if not bounded.
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };

                let mut buf = [0u8; 1024];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) if buf[..n].windows(4).any(|w| w == b"\r\n\r\n") => break,
                        Ok(_) => continue,
                        Err(_) => return,
                    }
                }

                let response = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://{addr}/loop\r\nContent-Length: 0\r\n\r\n"
                );
                if socket.write_all(response.as_bytes()).await.is_err() {
                    return;
                }
            }
        });

        let config = PerformanceConfig {
            allow_loopback_source_addresses: true,
            max_redirects: 3,
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let url = format!("http://{addr}/loop");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            service.download_image(&url),
        )
        .await
        .expect("a bounded redirect loop should not hang");

        let err = result.expect_err("an infinite redirect loop must be cut off");
        assert!(
            err.to_string()
                .to_lowercase()
                .contains("too many redirects"),
            "unexpected error: {err}"
        );

        server.abort();
    }

    // ---- #34/#60: alpha flattening and transparent-pixel normalisation ----

    /// Builds a `ResizeQuery` requesting no resize at all (keeps the
    /// fixture's original pixel grid so boundary assertions below land on
    /// exact, known pixel coordinates instead of whatever a resize filter's
    /// interpolation produces near the edge) with the given output format
    /// and `background`.
    fn query_for_alpha(format: ApiImageFormat, background: Option<[u8; 3]>) -> ResizeQuery {
        ResizeQuery {
            url: "https://images.example.com/alpha.png".to_string(),
            width: None,
            height: None,
            resize_type: ResizeType::Fit,
            format,
            background,
            ..Default::default()
        }
    }

    /// #34: pure unit test of the compositing formula itself, independent
    /// of any lossy encoding - `alpha == 255` must reproduce the source
    /// exactly, `alpha == 0` must land exactly on `background` (not
    /// whatever garbage RGB the source carried there), and partial
    /// transparency must genuinely blend rather than equal either input.
    #[test]
    fn flatten_onto_background_composites_alpha_correctly() {
        let mut img = image::RgbaImage::new(1, 3);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 255])); // opaque
        img.put_pixel(0, 1, image::Rgba([10, 20, 30, 0])); // fully transparent, garbage rgb
        img.put_pixel(0, 2, image::Rgba([0, 0, 0, 128])); // ~50% transparent, black

        let dynamic = DynamicImage::ImageRgba8(img);
        let background = [255, 255, 255];
        let flattened = ImageService::flatten_onto_background(&dynamic, background).to_rgb8();

        assert_eq!(
            flattened.get_pixel(0, 0).0,
            [10, 20, 30],
            "opaque pixel must pass through unchanged"
        );
        assert_eq!(
            flattened.get_pixel(0, 1).0,
            background,
            "fully transparent pixel must land exactly on the background, not its garbage RGB"
        );

        let blended = flattened.get_pixel(0, 2).0;
        assert_ne!(blended, [0, 0, 0], "must not equal the source colour");
        assert_ne!(blended, background, "must not equal the background either");
        // alpha = 128/255 ~= 0.502; out = 0*0.502 + 255*0.498 ~= 127.
        for channel in blended {
            assert!(
                (120..=135).contains(&channel),
                "expected the 50%-alpha blend near 127, got {blended:?}"
            );
        }
    }

    /// #60: only pixels with exactly `alpha == 0` are rewritten - partial
    /// transparency and full opacity must be byte-for-byte untouched, since
    /// their RGB is actually visible (blended with whatever is behind them
    /// for partial alpha, directly for opaque).
    #[test]
    fn normalize_transparent_pixels_only_rewrites_fully_transparent_pixels() {
        let mut img = image::RgbaImage::new(1, 3);
        img.put_pixel(0, 0, image::Rgba([10, 20, 30, 255])); // opaque
        img.put_pixel(0, 1, image::Rgba([99, 88, 77, 0])); // fully transparent, garbage rgb
        img.put_pixel(0, 2, image::Rgba([1, 2, 3, 128])); // partial

        let background = [255, 0, 0];
        let normalized = ImageService::normalize_transparent_pixels(img, background);

        assert_eq!(normalized.get_pixel(0, 0).0, [10, 20, 30, 255]);
        assert_eq!(
            normalized.get_pixel(0, 1).0,
            [background[0], background[1], background[2], 0],
            "fully transparent pixel's RGB must be rewritten to the background, alpha unchanged"
        );
        assert_eq!(
            normalized.get_pixel(0, 2).0,
            [1, 2, 3, 128],
            "partially transparent pixel must be left byte-for-byte untouched"
        );
    }

    /// #34: the alpha fixture (`fixtures::alpha`, mirrored by
    /// `bench-imgproxy/fixtures/generate.py`'s `alpha_fringe_rgba`) has a
    /// transparent border with deliberately garbage (non-zero) RGB
    /// underneath - exactly the vendored `to_rgb8()` raw-channel-drop bug
    /// the issue's source citation identified. Converting it to JPEG (no
    /// alpha channel) must flatten that border onto the default background
    /// (white, since `background` is `None` here) instead of letting the
    /// garbage show through. Golden-image style: asserts actual decoded
    /// pixel values at the boundary, not just a successful conversion.
    #[test]
    fn alpha_fixture_flattens_to_default_white_background_when_converted_to_jpeg() {
        let bytes = fixtures::alpha(); // 512x512, border width 32px
        let config = PerformanceConfig::default();
        let params = query_for_alpha(ApiImageFormat::Jpg, None);

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");
        assert_eq!(content_type, "image/jpeg");

        let decoded = image::load_from_memory_with_format(&output, ImageFormat::Jpeg)
            .expect("output should decode")
            .to_rgb8();

        // (0, 0) is deep inside the transparent border - garbage RGB
        // pre-fix, must be near-white post-fix (JPEG is lossy, so allow a
        // small tolerance rather than requiring exactly 255).
        let pixel = decoded.get_pixel(0, 0).0;
        for (channel, value) in ["r", "g", "b"].into_iter().zip(pixel) {
            assert!(
                value > 235,
                "expected boundary pixel channel {channel} near white (255), got {value} \
                 (full pixel: {pixel:?})"
            );
        }
    }

    /// #34: a caller-supplied `background` (the `bg:` processing option)
    /// must be honoured instead of the default white - proven against the
    /// same boundary pixel as the default-background test above.
    #[test]
    fn custom_background_is_honoured_when_flattening_to_jpeg() {
        let bytes = fixtures::alpha();
        let config = PerformanceConfig::default();
        let params = query_for_alpha(ApiImageFormat::Jpg, Some([0, 0, 255])); // pure blue

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let decoded = image::load_from_memory_with_format(&output, ImageFormat::Jpeg)
            .expect("output should decode")
            .to_rgb8();

        let [r, g, b] = decoded.get_pixel(0, 0).0;
        assert!(r < 40, "expected low red near a blue background, got {r}");
        assert!(g < 40, "expected low green near a blue background, got {g}");
        assert!(
            b > 200,
            "expected high blue near a blue background, got {b}"
        );
    }

    /// #60: output-size regression guard for the exact adversarial shape
    /// the issue measured. Pre-fix, the analogous corpus fixture
    /// (`bench-imgproxy/fixtures/corpus/alpha_1024.png`, same generator,
    /// same seed - see `bench-imgproxy/fixtures/generate.py`) encoded to
    /// PNG at `fill:400:300` measured 85,347 bytes. Post-fix, the in-repo
    /// 512px fixture at the same request measures 11,337 bytes - the
    /// 25,000-byte threshold asserted here gives ~2.2x headroom over that
    /// measurement (so the assertion isn't flaky against encoder-internals
    /// drift) while staying well under an order of magnitude below the
    /// pre-fix floor, so a regression that turns normalisation back into a
    /// no-op is still caught.
    #[test]
    fn alpha_fixture_encoded_to_png_stays_under_size_threshold() {
        let bytes = fixtures::alpha();
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            ..query_with_type(Some(400), Some(300), ResizeType::Fill)
        };

        let (output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        assert!(
            output.len() < 25_000,
            "expected normalised alpha PNG under 25,000 bytes, got {} \
             (pre-fix, the analogous corpus fixture measured 85,347 bytes)",
            output.len()
        );
    }

    /// #60 (the non-adversarial case the issue explicitly calls out): a
    /// flat/solid-colour PNG has no transparency at all, so this exercises
    /// only the `CompressionType::Best` PNG-encoder change, not
    /// flattening/normalisation. Measured 2,596 -> 1,063 bytes for the
    /// analogous 1024px corpus fixture; asserted here against the in-repo
    /// fixture with headroom.
    #[test]
    fn flat_colour_png_benefits_from_best_compression() {
        let bytes = fixtures::flat();
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            ..query_with_type(Some(400), Some(300), ResizeType::Fill)
        };

        let (output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        assert!(
            output.len() < 3_000,
            "expected flat-colour PNG under 3,000 bytes with Best compression, got {}",
            output.len()
        );
    }

    // ---- #35: quality wiring (JpegEncoder::new_with_quality, encode_webp's
    // quality/lossless parameters, per-format override, cache-key coverage
    // for these tested separately in `src/services/cache/handler.rs`) ----

    /// A lower JPEG quality must always produce a smaller (or equal, but in
    /// practice smaller for a real photographic fixture) output than a
    /// higher one, for the same source and dimensions - the whole point of
    /// exposing the knob at all.
    #[test]
    fn jpeg_quality_changes_output_size_monotonically() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();

        let size_at = |quality: u8| {
            let params = ResizeQuery {
                format: ApiImageFormat::Jpg,
                quality: Some(quality),
                ..query(Some(400), Some(300))
            };
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed")
                .0
                .len()
        };

        let small = size_at(30);
        let large = size_at(90);
        assert!(
            small < large,
            "expected q=30 ({small} bytes) to be smaller than q=90 ({large} bytes)"
        );
    }

    /// Same monotonicity property as `jpeg_quality_changes_output_size_monotonically`,
    /// but through the lossy-WebP path (`Self::encode_webp`'s `quality`
    /// parameter) instead of JPEG's.
    #[test]
    fn webp_quality_changes_output_size_monotonically() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();

        let size_at = |quality: u8| {
            let params = ResizeQuery {
                format: ApiImageFormat::Webp,
                quality: Some(quality),
                ..query(Some(400), Some(300))
            };
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed")
                .0
                .len()
        };

        let small = size_at(30);
        let large = size_at(90);
        assert!(
            small < large,
            "expected q=30 ({small} bytes) to be smaller than q=90 ({large} bytes)"
        );
    }

    /// `jpeg_quality` (the `fq:jpg:{quality}` per-format override) must win
    /// over the lower global `quality` - imgproxy's own documented
    /// precedence for `format_quality` over `quality`.
    #[test]
    fn format_quality_override_beats_global_quality() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let base = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let global_low = ResizeQuery {
            quality: Some(30),
            ..base.clone()
        };
        let override_high = ResizeQuery {
            quality: Some(30),
            jpeg_quality: Some(90),
            ..base
        };

        let (out_global, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &global_low, &config)
                .expect("processing should succeed");
        let (out_override, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &override_high, &config)
                .expect("processing should succeed");

        assert!(
            out_override.len() > out_global.len(),
            "expected jpeg_quality=90 ({} bytes) to override the lower global quality=30 \
             ({} bytes), producing larger output",
            out_override.len(),
            out_global.len()
        );
    }

    // ---- #76: progressive JPEG, chroma subsampling, max_bytes ----

    /// `jpgo:1` (progressive) must actually change the encoded bytes, not
    /// silently no-op - a flag that parses but does nothing is worse than
    /// no flag at all. Direction (smaller/larger) is measured separately in
    /// `benches/encode.rs` against the real corpus rather than asserted
    /// here, since the issue's "typically 2-10% smaller" claim needed
    /// verifying, not assuming.
    #[test]
    fn jpeg_progressive_option_produces_different_bytes_than_baseline() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let base = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let baseline = ResizeQuery {
            jpeg_progressive: Some(false),
            ..base.clone()
        };
        let progressive = ResizeQuery {
            jpeg_progressive: Some(true),
            ..base
        };

        let (out_baseline, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &baseline, &config)
                .expect("processing should succeed");
        let (out_progressive, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &progressive, &config)
                .expect("processing should succeed");

        assert_ne!(
            out_baseline, out_progressive,
            "jpgo:1 must produce different encoded bytes than the baseline default"
        );

        // Both must still decode to the same pixel dimensions - progressive
        // is a scan-structure change, not a resize.
        let dims_baseline = image::load_from_memory_with_format(&out_baseline, ImageFormat::Jpeg)
            .expect("baseline output should decode")
            .dimensions();
        let dims_progressive =
            image::load_from_memory_with_format(&out_progressive, ImageFormat::Jpeg)
                .expect("progressive output should decode")
                .dimensions();
        assert_eq!(dims_baseline, dims_progressive);
    }

    /// An unset `jpeg_progressive` (the common case) must resolve through
    /// `PerformanceConfig::jpeg_progressive_default` and produce output
    /// byte-identical to explicitly requesting the same default - proving
    /// the deployment-default resolution in `encode_single_image`'s JPEG
    /// branch is actually wired up, not just present in the struct.
    #[test]
    fn jpeg_progressive_unset_resolves_to_deployment_default() {
        let bytes = fixtures::photo_like();
        let config = PerformanceConfig::default(); // jpeg_progressive_default: false
        let base = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let unset = ResizeQuery {
            jpeg_progressive: None,
            ..base.clone()
        };
        let explicit_false = ResizeQuery {
            jpeg_progressive: Some(false),
            ..base
        };

        let (out_unset, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &unset, &config)
                .expect("processing should succeed");
        let (out_explicit, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &explicit_false, &config)
                .expect("processing should succeed");

        assert_eq!(
            out_unset, out_explicit,
            "unset jpeg_progressive must resolve to the same output as the deployment default"
        );
    }

    /// `jpgo:1:1` (no_subsampling, 4:4:4) must actually change the encoded
    /// bytes relative to this crate's 4:2:2 default, and - unlike
    /// progressive, whose size effect is corpus-dependent - retaining full
    /// chroma resolution can only ever cost the same or more bytes than
    /// throwing chroma detail away, for the same quality/content, so
    /// asserting the direction here (unlike progressive) is safe.
    #[test]
    fn jpeg_no_subsampling_produces_different_and_larger_output() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let base = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let subsampled = ResizeQuery {
            jpeg_no_subsampling: Some(false),
            ..base.clone()
        };
        let full_chroma = ResizeQuery {
            jpeg_no_subsampling: Some(true),
            ..base
        };

        let (out_subsampled, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &subsampled, &config)
                .expect("processing should succeed");
        let (out_full_chroma, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &full_chroma, &config)
                .expect("processing should succeed");

        assert_ne!(out_subsampled, out_full_chroma);
        assert!(
            out_full_chroma.len() >= out_subsampled.len(),
            "4:4:4 ({} bytes) should never be smaller than 4:2:2 ({} bytes) for the same \
             quality/content",
            out_full_chroma.len(),
            out_subsampled.len()
        );
    }

    /// `mb:{bytes}` must actually cap the encoded output - a real photo
    /// requested at a byte budget well under its default-quality size must
    /// come back under (or very close to) that budget, not simply ignore
    /// it. `encode_with_max_bytes`'s binary search can, in principle, not
    /// land exactly at the budget (best-effort, bounded attempts), so this
    /// asserts against a generous multiple of the budget rather than the
    /// exact number - still tight enough to catch a no-op implementation.
    #[test]
    fn max_bytes_caps_jpeg_output_size() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let unrestricted = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let (unrestricted_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &unrestricted, &config)
                .expect("processing should succeed");

        let budget = (unrestricted_output.len() / 4) as u64;
        let capped = ResizeQuery {
            max_bytes: Some(budget),
            ..unrestricted
        };

        let (capped_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &capped, &config)
                .expect("processing should succeed");

        assert!(
            capped_output.len() < unrestricted_output.len(),
            "max_bytes must actually lower quality and shrink output: capped {} bytes vs \
             unrestricted {} bytes",
            capped_output.len(),
            unrestricted_output.len()
        );
        assert!(
            (capped_output.len() as u64) <= budget * 2,
            "expected the max_bytes search to land reasonably close to the {budget}-byte \
             budget, got {} bytes",
            capped_output.len()
        );
    }

    /// `mb:0` (parsed to `None` at the URL layer, #76's "0 means unset"
    /// convention) must behave exactly like `max_bytes` never being set at
    /// all - no search, no output-size change.
    #[test]
    fn max_bytes_none_does_not_change_output() {
        let bytes = fixtures::photo_like();
        let config = PerformanceConfig::default();
        let base = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(Some(400), Some(300))
        };

        let (out_default, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &base, &config)
                .expect("processing should succeed");

        let explicit_none = ResizeQuery {
            max_bytes: None,
            ..base
        };
        let (out_explicit_none, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &explicit_none, &config)
                .expect("processing should succeed");

        assert_eq!(out_default, out_explicit_none);
    }

    /// Unit-level test of the search itself (`encode_with_max_bytes`),
    /// isolated from JPEG encoding: a synthetic "encoder" whose output size
    /// is just its quality argument lets the search's convergence and
    /// attempt-bounding be asserted precisely, including the "budget
    /// already satisfied by the first attempt costs nothing extra" and
    /// "budget unreachable even at the lowest quality" edge cases imgproxy
    /// itself documents (best-effort, not an error).
    #[test]
    fn encode_with_max_bytes_converges_within_the_attempt_bound() {
        use std::cell::Cell;

        let attempts = Cell::new(0u32);
        let encode_at = |quality: u8| -> Result<Vec<u8>> {
            attempts.set(attempts.get() + 1);
            Ok(vec![0u8; quality as usize])
        };

        let result = ImageService::encode_with_max_bytes(50, 100, encode_at).unwrap();
        assert!(
            result.len() as u64 <= 50,
            "expected the search to land at or under the 50-byte budget, got {} bytes",
            result.len()
        );
        // 1 initial attempt + at most `MAX_BYTES_SEARCH_ATTEMPTS` more.
        assert!(
            attempts.get() <= 1 + ImageService::MAX_BYTES_SEARCH_ATTEMPTS,
            "expected at most {} total encode attempts, got {}",
            1 + ImageService::MAX_BYTES_SEARCH_ATTEMPTS,
            attempts.get()
        );
    }

    /// A budget already satisfied by the caller's own requested quality
    /// must cost exactly one encode attempt - no search needed.
    #[test]
    fn encode_with_max_bytes_already_satisfied_costs_one_attempt() {
        use std::cell::Cell;

        let attempts = Cell::new(0u32);
        let encode_at = |quality: u8| -> Result<Vec<u8>> {
            attempts.set(attempts.get() + 1);
            Ok(vec![0u8; quality as usize])
        };

        let result = ImageService::encode_with_max_bytes(1000, 50, encode_at).unwrap();
        assert_eq!(result.len(), 50);
        assert_eq!(attempts.get(), 1);
    }

    /// A budget unreachable even at the lowest quality (`1`) must still
    /// return the smallest output found (best-effort), not an error -
    /// matching imgproxy's own documented behaviour.
    #[test]
    fn encode_with_max_bytes_unreachable_budget_returns_smallest_found() {
        let encode_at = |quality: u8| -> Result<Vec<u8>> { Ok(vec![0u8; quality as usize]) };

        let result = ImageService::encode_with_max_bytes(0, 100, encode_at).unwrap();
        assert_eq!(
            result.len(),
            1,
            "expected the search to bottom out at quality=1 (smallest possible) when the \
             budget is unreachable"
        );
    }

    /// Lossless WebP (`webp_lossless: Some(true)`) must round-trip the
    /// pixels the pipeline actually decoded exactly - no resize/blur/
    /// grayscale filter applied, and a source with no alpha channel so the
    /// #34/#60 flatten/normalise stage is a no-op, isolating this to purely
    /// the encoder's own lossless-ness.
    ///
    /// The reference decode must go through the same decoder the pipeline
    /// uses for this request (#67): `query(None, None)` requests no
    /// resize, so `decode_jpeg_scaled` picks `scale_num == 8` and decodes
    /// through `mozjpeg_decode`, not `image::load_from_memory`'s
    /// zune-jpeg path (that stopped being true once #67 retired the old
    /// scale_num == 8 special case - see `decode_jpeg_scaled`'s
    /// retired-rationale comment). The two decoders differ by up to 3/255
    /// per channel on real photographs - imperceptible, but enough to trip
    /// a byte-identity assertion if the reference decode used the *other*
    /// decoder than the one the pipeline actually ran. This test cares
    /// about the WebP encoder's losslessness, not about which JPEG decoder
    /// is faster, so `original` is decoded the same way the pipeline does.
    #[test]
    fn webp_lossless_round_trips_byte_identical_pixels() {
        let bytes = fixtures::photo_like(); // 1920x1080, no alpha channel
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Webp,
            webp_lossless: Some(true),
            ..query(None, None)
        };

        let (output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let original = ImageService::mozjpeg_decode(&bytes, 8)
            .expect("source should decode")
            .to_rgba8();
        let decoded = image::load_from_memory(&output)
            .expect("lossless webp output should decode")
            .to_rgba8();

        assert_eq!(decoded.dimensions(), original.dimensions());
        assert_eq!(
            decoded.as_raw(),
            original.as_raw(),
            "lossless webp round-trip must be byte-identical to the pixels the pipeline decoded"
        );
    }

    /// Lossless WebP must actually take the `encode_lossless` path, not
    /// merely happen to look identical - `lossless: Some(false)` (still
    /// lossy) on the exact same source/dimensions must produce different
    /// (and, for a real photographic fixture, larger) output, proving the
    /// flag is wired through rather than a no-op that always round-trips
    /// because e.g. quality was already 100.
    #[test]
    fn webp_lossless_differs_from_lossy_output() {
        let bytes = fixtures::photo_like(); // 1920x1080
        let config = PerformanceConfig::default();
        let base = ResizeQuery {
            format: ApiImageFormat::Webp,
            ..query(Some(400), Some(300))
        };

        let lossy = ResizeQuery {
            webp_lossless: Some(false),
            ..base.clone()
        };
        let lossless = ResizeQuery {
            webp_lossless: Some(true),
            ..base
        };

        let (out_lossy, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &lossy, &config)
                .expect("processing should succeed");
        let (out_lossless, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &lossless, &config)
                .expect("processing should succeed");

        assert_ne!(
            out_lossy.len(),
            out_lossless.len(),
            "lossy and lossless webp encodes of the same photographic source should differ in size"
        );
    }

    /// #66: a WebP *source* now decodes via `libwebp`
    /// (`decode_webp_libwebp`/`libwebp_decode`) instead of `image-webp`.
    /// Encodes a real photographic source to *lossless* WebP first (so the
    /// reference pixels are known exactly, no lossy re-encode noise to
    /// account for), feeds that WebP back in as a source, and asserts the
    /// pipeline's decode-then-re-encode-lossless round trip is
    /// byte-identical to the original pixels - proving the new libwebp
    /// decode path decodes real photographic content correctly, not just
    /// that it doesn't crash. `dssim`-based equivalence against the *old*
    /// `image-webp` decoder on the real Kodak corpus was measured
    /// separately (not an in-repo test: `dssim` is AGPL-3.0, kept out of
    /// this crate's own dependency tree for the same reason ADR 0003/0004
    /// did) - see this change's own report for that measurement (max
    /// DSSIM delta 0.00000000 across all 24 images, i.e. pixel-identical).
    #[test]
    fn webp_source_decodes_via_libwebp_correctly() {
        let photo = fixtures::photo_like(); // 1920x1080 JPEG
        let config = PerformanceConfig::default();

        // First pass: JPEG -> lossless WebP, to get a WebP source with
        // known-exact pixels (whatever the JPEG decoded to).
        let to_webp = ResizeQuery {
            format: ApiImageFormat::Webp,
            webp_lossless: Some(true),
            ..query(None, None)
        };
        let (webp_source, content_type) =
            ImageService::process_image_blocking_with_limits(&photo, &to_webp, &config)
                .expect("JPEG -> lossless WebP should succeed");
        assert_eq!(content_type, "image/webp");
        assert!(
            ImageService::detect_format_from_bytes(&webp_source) == Some(ImageFormat::WebP),
            "encoded bytes should be detected as WebP"
        );

        // Second pass: that WebP source -> lossless WebP again. If
        // `libwebp_decode` decodes it correctly, this must be an exact
        // byte-for-byte pixel round trip (both hops lossless).
        let (webp_again, _) =
            ImageService::process_image_blocking_with_limits(&webp_source, &to_webp, &config)
                .expect("WebP -> lossless WebP should succeed");

        let original = image::load_from_memory(&webp_source)
            .expect("first-pass webp should decode")
            .to_rgba8();
        let round_tripped = image::load_from_memory(&webp_again)
            .expect("second-pass webp should decode")
            .to_rgba8();

        assert_eq!(original.dimensions(), round_tripped.dimensions());
        assert_eq!(
            original.as_raw(),
            round_tripped.as_raw(),
            "libwebp-decoded WebP source, re-encoded lossless, must round-trip exactly"
        );
    }

    // ---- #33: EXIF autorotate ----

    /// `fixtures::oriented(code)`'s marker sits in the top-left
    /// `ORIENTED_MARKER`x`ORIENTED_MARKER` block of the *canonical*
    /// (already-upright) image - see that fixture's doc comment. `(5, 5)`
    /// is well inside it regardless of any JPEG-block-boundary blur from
    /// decoding the lossy source.
    fn assert_reddish(pixel: [u8; 3], context: &str) {
        let [r, g, b] = pixel;
        assert!(
            r > 150 && i32::from(r) - i32::from(b) > 40,
            "{context}: expected a reddish marker pixel, got {pixel:?} (r={r} g={g} b={b})"
        );
    }

    /// Counterpart to `assert_reddish` for the canonical image's blue
    /// background.
    fn assert_blueish(pixel: [u8; 3], context: &str) {
        let [r, g, b] = pixel;
        assert!(
            b > 150 && i32::from(b) - i32::from(r) > 40,
            "{context}: expected a blueish background pixel, got {pixel:?} (r={r} g={g} b={b})"
        );
    }

    /// #33: every one of the eight standard EXIF orientation values must
    /// decode to the same upright `ORIENTED_W`x`ORIENTED_H` layout -
    /// `fixtures::oriented`'s canonical marker (red top-left block on a
    /// blue background) landing in the same place regardless of how the
    /// source pixels were actually stored, dimensions included (5-8 swap
    /// width/height in the stored file). No resize is requested here, so
    /// this isolates autorotate itself from the resize/crop math the next
    /// test covers.
    #[test]
    fn all_eight_exif_orientations_render_upright() {
        let config = PerformanceConfig::default();

        for code in 1u8..=8 {
            let bytes = fixtures::oriented(code);
            let params = ResizeQuery {
                format: ApiImageFormat::Png,
                ..query(None, None)
            };

            let (output, _content_type) =
                ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                    .unwrap_or_else(|e| {
                        panic!("orientation {code}: processing should succeed: {e}")
                    });

            let decoded = image::load_from_memory(&output)
                .unwrap_or_else(|e| panic!("orientation {code}: output should decode: {e}"));
            assert_eq!(
                decoded.dimensions(),
                (fixtures::ORIENTED_W, fixtures::ORIENTED_H),
                "orientation {code}: expected the canonical upright dimensions"
            );

            let rgb = decoded.to_rgb8();
            assert_reddish(
                rgb.get_pixel(5, 5).0,
                &format!("orientation {code}, marker corner"),
            );
            assert_blueish(
                rgb.get_pixel(100, 70).0,
                &format!("orientation {code}, background"),
            );
        }
    }

    /// #33: the order-of-operations case the issue explicitly calls out -
    /// autorotate must be applied *before* resize, or a crop/scale composes
    /// against the wrong axes. `fixtures::oriented(6)` is stored 90-degrees
    /// rotated (80x120, portrait) under EXIF orientation 6; a `Fit` request
    /// naming only a width is aspect-ratio-preserving against whatever
    /// dimensions the resize step sees. If autorotation happened after
    /// resize (or not at all), the resize would run against the stored
    /// 80x120 portrait shape instead of the corrected 120x80 landscape one,
    /// producing the wrong output dimensions and a sideways marker.
    #[test]
    fn rotated_source_with_resize_produces_correctly_oriented_output_at_right_dimensions() {
        let bytes = fixtures::oriented(6); // stored 80x120, corrects to 120x80
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            ..query(Some(60), None) // half-scale fit, preserving the corrected 3:2 aspect
        };

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        assert_eq!(
            decoded.dimensions(),
            (60, 40),
            "expected the corrected (landscape) aspect ratio scaled to width 60, not the \
             stored (portrait) shape"
        );

        let rgb = decoded.to_rgb8();
        assert_reddish(rgb.get_pixel(3, 3).0, "resized marker corner");
        assert_blueish(rgb.get_pixel(50, 35).0, "resized background");
    }

    /// #33: `autorotate: false` must leave the image exactly as stored -
    /// the opt-out this crate's `ar:0` URL option (and imgproxy's own
    /// `IMGPROXY_AUTO_ROTATE=false`) exists for. No resize is requested, so
    /// the output dimensions must match the *stored* (portrait, un-rotated)
    /// shape, not the corrected (landscape) one the previous two tests
    /// assert.
    #[test]
    fn autorotate_disabled_leaves_image_as_stored() {
        let bytes = fixtures::oriented(6); // stored 80x120
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            autorotate: false,
            ..query(None, None)
        };

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        let decoded = image::load_from_memory(&output).expect("output should decode");
        assert_eq!(
            decoded.dimensions(),
            (fixtures::ORIENTED_H, fixtures::ORIENTED_W),
            "autorotate=false must leave the stored (portrait) dimensions untouched, not \
             apply the EXIF correction"
        );
    }

    /// #33: a source with no EXIF orientation tag at all must be completely
    /// unaffected by `autorotate` - `fixtures::photo_like` carries no Exif
    /// segment, so `decoder.orientation()` reports `NoTransforms` either
    /// way and `apply_orientation` is a no-op regardless of the flag,
    /// making output byte-for-byte identical between the two.
    #[test]
    fn images_without_orientation_tag_are_unaffected_by_autorotate() {
        let bytes = fixtures::photo_like(); // 1920x1080, no Exif segment
        let config = PerformanceConfig::default();

        let with_autorotate = query_with_type(Some(300), Some(200), ResizeType::Fit);
        let without_autorotate = ResizeQuery {
            autorotate: false,
            ..query_with_type(Some(300), Some(200), ResizeType::Fit)
        };

        let (output_on, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &with_autorotate, &config)
                .expect("autorotate=true processing should succeed");
        let (output_off, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &without_autorotate, &config)
                .expect("autorotate=false processing should succeed");

        assert_eq!(
            output_on, output_off,
            "a source with no EXIF orientation tag must produce identical output regardless \
             of autorotate"
        );
    }

    // ---- #49: AVIF output, animated GIF/WebP, content negotiation -------

    /// Builds a tiny (4x4) `frame_count`-frame animated GIF, alternating
    /// solid red/blue - not one of `benches/fixtures.rs`'s shared corpus
    /// fixtures, since none of those are animated.
    fn tiny_animated_gif(frame_count: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut buf);
            for i in 0..frame_count {
                let colour = if i % 2 == 0 {
                    image::Rgba([255, 0, 0, 255])
                } else {
                    image::Rgba([0, 0, 255, 255])
                };
                let img = image::RgbaImage::from_pixel(4, 4, colour);
                let frame =
                    image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1));
                encoder.encode_frame(frame).expect("encode gif frame");
            }
        }
        buf
    }

    /// Builds a tiny (4x4) `frame_count`-frame animated WebP the same way,
    /// via the `webp` crate's `AnimEncoder` - the exact same encoder
    /// `ImageService::encode_animated_webp` uses in production.
    fn tiny_animated_webp(frame_count: u32) -> Vec<u8> {
        let webp_config = webp::WebPConfig::new().expect("init webp config");
        let mut encoder = webp::AnimEncoder::new(4, 4, &webp_config);
        let mut buffers: Vec<Vec<u8>> = Vec::new();
        for i in 0..frame_count {
            let colour: [u8; 4] = if i % 2 == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            let mut buf = Vec::with_capacity(4 * 4 * 4);
            for _ in 0..16 {
                buf.extend_from_slice(&colour);
            }
            buffers.push(buf);
        }
        for (i, buf) in buffers.iter().enumerate() {
            encoder.add_frame(webp::AnimFrame::from_rgba(buf, 4, 4, (i as i32) * 100));
        }
        encoder.encode().to_vec()
    }

    /// #49: an animated GIF source requested as `.gif` must stay animated -
    /// every frame resized and re-encoded, not just the first one.
    #[test]
    fn animated_gif_source_to_gif_output_preserves_multiple_frames() {
        let bytes = tiny_animated_gif(3);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Gif,
            ..query(None, None)
        };

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("animated GIF processing should succeed");

        assert_eq!(content_type, "image/gif");

        let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&output))
            .expect("output should be a valid GIF");
        let frames = decoder
            .into_frames()
            .collect_frames()
            .expect("decode frames");
        assert!(
            frames.len() > 1,
            "expected the animated source's multiple frames to survive round-tripping \
             through .gif output, got {}",
            frames.len()
        );
    }

    /// #49: same as above, but for animated WebP staying animated through
    /// `.webp` output - the case #49 explicitly asked to verify rather than
    /// assume was possible at all (it is - see `encode_animated_webp`'s doc
    /// comment).
    #[test]
    fn animated_webp_source_to_webp_output_preserves_multiple_frames() {
        let bytes = tiny_animated_webp(3);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Webp,
            ..query(None, None)
        };

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("animated WebP processing should succeed");

        assert_eq!(content_type, "image/webp");

        let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&output))
            .expect("output should be a valid WebP");
        assert!(decoder.has_animation(), "output should still be animated");
        let frames = decoder
            .into_frames()
            .collect_frames()
            .expect("decode frames");
        assert!(
            frames.len() > 1,
            "expected the animated source's multiple frames to survive round-tripping \
             through .webp output, got {}",
            frames.len()
        );
    }

    /// #49's own description of the pre-existing (and still correct for a
    /// non-animatable output format) behaviour: an animated source
    /// requested as a format that can't carry animation flattens to its
    /// first frame instead of erroring.
    #[test]
    fn animated_gif_requested_as_jpg_flattens_to_first_frame() {
        let bytes = tiny_animated_gif(3);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(None, None)
        };

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");

        assert_eq!(content_type, "image/jpeg");
        let decoded = image::load_from_memory(&output).expect("output should decode as JPEG");
        assert_eq!(decoded.dimensions(), (4, 4));
    }

    /// A many-tiny-frames animated source must be rejected once it exceeds
    /// `config.max_animation_frames`, without decoding every frame first
    /// (the cap itself is what's under test here, not the decode cost).
    #[test]
    fn animation_frame_count_over_limit_is_rejected() {
        let bytes = tiny_animated_gif(5);
        let config = PerformanceConfig {
            max_animation_frames: 3,
            ..PerformanceConfig::default()
        };
        let params = ResizeQuery {
            format: ApiImageFormat::Gif,
            ..query(None, None)
        };

        let err = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect_err("a 5-frame source over a 3-frame cap should be rejected");
        assert!(
            err.to_string().to_lowercase().contains("frame"),
            "expected a frame-count-related error, got: {err}"
        );
    }

    /// A source *within* the frame cap must still succeed (the cap is a
    /// ceiling, not an off-by-one trap).
    #[test]
    fn animation_frame_count_within_limit_succeeds() {
        let bytes = tiny_animated_gif(3);
        let config = PerformanceConfig {
            max_animation_frames: 3,
            ..PerformanceConfig::default()
        };
        let params = ResizeQuery {
            format: ApiImageFormat::Gif,
            ..query(None, None)
        };

        assert!(ImageService::process_image_blocking_with_limits(&bytes, &params, &config).is_ok());
    }

    /// #49/#68: AVIF output produces a real AVIF container (`....ftypavif...`
    /// per the AVIF/ISOBMFF magic bytes) with the right `Content-Type`, via
    /// `avif_codec::encode` (`libavif`/AOM, #68's replacement for
    /// `ravif`/`rav1e`).
    #[test]
    fn avif_output_produces_a_valid_avif_container() {
        let bytes = fixtures::photo_like();
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Avif,
            ..query_with_type(Some(64), Some(64), ResizeType::Fill)
        };

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("AVIF processing should succeed");

        assert_eq!(content_type, "image/avif");
        assert!(
            output.len() > 12,
            "AVIF output should have a real ISOBMFF header"
        );
        assert_eq!(&output[4..8], b"ftyp", "expected an ISOBMFF ftyp box");
        assert_eq!(&output[8..12], b"avif", "expected the avif major brand");
    }

    /// #67: AVIF *decode* (`avif_codec::decode`, `libavif`+dav1d) round
    /// trips against #68's own AVIF encode (`avif_codec::encode`,
    /// `libavif`+AOM) - encode a known-size source to AVIF, then feed that
    /// AVIF straight back in as a *source* (`detect_format_from_bytes` must
    /// recognise it via `is_avif`, `peek_dimensions` must read its header
    /// via `avif_codec::peek_dimensions`, and `decode_with_limits` must
    /// decode it via `avif_codec::decode`) and confirm the pipeline
    /// produces correctly-resized output. Before this change an AVIF
    /// source failed outright (`ImageFormat`'s doc comment in
    /// `src/models/params.rs`, now updated).
    #[test]
    fn avif_source_decodes_and_resizes_correctly() {
        let photo = fixtures::photo_like();
        let photo_img = image::load_from_memory(&photo).expect("fixture should decode");
        let avif_source = crate::services::image::avif_codec::encode(&photo_img, 80, 8, None)
            .expect("AVIF encode should succeed");

        assert!(ImageService::detect_format_from_bytes(&avif_source) == Some(ImageFormat::Avif));

        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query_with_type(Some(100), Some(100), ResizeType::Fill)
        };

        let (output, content_type) =
            ImageService::process_image_blocking_with_limits(&avif_source, &params, &config)
                .expect("AVIF source should decode and resize");

        assert_eq!(content_type, "image/jpeg");
        let decoded =
            image::load_from_memory_with_format(&output, image::ImageFormat::Jpeg).unwrap();
        assert_eq!(decoded.dimensions(), (100, 100));
    }

    /// #67: an AVIF source whose header declares a resolution over the
    /// configured cap must be rejected *before* `avifDecoderNextImage`
    /// (the actual AV1 payload decode) runs - the same "tiny on disk,
    /// declares a huge resolution" decompression-bomb shape
    /// `decompression_bomb_fixture_is_rejected_before_full_decode` proves
    /// for the PNG `bomb()` fixture, reproduced here for AVIF by patching
    /// a real, cheaply-encoded AVIF's `ispe` (Image Spatial Extents) box -
    /// the field `avifDecoderParse`'s header-only read trusts - to declare
    /// 10000x10000 without actually re-encoding that many pixels.
    #[test]
    fn avif_decompression_bomb_is_rejected_before_full_decode() {
        let small = image::DynamicImage::ImageRgb8(fixtures::gradient_noise_rgb(8, 8));
        let mut avif_bytes = crate::services::image::avif_codec::encode(&small, 50, 8, None)
            .expect("AVIF encode should succeed");

        // Locate the `ispe` box (ISO/IEC 23008-12 6.5.3): FullBox header
        // (4-byte size, 4-byte type "ispe", 1-byte version, 3-byte flags)
        // followed by big-endian `image_width`/`image_height` u32s -
        // exactly the two fields `avifDecoderParse` populates
        // `decoder->image->width/height` from without ever touching the
        // AV1-coded payload.
        let ispe_offset = avif_bytes
            .windows(4)
            .position(|w| w == b"ispe")
            .expect("encoded AVIF should contain an ispe box");
        let width_offset = ispe_offset + 4 /* type */ + 4 /* version+flags */;
        avif_bytes[width_offset..width_offset + 4].copy_from_slice(&10_000u32.to_be_bytes());
        avif_bytes[width_offset + 4..width_offset + 8].copy_from_slice(&10_000u32.to_be_bytes());

        let config = PerformanceConfig::default(); // 50 MP cap
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query_with_type(Some(100), Some(100), ResizeType::Fill)
        };

        let err =
            ImageService::process_image_blocking_with_limits(&avif_bytes, &params, &config)
                .expect_err("10000x10000-declared AVIF source should be rejected");
        let msg = err.to_string().to_lowercase();
        assert!(
            msg.contains("too large"),
            "expected a resolution-too-large error, got: {msg}"
        );
    }

    /// `params.quality` (the `q:` processing option) must actually change
    /// AVIF output size - unlike the pre-existing WebP/JPEG paths, #49
    /// wires it through (now, post-#68, via
    /// `crate::services::image::avif_codec::encode`'s own `quality`
    /// parameter, libavif/AOM's `avifEncoder.quality`, rather than the old
    /// `AvifEncoder::new_with_speed_quality`).
    #[test]
    fn avif_quality_changes_output_size() {
        let bytes = fixtures::photo_like();
        let config = PerformanceConfig::default();
        let params_at = |quality: u8| ResizeQuery {
            format: ApiImageFormat::Avif,
            quality: Some(quality),
            ..query_with_type(Some(128), Some(128), ResizeType::Fill)
        };

        let (low, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params_at(20), &config)
                .expect("low-quality AVIF should succeed");
        let (high, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params_at(95), &config)
                .expect("high-quality AVIF should succeed");

        assert!(
            low.len() < high.len(),
            "expected quality 20 ({} bytes) to be smaller than quality 95 ({} bytes)",
            low.len(),
            high.len()
        );
    }

    // ---- #50: explicit crop + gravity ----

    /// Builds a `ResizeQuery` requesting no resize at all (`width`/`height`
    /// both `None`, so `process_image_blocking_with_limits`'s
    /// `(None, None) => img` arm returns the crop step's output untouched)
    /// with the given `crop`, against `fixtures::gravity_marker()`. Keeping
    /// resize out of the picture makes `apply_crop`'s output pixel-exact -
    /// `DynamicImage::crop_imm` is a plain extraction, no resampling - so
    /// assertions below can check for a marker's *exact* RGB rather than a
    /// resampled approximation.
    fn crop_only_query(crop: Crop) -> ResizeQuery {
        ResizeQuery {
            crop: Some(crop),
            format: ApiImageFormat::Png,
            ..query(None, None)
        }
    }

    /// True if any pixel in `img` matches `color` exactly.
    fn contains_exact_color(img: &image::RgbImage, color: image::Rgb<u8>) -> bool {
        img.pixels().any(|p| *p == color)
    }

    /// True if any pixel in `img` matches `color` within `tol` per channel -
    /// used for the `Fill`+gravity test below, where the marker actually
    /// goes through `fast_image_resize` resampling (not just extraction),
    /// so an exact-equality check would be too strict at a marker's own
    /// edge pixels (interior pixels, away from the edge, still land on the
    /// exact colour - see `fixtures::gravity_marker_rgb`'s doc comment -
    /// but scanning the whole image doesn't distinguish edge from
    /// interior).
    fn contains_color_near(img: &image::RgbImage, color: image::Rgb<u8>, tol: u8) -> bool {
        img.pixels().any(|p| {
            p.0.iter()
                .zip(color.0.iter())
                .all(|(a, b)| a.abs_diff(*b) <= tol)
        })
    }

    fn decode_rgb(output: &[u8]) -> image::RgbImage {
        image::load_from_memory_with_format(output, ImageFormat::Png)
            .expect("output should decode")
            .to_rgb8()
    }

    /// #50: a half-size (`MARKER_W/2 x MARKER_H/2`), `Center`-gravity crop
    /// of the marker fixture excludes every corner marker (each corner
    /// square sits entirely outside the centred crop window) but keeps the
    /// centre marker - proving the crop is where imgproxy's own default
    /// (`ce:0:0`) says it should be, not accidentally reproducing some
    /// other anchor.
    #[test]
    fn explicit_crop_center_gravity_keeps_only_the_centre_marker() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();
        let crop = Crop {
            width: CropDimension::Absolute(fixtures::MARKER_W / 2),
            height: CropDimension::Absolute(fixtures::MARKER_H / 2),
            gravity: Gravity::Center,
        };
        let params = crop_only_query(crop);

        let (output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                .expect("processing should succeed");
        let decoded = decode_rgb(&output);

        assert!(contains_exact_color(&decoded, fixtures::MARKER_CENTER));
        for corner in [
            fixtures::MARKER_NORTHWEST,
            fixtures::MARKER_NORTHEAST,
            fixtures::MARKER_SOUTHWEST,
            fixtures::MARKER_SOUTHEAST,
        ] {
            assert!(
                !contains_exact_color(&decoded, corner),
                "centre-gravity crop should not contain corner marker {corner:?}"
            );
        }
    }

    /// #50: each corner gravity, with a half-size crop window, isolates
    /// exactly the marker in that corner - the whole point of gravity
    /// (imgproxy's `no`/`so`/`ea`/`we`/`noea`/`nowe`/`soea`/`sowe`,
    /// <https://docs.imgproxy.net/usage/processing#gravity>). Distinct from
    /// a dimensions-only assertion: a `NorthWest` crop and a `SouthEast`
    /// crop of the same source produce identical output dimensions here,
    /// and only differ in which marker survives.
    #[test]
    fn explicit_crop_corner_gravity_keeps_only_the_matching_corner_marker() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();

        let cases = [
            (Gravity::NorthWest, fixtures::MARKER_NORTHWEST),
            (Gravity::NorthEast, fixtures::MARKER_NORTHEAST),
            (Gravity::SouthWest, fixtures::MARKER_SOUTHWEST),
            (Gravity::SouthEast, fixtures::MARKER_SOUTHEAST),
        ];
        let all_markers = [
            fixtures::MARKER_NORTHWEST,
            fixtures::MARKER_NORTHEAST,
            fixtures::MARKER_SOUTHWEST,
            fixtures::MARKER_SOUTHEAST,
        ];

        for (gravity, expected_marker) in cases {
            let crop = Crop {
                width: CropDimension::Absolute(fixtures::MARKER_W / 2),
                height: CropDimension::Absolute(fixtures::MARKER_H / 2),
                gravity,
            };
            let params = crop_only_query(crop);

            let (output, _) =
                ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
                    .unwrap_or_else(|e| panic!("{gravity:?} processing should succeed: {e}"));
            let decoded = decode_rgb(&output);

            assert!(
                contains_exact_color(&decoded, expected_marker),
                "{gravity:?} crop should contain its own corner marker {expected_marker:?}"
            );
            for marker in all_markers {
                if marker == expected_marker {
                    continue;
                }
                assert!(
                    !contains_exact_color(&decoded, marker),
                    "{gravity:?} crop should not contain the other corner marker {marker:?}"
                );
            }
        }
    }

    // ---- #52: watermarking ----

    fn watermark_query() -> WatermarkQuery {
        WatermarkQuery {
            opacity: 1.0,
            position: WatermarkPosition::Center,
            x_offset: 0.0,
            y_offset: 0.0,
            scale: 0.0,
            url: None,
            size: None,
            rotate: 0.0,
            shadow: None,
        }
    }

    /// Opaque `width x height` RGBA image filled with `color`.
    fn solid_rgba(width: u32, height: u32, color: [u8; 4]) -> image::RgbaImage {
        image::ImageBuffer::from_fn(width, height, |_, _| image::Rgba(color))
    }

    /// #52: every documented (non-tiling) position anchors the watermark at
    /// the expected corner/edge/centre, with no offset applied.
    #[test]
    fn watermark_position_computes_every_anchor() {
        // 100x100 base, 20x10 watermark - asymmetric watermark dimensions
        // catch a position formula that accidentally swaps width/height.
        let (bw, bh, ww, wh) = (100u32, 100u32, 20u32, 10u32);
        let cases = [
            (WatermarkPosition::Center, (40, 45)),
            (WatermarkPosition::North, (40, 0)),
            (WatermarkPosition::South, (40, 90)),
            (WatermarkPosition::East, (80, 45)),
            (WatermarkPosition::West, (0, 45)),
            (WatermarkPosition::NorthEast, (80, 0)),
            (WatermarkPosition::NorthWest, (0, 0)),
            (WatermarkPosition::SouthEast, (80, 90)),
            (WatermarkPosition::SouthWest, (0, 90)),
        ];

        for (position, expected) in cases {
            let got = ImageService::watermark_position(position, 0.0, 0.0, bw, bh, ww, wh);
            assert_eq!(got, expected, "position {position:?}");
        }
    }

    /// #52: an offset with magnitude `>= 1.0` is absolute pixels; smaller
    /// than that is a fraction of the corresponding base dimension.
    #[test]
    fn watermark_position_offset_absolute_and_relative() {
        let (bw, bh, ww, wh) = (100u32, 100u32, 20u32, 20u32);

        // Absolute: NorthWest anchor is (0, 0); +15/-15 pixels moves it there directly.
        assert_eq!(
            ImageService::watermark_position(WatermarkPosition::NorthWest, 15.0, 5.0, bw, bh, ww, wh),
            (15, 5)
        );

        // Relative: 0.1 of a 100px base is 10px.
        assert_eq!(
            ImageService::watermark_position(WatermarkPosition::NorthWest, 0.1, 0.2, bw, bh, ww, wh),
            (10, 20)
        );

        // Negative offsets move left/up.
        assert_eq!(
            ImageService::watermark_position(WatermarkPosition::SouthEast, -5.0, -5.0, bw, bh, ww, wh),
            (75, 75)
        );
    }

    /// #52 pixel-exact: an opaque overlay composited at `opacity = 1.0`
    /// over an opaque canvas must exactly replace the covered pixels and
    /// leave every other pixel untouched.
    #[test]
    fn composite_over_full_opacity_fully_replaces_covered_pixels() {
        let mut canvas = solid_rgba(4, 4, [255, 0, 0, 255]); // red
        let overlay = solid_rgba(2, 2, [0, 0, 255, 255]); // blue

        ImageService::composite_over(&mut canvas, &overlay, 1, 1, 1.0);

        for y in 0..4 {
            for x in 0..4 {
                let covered = (1..3).contains(&x) && (1..3).contains(&y);
                let expected = if covered { [0, 0, 255, 255] } else { [255, 0, 0, 255] };
                assert_eq!(
                    canvas.get_pixel(x, y).0,
                    expected,
                    "pixel ({x},{y}), covered={covered}"
                );
            }
        }
    }

    /// #52 pixel-exact opacity blend: with an opaque overlay over an opaque
    /// background, `out = src*opacity + dst*(1-opacity)` and the result
    /// stays fully opaque.
    #[test]
    fn composite_over_honors_opacity() {
        let mut canvas = solid_rgba(1, 1, [0, 0, 0, 255]); // black
        let overlay = solid_rgba(1, 1, [255, 255, 255, 255]); // white

        ImageService::composite_over(&mut canvas, &overlay, 0, 0, 0.25);

        // 255*0.25 + 0*0.75 = 63.75 -> rounds to 64.
        assert_eq!(canvas.get_pixel(0, 0).0, [64, 64, 64, 255]);
    }

    /// #52: an overlay positioned partially (or fully) outside the canvas
    /// must not panic, and only the in-bounds portion is drawn.
    #[test]
    fn composite_over_clips_out_of_bounds_overlay() {
        let mut canvas = solid_rgba(4, 4, [0, 0, 0, 255]);
        let overlay = solid_rgba(4, 4, [255, 255, 255, 255]);

        // Positioned so only the bottom-right 2x2 of the overlay lands on
        // the canvas.
        ImageService::composite_over(&mut canvas, &overlay, -2, -2, 1.0);

        for y in 0..4u32 {
            for x in 0..4u32 {
                let covered = x < 2 && y < 2;
                let expected = if covered { 255 } else { 0 };
                assert_eq!(canvas.get_pixel(x, y).0, [expected, expected, expected, 255]);
            }
        }

        // Entirely off-canvas must be a total no-op, not a panic.
        let mut canvas2 = solid_rgba(4, 4, [1, 2, 3, 255]);
        let before = canvas2.clone();
        ImageService::composite_over(&mut canvas2, &overlay, 100, 100, 1.0);
        assert_eq!(canvas2, before);
    }

    #[test]
    fn rotate_rgba_zero_degrees_is_a_no_op() {
        let img = solid_rgba(5, 3, [10, 20, 30, 255]);
        let rotated = ImageService::rotate_rgba(&img, 0.0);
        assert_eq!(rotated.dimensions(), (5, 3));
        assert_eq!(rotated, img);
    }

    /// #52 pixel-exact: rotating 180 degrees around the centre of an
    /// odd-sized image keeps the same dimensions and maps pixel `(x, y)` to
    /// what was at `(w-1-x, h-1-y)` - odd dimensions keep every sampled
    /// coordinate away from an exact pixel-grid boundary, so nearest-
    /// neighbour sampling is exact here rather than merely close.
    #[test]
    fn rotate_rgba_180_degrees_maps_pixels_exactly() {
        let mut img = image::RgbaImage::new(5, 5);
        for y in 0..5 {
            for x in 0..5 {
                img.put_pixel(x, y, image::Rgba([x as u8, y as u8, 0, 255]));
            }
        }

        let rotated = ImageService::rotate_rgba(&img, 180.0);
        assert_eq!(rotated.dimensions(), (5, 5));

        for y in 0..5u32 {
            for x in 0..5u32 {
                let expected = img.get_pixel(4 - x, 4 - y);
                assert_eq!(
                    rotated.get_pixel(x, y),
                    expected,
                    "pixel ({x},{y}) after 180 degree rotation"
                );
            }
        }
    }

    /// #50: `North`/`South` keep the full width but anchor vertically -
    /// both top corners survive `North`, both bottom corners survive
    /// `South`, and vice versa is excluded. Uses a *full-width* crop
    /// (`CropDimension::Full`) so the horizontal axis can't accidentally
    /// exclude a corner and confound the vertical assertion - see
    /// `CropDimension::Full`'s doc comment ("use the full source dimension
    /// on this axis").
    #[test]
    fn explicit_crop_north_south_gravity_keeps_the_matching_edge_markers() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();

        let north_crop = Crop {
            width: CropDimension::Full,
            height: CropDimension::Absolute(fixtures::MARKER_H / 2),
            gravity: Gravity::North,
        };
        let (output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(north_crop),
            &config,
        )
        .expect("north crop should succeed");
        let decoded = decode_rgb(&output);
        assert!(contains_exact_color(&decoded, fixtures::MARKER_NORTHWEST));
        assert!(contains_exact_color(&decoded, fixtures::MARKER_NORTHEAST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_SOUTHWEST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_SOUTHEAST));

        let south_crop = Crop {
            width: CropDimension::Full,
            height: CropDimension::Absolute(fixtures::MARKER_H / 2),
            gravity: Gravity::South,
        };
        let (output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(south_crop),
            &config,
        )
        .expect("south crop should succeed");
        let decoded = decode_rgb(&output);
        assert!(contains_exact_color(&decoded, fixtures::MARKER_SOUTHWEST));
        assert!(contains_exact_color(&decoded, fixtures::MARKER_SOUTHEAST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_NORTHWEST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_NORTHEAST));
    }

    /// #50: a focus point placed right on top of the north-west marker's
    /// centre clamps to the same crop window a `NorthWest` gravity would
    /// produce (the box can't extend past the source's top-left corner),
    /// proving `Gravity::FocusPoint`'s clamping behaves like the
    /// corresponding corner at the boundary rather than partially reading
    /// out of bounds or panicking.
    #[test]
    fn explicit_crop_focus_point_near_a_corner_behaves_like_that_corner() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();

        // The north-west marker's centre, as a fraction of the source
        // dimensions.
        let fx = (fixtures::MARKER_SIZE as f64 / 2.0) / f64::from(fixtures::MARKER_W);
        let fy = (fixtures::MARKER_SIZE as f64 / 2.0) / f64::from(fixtures::MARKER_H);

        let crop = Crop {
            width: CropDimension::Absolute(fixtures::MARKER_W / 2),
            height: CropDimension::Absolute(fixtures::MARKER_H / 2),
            gravity: Gravity::FocusPoint { x: fx, y: fy },
        };
        let (output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(crop),
            &config,
        )
        .expect("focus-point crop should succeed");
        let decoded = decode_rgb(&output);

        assert!(contains_exact_color(&decoded, fixtures::MARKER_NORTHWEST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_NORTHEAST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_SOUTHWEST));
        assert!(!contains_exact_color(&decoded, fixtures::MARKER_SOUTHEAST));
    }

    /// #50: `CropDimension::Relative` (a `(0, 1)` fraction of the source
    /// dimension) and `CropDimension::Full` (the whole axis) resolve to the
    /// pixel sizes their doc comments claim - `Relative(0.5)` on a
    /// `MARKER_W`-wide source is exactly the same crop width as
    /// `Absolute(MARKER_W / 2)` used throughout the tests above, so the two
    /// forms must produce byte-identical output for equivalent requests.
    #[test]
    fn crop_relative_and_full_dimensions_resolve_correctly() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();

        let absolute = Crop {
            width: CropDimension::Absolute(fixtures::MARKER_W / 2),
            height: CropDimension::Absolute(fixtures::MARKER_H / 2),
            gravity: Gravity::NorthWest,
        };
        let relative = Crop {
            width: CropDimension::Relative(0.5),
            height: CropDimension::Relative(0.5),
            gravity: Gravity::NorthWest,
        };

        let (absolute_output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(absolute),
            &config,
        )
        .expect("absolute crop should succeed");
        let (relative_output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(relative),
            &config,
        )
        .expect("relative crop should succeed");

        assert_eq!(
            absolute_output, relative_output,
            "Absolute(W/2) and Relative(0.5) must resolve to the same crop"
        );

        let full = Crop {
            width: CropDimension::Full,
            height: CropDimension::Full,
            gravity: Gravity::Center,
        };
        let (full_output, _) = ImageService::process_image_blocking_with_limits(
            &bytes,
            &crop_only_query(full),
            &config,
        )
        .expect("full crop should succeed");
        let decoded = decode_rgb(&full_output);
        assert_eq!(
            (decoded.width(), decoded.height()),
            (fixtures::MARKER_W, fixtures::MARKER_H),
            "Full:Full crop should keep the entire source"
        );
        for marker in [
            fixtures::MARKER_NORTHWEST,
            fixtures::MARKER_NORTHEAST,
            fixtures::MARKER_SOUTHWEST,
            fixtures::MARKER_SOUTHEAST,
            fixtures::MARKER_CENTER,
        ] {
            assert!(contains_exact_color(&decoded, marker));
        }
    }

    /// #50: `gravity` also drives the `ResizeType::Fill` cover-crop, not
    /// just explicit `c:` crop - this is the change to
    /// `fir_resize_to_fill`/`ImageService`'s `ResizeType::Fill` arm
    /// (`src/services/image/handler.rs`) that replaces the old hardcoded
    /// centre crop. Downscales the marker fixture 2x into a square box,
    /// which forces a real horizontal crop (400x200 cover-scaled to 200x100
    /// for a 100x100 box), and checks that `West`/`East` gravity keep the
    /// matching side's markers - unlike the crop-only tests above, this
    /// goes through real `fast_image_resize` resampling, so marker presence
    /// is checked with a small colour tolerance
    /// (`contains_color_near`) rather than exact equality.
    #[test]
    fn fill_resize_honours_gravity_not_just_a_hardcoded_centre() {
        let bytes = fixtures::gravity_marker();
        let config = PerformanceConfig::default();

        let west_params = ResizeQuery {
            gravity: Gravity::West,
            format: ApiImageFormat::Png,
            ..query_with_type(Some(100), Some(100), ResizeType::Fill)
        };
        let (west_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &west_params, &config)
                .expect("west-gravity fill should succeed");
        let west_decoded = decode_rgb(&west_output);
        assert!(contains_color_near(
            &west_decoded,
            fixtures::MARKER_NORTHWEST,
            20
        ));
        assert!(contains_color_near(
            &west_decoded,
            fixtures::MARKER_SOUTHWEST,
            20
        ));
        assert!(!contains_color_near(
            &west_decoded,
            fixtures::MARKER_NORTHEAST,
            20
        ));
        assert!(!contains_color_near(
            &west_decoded,
            fixtures::MARKER_SOUTHEAST,
            20
        ));

        let east_params = ResizeQuery {
            gravity: Gravity::East,
            format: ApiImageFormat::Png,
            ..query_with_type(Some(100), Some(100), ResizeType::Fill)
        };
        let (east_output, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &east_params, &config)
                .expect("east-gravity fill should succeed");
        let east_decoded = decode_rgb(&east_output);
        assert!(contains_color_near(
            &east_decoded,
            fixtures::MARKER_NORTHEAST,
            20
        ));
        assert!(contains_color_near(
            &east_decoded,
            fixtures::MARKER_SOUTHEAST,
            20
        ));
        assert!(!contains_color_near(
            &east_decoded,
            fixtures::MARKER_NORTHWEST,
            20
        ));
        assert!(!contains_color_near(
            &east_decoded,
            fixtures::MARKER_SOUTHWEST,
            20
        ));

        assert_ne!(
            west_output, east_output,
            "west and east gravity must produce different output bytes, not the same \
             hardcoded centre crop"
        );
    }

    /// #50: `Self::gravity_anchor`'s `Center` case must reproduce
    /// `resize_to_fill`'s original always-centred arithmetic
    /// (`(container - box) / 2` per axis, integer/floor division) exactly -
    /// this is what makes replacing the hardcoded centre-crop with a
    /// gravity-driven one a strict generalisation rather than a behaviour
    /// change for the pre-#50 default.
    #[test]
    fn gravity_anchor_center_matches_original_floor_division_arithmetic() {
        for (container_w, container_h, box_w, box_h) in [
            (200u32, 100u32, 77u32, 33u32),
            (201, 101, 50, 50),
            (9, 9, 2, 2),
        ] {
            let (x, y) = ImageService::gravity_anchor(
                Gravity::Center,
                container_w,
                container_h,
                box_w,
                box_h,
            );
            assert_eq!(
                x,
                (container_w - box_w) / 2,
                "x for {container_w}x{container_h} box {box_w}x{box_h}"
            );
            assert_eq!(
                y,
                (container_h - box_h) / 2,
                "y for {container_w}x{container_h} box {box_w}x{box_h}"
            );
        }
    }

    /// #52: a 90 degree rotation swaps width and height (a non-square
    /// source proves the bounding-box math isn't accidentally squaring
    /// everything).
    #[test]
    fn rotate_rgba_90_degrees_swaps_dimensions() {
        let img = solid_rgba(6, 4, [1, 2, 3, 255]);
        let rotated = ImageService::rotate_rgba(&img, 90.0);
        assert_eq!(rotated.dimensions(), (4, 6));
    }

    /// #52: negative-angle input (counter-clockwise, imgproxy's own `wmr:`
    /// domain doesn't forbid it) must not panic and must still produce a
    /// sane, non-empty result via `rem_euclid` normalisation.
    #[test]
    fn rotate_rgba_negative_angle_does_not_panic() {
        let img = solid_rgba(5, 5, [1, 2, 3, 255]);
        let rotated = ImageService::rotate_rgba(&img, -90.0);
        assert_eq!(rotated.dimensions(), (5, 5));
    }

    /// #52: the shadow layer is the same size as its source, opaque pixels
    /// become an opaque-alpha black silhouette pre-blur, and blurring
    /// spreads some non-zero alpha into what was fully transparent border -
    /// the visible "halo" effect.
    #[test]
    fn build_shadow_layer_darkens_and_spreads_past_the_source_alpha() {
        // A 1px opaque dot in the middle of an otherwise fully transparent
        // 7x7 canvas.
        let mut watermark = image::RgbaImage::new(7, 7);
        watermark.put_pixel(3, 3, image::Rgba([200, 200, 200, 255]));

        let shadow = ImageService::build_shadow_layer(&watermark, 1.5);
        assert_eq!(shadow.dimensions(), (7, 7));

        // The centre pixel's colour channels must be black (the shadow
        // silhouette is colourless), whatever its post-blur alpha is.
        let centre = shadow.get_pixel(3, 3).0;
        assert_eq!([centre[0], centre[1], centre[2]], [0, 0, 0]);

        // A neighbouring pixel that started at alpha=0 must have picked up
        // some non-zero alpha from the blur spreading past the single
        // source pixel - otherwise this is just an unblurred silhouette.
        let neighbour_alpha = shadow.get_pixel(4, 3).0[3];
        assert!(
            neighbour_alpha > 0,
            "expected the blur to spread some alpha into a neighbouring pixel, got {neighbour_alpha}"
        );
    }

    /// #52 pixel-exact, full `apply_watermark` pipeline: a fully opaque
    /// watermark at `opacity: 1.0` and no scale/rotate/shadow must exactly
    /// replace the covered region with its own colour, at every documented
    /// position.
    #[test]
    fn apply_watermark_composites_at_every_position() {
        let base_color = [255, 0, 0, 255]; // red
        let watermark_color = [0, 255, 0, 255]; // green
        let (bw, bh, ww, wh) = (40u32, 40u32, 10u32, 10u32);

        let watermark_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(ww, wh, watermark_color));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };

        for position in [
            WatermarkPosition::Center,
            WatermarkPosition::North,
            WatermarkPosition::South,
            WatermarkPosition::East,
            WatermarkPosition::West,
            WatermarkPosition::NorthEast,
            WatermarkPosition::NorthWest,
            WatermarkPosition::SouthEast,
            WatermarkPosition::SouthWest,
        ] {
            let base = DynamicImage::ImageRgba8(solid_rgba(bw, bh, base_color));
            let wm = WatermarkQuery {
                position,
                ..watermark_query()
            };

            let composited = ImageService::apply_watermark(base, &watermark_bytes, &wm)
                .unwrap_or_else(|e| panic!("{position:?}: {e}"));
            let rgba = composited.to_rgba8();

            let (x, y) = ImageService::watermark_position(position, 0.0, 0.0, bw, bh, ww, wh);
            let (x, y) = (x as u32, y as u32);

            // Inside the watermark's own footprint: exactly its colour.
            assert_eq!(
                rgba.get_pixel(x + ww / 2, y + wh / 2).0,
                watermark_color,
                "{position:?}: expected watermark colour at its own centre"
            );
            // A far corner never covered by any of these positions/sizes:
            // still the untouched base colour.
            assert_eq!(
                rgba.get_pixel(bw / 2, bh / 2).0,
                if (x..x + ww).contains(&(bw / 2)) && (y..y + wh).contains(&(bh / 2)) {
                    watermark_color
                } else {
                    base_color
                },
                "{position:?}: base-image centre pixel"
            );
        }
    }

    /// #50: every directional/corner gravity anchors the box against the
    /// named edge(s)/corner exactly - `0` on an axis means "flush against
    /// the near edge", `container - box` means "flush against the far
    /// edge".
    #[test]
    fn gravity_anchor_directional_variants_anchor_to_the_named_edge() {
        let (w, h, bw, bh) = (300u32, 200u32, 100u32, 50u32);
        let max_x = w - bw;
        let max_y = h - bh;

        let cases = [
            (Gravity::North, (max_x / 2, 0)),
            (Gravity::South, (max_x / 2, max_y)),
            (Gravity::West, (0, max_y / 2)),
            (Gravity::East, (max_x, max_y / 2)),
            (Gravity::NorthWest, (0, 0)),
            (Gravity::NorthEast, (max_x, 0)),
            (Gravity::SouthWest, (0, max_y)),
            (Gravity::SouthEast, (max_x, max_y)),
        ];

        for (gravity, expected) in cases {
            let actual = ImageService::gravity_anchor(gravity, w, h, bw, bh);
            assert_eq!(actual, expected, "{gravity:?}");
        }
    }

    /// #50: `Gravity::FocusPoint` centres the box on the named point and
    /// clamps so the box never crosses a container edge - `(0, 0)` and
    /// `(1, 1)` (the extreme corners) clamp to the same anchor a corner
    /// gravity would produce, and `(0.5, 0.5)` matches `Center` exactly.
    #[test]
    fn gravity_anchor_focus_point_centres_and_clamps() {
        let (w, h, bw, bh) = (300u32, 200u32, 100u32, 50u32);
        let max_x = w - bw;
        let max_y = h - bh;

        assert_eq!(
            ImageService::gravity_anchor(Gravity::FocusPoint { x: 0.0, y: 0.0 }, w, h, bw, bh),
            (0, 0),
            "top-left focus point should clamp like NorthWest"
        );
        assert_eq!(
            ImageService::gravity_anchor(Gravity::FocusPoint { x: 1.0, y: 1.0 }, w, h, bw, bh),
            (max_x, max_y),
            "bottom-right focus point should clamp like SouthEast"
        );
        assert_eq!(
            ImageService::gravity_anchor(Gravity::FocusPoint { x: 0.5, y: 0.5 }, w, h, bw, bh),
            (max_x / 2, max_y / 2),
            "centre focus point should match Center gravity"
        );
    }
    // =====================================================================
    // #51: rotate, flip, trim, extend, padding, zoom, dpr, min-width/
    // min-height. Golden-image tests below assert *dimensions and pixel
    // positions*, not just status/success - a marker pixel is placed in
    // each fixture and the tests assert exactly where it ends up, so an
    // operation that returns the right size but scrambled content would
    // still fail. PNG is used throughout (never JPEG) so every assertion
    // is against byte-exact, lossless pixel data - no compression noise to
    // account for.
    // =====================================================================

    /// A solid-colour `width x height` image with a single, distinctly-
    /// coloured 1-pixel marker at `(marker_x, marker_y)` - used to prove not
    /// just output *dimensions* but where specific content actually ends
    /// up (a rotate/flip/trim/extend/padding that returns the right size
    /// but the wrong content would still pass a dimensions-only test).
    fn marker_image(width: u32, height: u32, marker_x: u32, marker_y: u32) -> RgbImage {
        let mut img = RgbImage::from_pixel(width, height, Rgb([20, 20, 20]));
        img.put_pixel(marker_x, marker_y, Rgb([255, 0, 0]));
        img
    }

    /// Finds the single `[255, 0, 0]` marker pixel `marker_image` places,
    /// panicking if it isn't present (e.g. lost to lossy encoding, which is
    /// exactly why every #51 test below uses PNG, never JPEG).
    fn find_marker(img: &RgbImage) -> (u32, u32) {
        img.enumerate_pixels()
            .find(|(_, _, p)| **p == Rgb([255, 0, 0]))
            .map(|(x, y, _)| (x, y))
            .expect("marker pixel should be present in the output")
    }

    fn encode_test_png(img: &RgbImage) -> Vec<u8> {
        let mut buf = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(img.clone())
            .write_to(&mut buf, ImageFormat::Png)
            .expect("test fixture should encode");
        buf.into_inner()
    }

    fn geometry_query() -> ResizeQuery {
        ResizeQuery {
            url: "https://images.example.com/marker.png".to_string(),
            format: ApiImageFormat::Png,
            ..Default::default()
        }
    }

    /// #51: `rotate:90` with no resize requested - proves the pixel
    /// rotation itself (dimensions swap, content moves) independent of any
    /// interaction with resize. The expected marker position is computed
    /// by rotating the *same* source image directly via the `image` crate
    /// (rather than hand-deriving `rotate90`'s pixel-mapping formula here),
    /// so this test is really asserting that `ImageService` invokes the
    /// rotation at the right point with the right angle - not re-testing
    /// the `image` crate's own `rotate90` correctness.
    #[test]
    fn rotate_90_swaps_dimensions_and_moves_marker() {
        let marker = marker_image(4, 8, 0, 0); // narrow/tall, marker top-left
        let bytes = encode_test_png(&marker);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            rotate: 90,
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(decoded.dimensions(), (8, 4), "rotate90 should swap width/height");

        let expected = image::imageops::rotate90(&marker);
        assert_eq!(find_marker(&decoded), find_marker(&expected));
    }

    /// #51: `flip:1:1` (both axes) moves a corner marker to the opposite
    /// corner, on a rectangular (non-square) image so a width/height mixup
    /// would be caught too.
    #[test]
    fn flip_both_axes_moves_marker_to_opposite_corner() {
        let marker = marker_image(6, 4, 0, 0); // marker at top-left
        let bytes = encode_test_png(&marker);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            flip_horizontal: true,
            flip_vertical: true,
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(decoded.dimensions(), (6, 4), "flip alone must not change dimensions");
        assert_eq!(
            find_marker(&decoded),
            (5, 3),
            "flipping both axes should move a top-left marker to the bottom-right corner"
        );
    }

    /// #51: `trim` removes a uniform border and leaves the interior -
    /// including a marker at its very first (top-left) pixel - exactly in
    /// place relative to the new, smaller canvas. No resize is requested,
    /// isolating trim's own behaviour.
    #[test]
    fn trim_removes_uniform_border_and_preserves_interior_marker_position() {
        const BORDER: u32 = 4;
        const INNER: u32 = 12;
        const SIZE: u32 = INNER + BORDER * 2;
        let border_colour = [200, 200, 200];

        let mut img = RgbImage::from_pixel(SIZE, SIZE, Rgb(border_colour));
        for y in BORDER..BORDER + INNER {
            for x in BORDER..BORDER + INNER {
                img.put_pixel(x, y, Rgb([10, 10, 10]));
            }
        }
        img.put_pixel(BORDER, BORDER, Rgb([255, 0, 0])); // marker: interior's top-left corner

        let bytes = encode_test_png(&img);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            trim: Some(TrimOptions {
                threshold: 0.0,
                color: Some(border_colour),
                equal_hor: false,
                equal_ver: false,
            }),
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(
            decoded.dimensions(),
            (INNER, INNER),
            "trim should remove exactly the uniform border"
        );
        assert_eq!(
            find_marker(&decoded),
            (0, 0),
            "the interior's top-left pixel should land at the trimmed image's (0, 0)"
        );
    }

    /// #51: `extend` pads a too-small image up to the requested canvas,
    /// centring the original - even with `enlarge` left at its default
    /// `false`, which alone would have refused to upscale the actual
    /// pixels. This is the key interaction `Self::effective_resize_box`'s
    /// `extend_box` exists for: `extend`'s target is read *before* the
    /// enlarge guard caps the resize step.
    #[test]
    fn extend_pads_to_target_size_centred_even_with_enlarge_disabled() {
        let marker = marker_image(4, 4, 0, 0);
        let bytes = encode_test_png(&marker);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            width: Some(12),
            height: Some(8),
            extend: true,
            ..geometry_query()
        };
        assert!(!params.enlarge, "test assumes enlarge defaults to false");

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(
            decoded.dimensions(),
            (12, 8),
            "extend should reach the full requested canvas despite enlarge=false"
        );
        // 4x4 source centred in a 12x8 canvas: offset ((12-4)/2, (8-4)/2) = (4, 2).
        assert_eq!(find_marker(&decoded), (4, 2));
        // The padded-in background should default to opaque white
        // (`DEFAULT_BACKGROUND`), matching #34/#60's own default.
        assert_eq!(*decoded.get_pixel(0, 0), Rgb(DEFAULT_BACKGROUND));
    }

    /// #51: `padding` always enlarges the canvas by exactly the requested
    /// amount on each side (CSS `top:right:bottom:left` order), regardless
    /// of `width`/`height`/`enlarge`.
    #[test]
    fn padding_grows_canvas_by_exact_amounts_on_each_side() {
        let marker = marker_image(4, 4, 3, 3); // marker at bottom-right corner
        let bytes = encode_test_png(&marker);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            padding: Some(Padding {
                top: 2,
                right: 3,
                bottom: 4,
                left: 5,
            }),
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(
            decoded.dimensions(),
            (4 + 5 + 3, 4 + 2 + 4),
            "canvas should grow by exactly left+right and top+bottom"
        );
        assert_eq!(
            find_marker(&decoded),
            (3 + 5, 3 + 2),
            "the source marker should shift by exactly (left, top)"
        );
        assert_eq!(*decoded.get_pixel(0, 0), Rgb(DEFAULT_BACKGROUND));
    }

    /// #51: `dpr` multiplies an explicit `width` (aspect-preserving, since
    /// `height` is unset) - the primary "responsive images" use case the
    /// issue calls out. Dimension-only (not marker-position): `dpr`/`zoom`
    /// are pure resampling multipliers, and resampling interpolates
    /// (unlike rotate/flip/trim/extend/padding's pixel-exact permutations),
    /// so a single-pixel marker doesn't survive intact - the *dimensions*
    /// are the property this option actually controls.
    #[test]
    fn dpr_multiplies_explicit_width_aspect_preserving() {
        let img = marker_image(10, 10, 0, 0);
        let bytes = encode_test_png(&img);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            width: Some(20),
            dpr: 2.0,
            // 20 * dpr 2 = 40, larger than the 10x10 source - needs
            // `enlarge` opted in, same as any other option that inflates
            // the effective target past the source (#36's guard applies
            // uniformly, see `Self::effective_resize_box`'s doc comment).
            enlarge: true,
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output).expect("output should decode");

        assert_eq!(
            decoded.dimensions(),
            (40, 40),
            "width 20 * dpr 2 = 40; square source stays square"
        );
    }

    /// #51: `min-width` forces the output up to at least the given size
    /// even with `enlarge` left at its default `false` - matching
    /// imgproxy's own documented behaviour (`prepare.go`'s min-width block
    /// runs unconditionally, bypassing the enlarge-gated shrink cap).
    #[test]
    fn min_width_bypasses_the_enlarge_guard() {
        let img = marker_image(10, 10, 0, 0);
        let bytes = encode_test_png(&img);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            min_width: Some(50),
            ..geometry_query()
        };
        assert!(!params.enlarge, "test assumes enlarge defaults to false");

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output).expect("output should decode");

        assert_eq!(
            decoded.dimensions(),
            (50, 50),
            "min-width should force upscaling past the source despite enlarge=false"
        );
    }

    /// #51 combined-operation test (explicitly required by the issue,
    /// since these operations don't commute): `rotate:90` together with an
    /// explicit `width`/`height` resize. Without the axis swap in
    /// `Self::effective_resize_box`, this would silently produce an
    /// 80x40 image instead of the requested 40x80 - exactly the "parses
    /// but means something different" trap #51 warns against. Uses a
    /// landscape (100x50) source with a *portrait* target (40x80) so the
    /// swap is actually exercised (a square source/target wouldn't
    /// distinguish a correct implementation from a buggy one that forgot
    /// to swap), and picks target dimensions that fit within the source's
    /// rotated-into-final-orientation box (50x100) so the #36 enlarge
    /// guard (left at its default `false`) doesn't also need to be
    /// involved - this test is isolating the axis swap, not the guard.
    #[test]
    fn rotate_90_combined_with_explicit_resize_yields_requested_final_dimensions() {
        let img = marker_image(100, 50, 0, 0); // landscape source
        let bytes = encode_test_png(&img);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            rotate: 90,
            width: Some(40),
            height: Some(80),
            resize_type: ResizeType::Force,
            ..geometry_query()
        };
        assert!(!params.enlarge, "test assumes enlarge defaults to false");

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output).expect("output should decode");

        assert_eq!(
            decoded.dimensions(),
            (40, 80),
            "the final, post-rotation image should match the requested width x height \
             exactly, not the pre-rotation resize box"
        );
    }

    /// #51 second combined-operation test: `trim` then `resize` - these
    /// don't commute (resizing first would blend the border into the
    /// interior via interpolation, making it untrimmable), so trim must
    /// run *before* resize sees the image. A source with a uniform border
    /// around a distinctly-marked interior is trimmed down to just the
    /// interior, then resized (with `Force`, so the exact target
    /// dimensions are unambiguous) - proving both that trim ran first
    /// (else the border would still be present, uniformly stretched, and
    /// wouldn't survive as sharp scaled-marker content the way this test
    /// checks) and that the resize afterwards used the post-trim size as
    /// its source, not the original.
    #[test]
    fn trim_then_resize_applies_trim_before_resize_sees_the_image() {
        const BORDER: u32 = 4;
        const INNER: u32 = 8;
        const SIZE: u32 = INNER + BORDER * 2;
        let border_colour = [200, 200, 200];

        let mut img = RgbImage::from_pixel(SIZE, SIZE, Rgb(border_colour));
        for y in BORDER..BORDER + INNER {
            for x in BORDER..BORDER + INNER {
                img.put_pixel(x, y, Rgb([10, 10, 10]));
            }
        }

        let bytes = encode_test_png(&img);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            trim: Some(TrimOptions {
                threshold: 0.0,
                color: Some(border_colour),
                equal_hor: false,
                equal_ver: false,
            }),
            width: Some(32),
            height: Some(32),
            resize_type: ResizeType::Force,
            // Upscaling from the post-trim 8x8 interior to 32x32 needs
            // `enlarge` opted in (#36) - irrelevant to what this test is
            // actually checking (trim-before-resize ordering), so it's
            // just switched on rather than picking non-upscaling numbers,
            // to keep the border-proportion arithmetic in the comment
            // below simple.
            enlarge: true,
            ..geometry_query()
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");
        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgb8();

        assert_eq!(decoded.dimensions(), (32, 32));

        // If trim had *not* run before resize, the border would still
        // occupy its original proportion (4/16 = 25%) of each edge after
        // a uniform Force stretch, so the resized border band would be
        // 25% of 32px = 8px wide/tall. Since trim removes the border
        // first, the resize source is a flat 8x8 interior with no border
        // at all - every corner of the 32x32 output should be the
        // interior colour, not the border colour.
        for &(x, y) in &[(0u32, 0u32), (31, 0), (0, 31), (31, 31)] {
            assert_eq!(
                *decoded.get_pixel(x, y),
                Rgb([10, 10, 10]),
                "corner ({x}, {y}) should be interior colour - trim must run before resize"
            );
        }
    }

    /// #52 pixel-exact opacity: same setup as above but `opacity: 0.5` over
    /// an opaque base - the covered region must be the exact 50/50 blend,
    /// not either input colour.
    #[test]
    fn apply_watermark_honors_opacity() {
        let base = DynamicImage::ImageRgba8(solid_rgba(20, 20, [0, 0, 0, 255]));
        let watermark_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(10, 10, [255, 255, 255, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let wm = WatermarkQuery {
            opacity: 0.5,
            ..watermark_query()
        };

        let composited = ImageService::apply_watermark(base, &watermark_bytes, &wm).unwrap();
        let rgba = composited.to_rgba8();

        // Centre of a 20x20 base with a centred 10x10 watermark is (10, 10)
        // - inside the watermark's footprint.
        assert_eq!(rgba.get_pixel(10, 10).0, [128, 128, 128, 255]);
        // Untouched corner keeps the base colour exactly.
        assert_eq!(rgba.get_pixel(0, 0).0, [0, 0, 0, 255]);
    }

    /// #52: `scale` resizes the watermark to `base_size * scale` (fit,
    /// preserving aspect ratio) before positioning - a square watermark
    /// scaled by 0.5 into a 40x40 base must land as a 20x20 region, not its
    /// original 10x10 size.
    #[test]
    fn apply_watermark_honors_scale() {
        let base_color = [10, 10, 10, 255];
        let watermark_color = [200, 200, 200, 255];
        let base = DynamicImage::ImageRgba8(solid_rgba(40, 40, base_color));
        let watermark_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(10, 10, watermark_color));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let wm = WatermarkQuery {
            scale: 0.5, // -> target 20x20 (fit within a 20x20 box)
            ..watermark_query()
        };

        let composited = ImageService::apply_watermark(base, &watermark_bytes, &wm).unwrap();
        let rgba = composited.to_rgba8();

        // Centred 20x20 watermark on a 40x40 base spans [10, 30) on both
        // axes. Just inside that boundary must be watermark colour, just
        // outside must still be the base colour.
        assert_eq!(rgba.get_pixel(10, 20).0, watermark_color, "inside scaled watermark");
        assert_eq!(rgba.get_pixel(29, 20).0, watermark_color, "inside scaled watermark, far edge");
        assert_eq!(rgba.get_pixel(9, 20).0, base_color, "just outside scaled watermark");
        assert_eq!(rgba.get_pixel(30, 20).0, base_color, "just outside scaled watermark");
    }

    /// #52 end-to-end: watermarking wired into the real decode/resize/
    /// encode pipeline, through a lossless format (PNG) so the composited
    /// pixels can be asserted exactly after a real encode/decode round
    /// trip - not just at the `apply_watermark` unit level above.
    #[test]
    fn watermark_composites_through_the_full_pipeline() {
        let base_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(20, 20, [255, 0, 0, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let watermark_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(10, 10, [0, 255, 0, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };

        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            watermark: Some(watermark_query()),
            ..query(None, None)
        };
        let config = PerformanceConfig::default();

        let (output, _content_type) =
            ImageService::process_image_blocking_with_limits_and_watermark(
                &base_bytes,
                &params,
                &config,
                Some(&watermark_bytes),
            )
            .expect("watermarked processing should succeed");

        let decoded = image::load_from_memory(&output)
            .expect("output should decode")
            .to_rgba8();
        assert_eq!(decoded.dimensions(), (20, 20));
        // Centre (inside the centred 10x10 watermark) is green; a corner
        // (outside it) is still the base's red.
        assert_eq!(decoded.get_pixel(10, 10).0, [0, 255, 0, 255]);
        assert_eq!(decoded.get_pixel(0, 0).0, [255, 0, 0, 255]);
    }

    /// #52's `process_image_blocking_with_limits` (the pre-existing
    /// 3-argument form every other test in this module calls) must remain
    /// exactly equivalent to "no watermark" - a watermark-carrying
    /// `ResizeQuery` passed through it must be processed as if `watermark`
    /// were `None`, since this entry point has no watermark bytes to
    /// composite. This is what lets the ~20 pre-existing call sites stay
    /// unmodified by #52 without silently changing their behaviour.
    #[test]
    fn three_argument_entry_point_ignores_watermark_field_with_no_bytes_supplied() {
        let base_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(10, 10, [1, 2, 3, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            watermark: Some(watermark_query()),
            ..query(None, None)
        };
        let config = PerformanceConfig::default();

        let (output, _) =
            ImageService::process_image_blocking_with_limits(&base_bytes, &params, &config)
                .expect("processing should succeed even though there is no watermark to composite");
        let decoded = image::load_from_memory(&output).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(5, 5).0, [1, 2, 3, 255]);
    }

    /// #52's SSRF regression test: a watermark URL that resolves to a
    /// blocked private address must be refused through the exact same
    /// guard the main source URL goes through (#21/#57) - not silently
    /// skipped, not fetched anyway.
    #[tokio::test]
    async fn watermark_url_pointing_at_blocked_private_address_is_refused() {
        let service = ImageService::with_config(PerformanceConfig::default()).unwrap();
        let params = ResizeQuery {
            watermark: Some(WatermarkQuery {
                url: Some("http://127.0.0.1:1/watermark.png".to_string()),
                ..watermark_query()
            }),
            ..query(Some(10), Some(10))
        };
        let bytes = Bytes::from(fixtures::tiny());

        let err = service
            .process_image(&bytes, &params)
            .await
            .expect_err("a watermark URL pointing at a blocked private address must be refused");

        let rejected = err.downcast_ref::<crate::services::image::source_guard::SourceRejected>();
        assert!(
            rejected.is_some(),
            "expected a typed SourceRejected rejection, got: {err}"
        );
    }

    /// A `wm:` request with neither `wmu:` nor a configured `WATERMARK_URL`
    /// default is a clear error, not a silently-skipped watermark.
    #[tokio::test]
    async fn watermark_requested_with_no_source_available_is_an_error() {
        let service = ImageService::with_config(PerformanceConfig::default()).unwrap();
        let params = ResizeQuery {
            watermark: Some(watermark_query()), // url: None, and config.watermark_url is also None
            ..query(Some(10), Some(10))
        };
        let bytes = Bytes::from(fixtures::tiny());

        let err = service
            .process_image(&bytes, &params)
            .await
            .expect_err("watermark with no available source must fail, not silently skip");
        assert!(
            err.to_string().to_lowercase().contains("watermark"),
            "expected an error mentioning the watermark, got: {err}"
        );
    }

    /// The deployment's configured `WATERMARK_URL` default is used when
    /// the request's own `wmu:` is absent.
    #[tokio::test]
    async fn configured_default_watermark_url_is_used_when_request_supplies_none() {
        let watermark_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(4, 4, [0, 255, 0, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = watermark_bytes.clone();
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

        let config = PerformanceConfig {
            enable_http2: false,
            allow_loopback_source_addresses: true,
            watermark_url: Some(format!("http://{addr}/wm.png")),
            ..PerformanceConfig::default()
        };
        let service = ImageService::with_config(config).unwrap();

        let base_bytes = {
            let img = DynamicImage::ImageRgba8(solid_rgba(10, 10, [255, 0, 0, 255]));
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            watermark: Some(watermark_query()), // wmu: absent - must fall back to config
            ..query(None, None)
        };

        let (output, _) = service
            .process_image(&Bytes::from(base_bytes), &params)
            .await
            .expect("should fall back to the configured default watermark URL");
        let decoded = image::load_from_memory(&output).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(5, 5).0, [0, 255, 0, 255]);
    }

    // ---- #5: strip_metadata ----

    /// Decodes `bytes` as `format` and returns whatever raw Exif blob (if
    /// any) `image`'s own decoder finds - used below to check the *actual*
    /// encoded output, not just that `strip_metadata` parsed.
    fn decoded_exif(bytes: &[u8], format: ImageFormat) -> Option<Vec<u8>> {
        image::ImageReader::with_format(Cursor::new(bytes), format)
            .into_decoder()
            .expect("test fixture/output should have a valid header")
            .exif_metadata()
            .expect("exif_metadata() read should not itself fail")
    }

    /// The exact GPS `GPSLatitudeRef` marker byte (`fixtures::jpeg_with_gps_exif`
    /// encodes latitude ref `"N"`) - a small, distinctive fingerprint used
    /// by the AVIF test below. That test checks raw byte presence rather
    /// than decoding the output: #67 did add an AVIF decoder
    /// (`crate::services::image::avif_codec`), but round-tripping through
    /// it would test the decoder as much as the metadata write, so the
    /// fingerprint check is deliberately kept.
    const GPS_LATITUDE_REF_NORTH: &[u8] = b"N\0\0\0";

    /// #5: `sm` absent must default to *stripping* EXIF - imgproxy's own
    /// `IMGPROXY_STRIP_METADATA` default, and the behaviour change this
    /// issue exists to make deliberate. `fixtures::jpeg_with_gps_exif(1)`
    /// carries a real GPS location (Golden Gate Bridge) in its source Exif;
    /// orientation `1` (`Normal`) keeps this test isolated from the
    /// orientation-neutralization behaviour covered separately below.
    #[test]
    fn strip_metadata_defaults_to_stripping_exif_from_jpeg_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        // Sanity check on the fixture itself - if this ever fails, the
        // fixture stopped carrying Exif at all and every assertion below
        // would be vacuous.
        assert!(
            decoded_exif(&bytes, ImageFormat::Jpeg).is_some(),
            "fixture sanity check: the source must actually carry Exif"
        );

        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            ..query(None, None) // strip_metadata left unset -> defaults true
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        assert_eq!(
            decoded_exif(&output, ImageFormat::Jpeg),
            None,
            "default (sm unset) must strip Exif - including GPS - from JPEG output"
        );
    }

    /// #5: `sm:0`/`strip_metadata: false` must forward the source's Exif -
    /// GPS included - to JPEG output via the raw `APP1` marker
    /// `encode_jpeg_inner` writes by hand (mozjpeg has no higher-level Exif
    /// API, unlike PNG/AVIF's `set_exif_metadata`).
    #[test]
    fn strip_metadata_false_keeps_gps_in_jpeg_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            strip_metadata: false,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        let exif = decoded_exif(&output, ImageFormat::Jpeg)
            .expect("sm:0 must keep Exif metadata in JPEG output");
        assert!(
            exif.windows(GPS_LATITUDE_REF_NORTH.len())
                .any(|w| w == GPS_LATITUDE_REF_NORTH),
            "kept Exif must still contain the real GPSLatitudeRef field, not just an empty/\
             truncated APP1 segment"
        );
    }

    /// Correctness requirement from the issue: a strip and a keep request
    /// for the otherwise-identical parameters must produce genuinely
    /// different encoded bytes, not just a parsed-but-inert option - the
    /// exact "flag that silently does nothing" failure mode the JPEG
    /// progressive bug (`.bench-baseline/BASELINE.md`) already shipped once.
    #[test]
    fn strip_metadata_true_and_false_produce_different_jpeg_bytes() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();

        let stripped = ResizeQuery {
            format: ApiImageFormat::Jpg,
            strip_metadata: true,
            ..query(None, None)
        };
        let kept = ResizeQuery {
            format: ApiImageFormat::Jpg,
            strip_metadata: false,
            ..query(None, None)
        };

        let (out_stripped, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &stripped, &config)
                .expect("processing should succeed");
        let (out_kept, _) = ImageService::process_image_blocking_with_limits(&bytes, &kept, &config)
            .expect("processing should succeed");

        assert_ne!(
            out_stripped, out_kept,
            "strip_metadata=true and =false must produce different encoded bytes"
        );
    }

    /// The double-rotation guard: `fixtures::jpeg_with_gps_exif(6)` is
    /// tagged EXIF orientation `6` (`Rotate90` per `apply_orientation`'s own
    /// match arms). With `autorotate` on (the default) and metadata kept,
    /// the *pixels* get rotated - so the *kept* Exif's own Orientation tag
    /// must read back as `1` (`Orientation::Normal`) in the output, proving
    /// `Self::neutralize_exif_orientation` actually ran rather than
    /// forwarding a now-stale "rotate me" instruction that would
    /// double-rotate the image in any EXIF-aware viewer. GPS must still
    /// survive untouched - only the one field changes.
    #[test]
    fn kept_metadata_neutralizes_stale_orientation_to_avoid_double_rotation() {
        let bytes = fixtures::jpeg_with_gps_exif(6);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            strip_metadata: false,
            autorotate: true,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        let mut decoder = image::ImageReader::with_format(Cursor::new(&output), ImageFormat::Jpeg)
            .into_decoder()
            .expect("output should have a valid JPEG header");
        assert_eq!(
            decoder.orientation().expect("orientation read should not fail"),
            Orientation::NoTransforms,
            "the kept Exif's Orientation tag must be neutralized to 1/NoTransforms once \
             autorotate has already rotated the pixels, or a viewer would rotate twice"
        );

        let exif = decoder
            .exif_metadata()
            .expect("exif_metadata read should not fail")
            .expect("Exif must still be present (kept, just orientation-neutralized)");
        assert!(
            exif.windows(GPS_LATITUDE_REF_NORTH.len())
                .any(|w| w == GPS_LATITUDE_REF_NORTH),
            "neutralizing the Orientation tag must not disturb the unrelated GPS fields"
        );
    }

    /// When `autorotate` is off, the pixels are never touched, so the
    /// original (still-accurate) Orientation tag must survive a `sm:0`
    /// request completely unchanged - `neutralize_exif_orientation` must
    /// not even be invoked in this case (`exif_orientation_applied` is
    /// `false`).
    #[test]
    fn kept_metadata_preserves_orientation_tag_when_autorotate_is_disabled() {
        let bytes = fixtures::jpeg_with_gps_exif(6);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Jpg,
            strip_metadata: false,
            autorotate: false,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        let orientation = image::ImageReader::with_format(Cursor::new(&output), ImageFormat::Jpeg)
            .into_decoder()
            .expect("output should have a valid JPEG header")
            .orientation()
            .expect("orientation read should not fail");
        assert_eq!(
            orientation,
            Orientation::Rotate90,
            "autorotate:false must leave the original Orientation tag (6/Rotate90) untouched - \
             the pixels were never rotated, so the tag is still accurate"
        );
    }

    /// #5's per-format matrix: PNG carries a real `eXIf` chunk
    /// (`image::codecs::png::PngEncoder::set_exif_metadata`), so metadata
    /// kept from a JPEG *source* must still reach a PNG *output* - proving
    /// this isn't JPEG-specific plumbing.
    #[test]
    fn strip_metadata_false_keeps_gps_in_png_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            strip_metadata: false,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        let exif =
            decoded_exif(&output, ImageFormat::Png).expect("sm:0 must keep Exif in PNG output");
        assert!(
            exif.windows(GPS_LATITUDE_REF_NORTH.len())
                .any(|w| w == GPS_LATITUDE_REF_NORTH),
            "kept Exif forwarded into a PNG output must still contain the GPS field"
        );
    }

    /// The default (strip) must apply identically to a PNG *output*, not
    /// just JPEG - same fixture, opposite of the test above.
    #[test]
    fn strip_metadata_defaults_to_stripping_exif_from_png_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Png,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        assert_eq!(decoded_exif(&output, ImageFormat::Png), None);
    }

    /// #5's per-format matrix: this crate's lossy WebP output goes through
    /// the standalone `webp` crate (`Self::encode_webp`), whose `Encoder`
    /// has no Exif/ICC API at all - `sm:0` against a `.webp` output is a
    /// real, documented no-op, not a bug. Proven directly against the
    /// output bytes rather than just asserting `encode_single_image` didn't
    /// panic, so a future encoder swap that silently starts (or stops)
    /// carrying metadata would be caught here.
    #[test]
    fn strip_metadata_false_has_no_effect_on_webp_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();
        let params = ResizeQuery {
            format: ApiImageFormat::Webp,
            strip_metadata: false,
            ..query(None, None)
        };

        let (output, _) = ImageService::process_image_blocking_with_limits(&bytes, &params, &config)
            .expect("processing should succeed");

        assert_eq!(
            decoded_exif(&output, ImageFormat::WebP),
            None,
            "the webp crate's Encoder has no Exif API - sm:0 cannot be honoured for WebP output"
        );
    }

    /// #5's per-format matrix: unlike ICC (not threaded through for AVIF -
    /// see `avif_codec::encode`'s own doc comment for why that's a
    /// "not wired up" gap, not a hard capability limit), AVIF output *can*
    /// keep Exif - `crate::services::image::avif_codec::encode`
    /// (libavif/AOM, #68's replacement for
    /// `image::codecs::avif::AvifEncoder`) writes it via
    /// `avifImageSetMetadataExif`. #67 later added an AVIF *decoder*
    /// (`avif_codec::decode`), but it only returns pixels plus
    /// `icc_profile`/`exif_metadata` as opaque blobs, not a way to assert on
    /// tag-level EXIF content, so a raw byte search for the GPS fingerprint
    /// in the encoded output is still simpler than decoding it back -
    /// unlike every other format-matrix test above.
    #[test]
    fn strip_metadata_false_keeps_gps_in_avif_output() {
        let bytes = fixtures::jpeg_with_gps_exif(1);
        let config = PerformanceConfig::default();

        let kept = ResizeQuery {
            format: ApiImageFormat::Avif,
            strip_metadata: false,
            ..query(None, None)
        };
        let stripped = ResizeQuery {
            format: ApiImageFormat::Avif,
            strip_metadata: true,
            ..query(None, None)
        };

        let (out_kept, _) = ImageService::process_image_blocking_with_limits(&bytes, &kept, &config)
            .expect("processing should succeed");
        let (out_stripped, _) =
            ImageService::process_image_blocking_with_limits(&bytes, &stripped, &config)
                .expect("processing should succeed");

        assert!(
            out_kept
                .windows(GPS_LATITUDE_REF_NORTH.len())
                .any(|w| w == GPS_LATITUDE_REF_NORTH),
            "sm:0 against an AVIF output must still carry the GPS field"
        );
        assert!(
            !out_stripped
                .windows(GPS_LATITUDE_REF_NORTH.len())
                .any(|w| w == GPS_LATITUDE_REF_NORTH),
            "default (strip) AVIF output must not carry the GPS field"
        );
    }

    /// Direct unit tests of `neutralize_exif_orientation`, independent of
    /// the full encode pipeline above - covers the fail-closed cases the
    /// end-to-end tests can't easily exercise (malformed input, no
    /// Orientation tag present at all) plus both TIFF byte orders.
    #[test]
    fn neutralize_exif_orientation_rewrites_value_to_one_little_endian() {
        let mut exif = Vec::new();
        exif.extend_from_slice(b"II");
        exif.extend_from_slice(&42u16.to_le_bytes());
        exif.extend_from_slice(&8u32.to_le_bytes());
        exif.extend_from_slice(&1u16.to_le_bytes()); // 1 entry
        exif.extend_from_slice(&0x0112u16.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&6u16.to_le_bytes()); // orientation 6
        exif.extend_from_slice(&0u16.to_le_bytes());
        exif.extend_from_slice(&0u32.to_le_bytes());

        let patched =
            ImageService::neutralize_exif_orientation(&exif).expect("well-formed input");
        assert_eq!(&patched[18..20], &1u16.to_le_bytes());
        // Nothing else in the blob should have moved.
        assert_eq!(patched.len(), exif.len());
        assert_eq!(&patched[..18], &exif[..18]);
    }

    #[test]
    fn neutralize_exif_orientation_rewrites_value_to_one_big_endian() {
        let mut exif = Vec::new();
        exif.extend_from_slice(b"MM");
        exif.extend_from_slice(&42u16.to_be_bytes());
        exif.extend_from_slice(&8u32.to_be_bytes());
        exif.extend_from_slice(&1u16.to_be_bytes());
        exif.extend_from_slice(&0x0112u16.to_be_bytes());
        exif.extend_from_slice(&3u16.to_be_bytes());
        exif.extend_from_slice(&1u32.to_be_bytes());
        exif.extend_from_slice(&8u16.to_be_bytes()); // orientation 8
        exif.extend_from_slice(&0u16.to_be_bytes());
        exif.extend_from_slice(&0u32.to_be_bytes());

        let patched =
            ImageService::neutralize_exif_orientation(&exif).expect("well-formed input");
        assert_eq!(&patched[18..20], &1u16.to_be_bytes());
    }

    #[test]
    fn neutralize_exif_orientation_returns_none_for_malformed_input() {
        assert_eq!(ImageService::neutralize_exif_orientation(&[]), None);
        assert_eq!(ImageService::neutralize_exif_orientation(b"not tiff"), None);
        assert_eq!(
            ImageService::neutralize_exif_orientation(b"II\x2a\x00\xff\xff\xff\xff"),
            None,
            "an IFD0 offset pointing past the end of the blob must fail closed"
        );
    }

    #[test]
    fn neutralize_exif_orientation_returns_none_when_tag_absent() {
        // Well-formed TIFF/IFD0 with a single, unrelated tag (ImageWidth,
        // 0x0100) instead of Orientation.
        let mut exif = Vec::new();
        exif.extend_from_slice(b"II");
        exif.extend_from_slice(&42u16.to_le_bytes());
        exif.extend_from_slice(&8u32.to_le_bytes());
        exif.extend_from_slice(&1u16.to_le_bytes());
        exif.extend_from_slice(&0x0100u16.to_le_bytes());
        exif.extend_from_slice(&3u16.to_le_bytes());
        exif.extend_from_slice(&1u32.to_le_bytes());
        exif.extend_from_slice(&100u16.to_le_bytes());
        exif.extend_from_slice(&0u16.to_le_bytes());
        exif.extend_from_slice(&0u32.to_le_bytes());

        assert_eq!(ImageService::neutralize_exif_orientation(&exif), None);
    }
}
