use crate::config::performance::PerformanceConfig;
use crate::models::params::{ResizeQuery, ResizeType};
use crate::services::image::source_guard;
use anyhow::{Context, Result};
use bytes::{Bytes, BytesMut};
use derive_builder::Builder;
use fast_image_resize as fir;
use futures::StreamExt;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, ImageFormat};
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Semaphore;
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
        // bytes (#31).
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

            let result = Self::process_image_blocking_with_limits(&image_bytes, &params, &config);
            let _ = tx.send(result);
        });

        rx.await.context("Image processing task was cancelled")?
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
    fn process_image_blocking_with_limits(
        image_bytes: &[u8],
        params: &ResizeQuery,
        config: &PerformanceConfig,
    ) -> Result<(Vec<u8>, String)> {
        Self::check_output_dimensions(params, config)?;

        // Use faster image decoding with format hints
        let format = Self::detect_format_from_bytes(image_bytes);

        // Peek the header-only dimensions *before* touching the full
        // decode path, so a decompression-bomb-style source (tiny on disk,
        // huge decoded) is rejected without ever allocating the decoded
        // buffer.
        let (src_width, src_height) = Self::peek_dimensions(image_bytes, format)?;
        Self::check_source_resolution(src_width, src_height, config.max_src_resolution_mp)?;

        let img = Self::decode_with_limits(image_bytes, format, config.max_src_resolution_mp)?;

        let (src_width, src_height) = img.dimensions();

        // Upscale guard (#36): refuses to enlarge past the source
        // resolution unless `params.enlarge` opts in, mirroring imgproxy's
        // `enlarge` option (default off). Capping each requested dimension
        // to the source's, rather than rejecting the request outright,
        // keeps every resize branch below unchanged - it just never sees a
        // target dimension larger than the source, so none of them can
        // upscale. This also closes a cheap CPU amplification vector: the
        // committed benchmark baseline (.bench-baseline/BASELINE.md) puts
        // resize/upscale/lanczos3 at 143ms vs 17.4ms for the equivalent
        // downscale (~8x), for what would otherwise be a single request
        // naming an arbitrary output size against a tiny source.
        let effective_width = params
            .width
            .map(|w| if params.enlarge { w } else { w.min(src_width) });
        let effective_height = params
            .height
            .map(|h| if params.enlarge { h } else { h.min(src_height) });

        // Use faster resize algorithms for different scenarios
        let filter = match (effective_width, effective_height) {
            // For thumbnails, use faster Triangle filter
            (Some(w), Some(h)) if w <= 300 && h <= 300 => FilterType::Triangle,
            // For high quality, use Lanczos3
            _ => FilterType::Lanczos3,
        };

        // Resize image with optimized logic. `effective_width`/
        // `effective_height` are already capped to the source resolution
        // per axis unless `enlarge` is set (above), and every branch below
        // - `resize` (fit), `resize_to_fill` (fill/auto-as-fill),
        // `resize_exact` (force) - only ever shrinks each axis to at most
        // its capped target, so none of them can upscale past the source:
        // the #36 guard holds for every resize type, not just fill.
        // #63 stage 1: the actual resampling is now done by
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
            (Some(w), None) => Self::fir_resize(&img, w, u32::MAX, filter)?,
            (None, Some(h)) => Self::fir_resize(&img, u32::MAX, h, filter)?,
            (Some(w), Some(h)) => match params.resize_type {
                // Fit inside the box, preserving aspect ratio - neither
                // output dimension exceeds `w`/`h`. This is also what a
                // lone width or height already did above, so `Fit` (the
                // default, see `ResizeType`) keeps that existing behaviour
                // consistent once both dimensions are given (#59).
                ResizeType::Fit => Self::fir_resize(&img, w, h, filter)?,
                // Cover the box, preserving aspect ratio, then crop the
                // overflow. `resize_to_fill` already crops to exactly
                // `w x h` internally (verified against image-0.25.10's
                // `DynamicImage::resize_to_fill`,
                // `image-0.25.10/src/images/dynimage.rs:943-962`, which
                // calls `.crop(...)` on the scaled image before
                // returning), so no separate manual crop step is needed
                // here (#36).
                ResizeType::Fill => Self::fir_resize_to_fill(&img, w, h, filter)?,
                // Stretch to exactly `w x h`, ignoring aspect ratio.
                ResizeType::Force => Self::fir_resize_exact(&img, w, h, filter)?,
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
                        Self::fir_resize_to_fill(&img, w, h, filter)?
                    } else {
                        Self::fir_resize(&img, w, h, filter)?
                    }
                }
            },
            (None, None) => img,
        };

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

        // Optimize encoding based on format
        // #53: `gen_server` (OpenAPI codegen) was deleted; `ImageFormat` is
        // now hand-written in `src/models/params.rs`. Mechanical path
        // change only - same three variants, no logic here changed.
        let (output_format, content_type) = match params.format {
            crate::models::params::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            crate::models::params::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            crate::models::params::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
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
                // PNG/WebP keep their alpha channel, so this is not a
                // flatten - but a fully-transparent pixel's RGB is
                // invisible by definition, and the source frequently
                // carries undefined/noisy values there that cost real
                // encoded bytes for a region nobody can see (#60).
                // Normalising just those pixels to a constant lets
                // DEFLATE/VP8L collapse the region instead. Only exactly
                // `alpha == 0` pixels are touched - partial transparency is
                // visibly blended with whatever is behind it, so rewriting
                // its RGB would be a real (lossy) visual change, not the
                // lossless one this is meant to be.
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
        // `params.quality`/`params.jpeg_quality` actually reach the encoder.
        //
        // PNG has no quality knob in `params` to honour - `CompressionType`
        // is a fixed lossless setting, not a continuous 0-100 scale, and
        // `fq:png:N` is rejected at parse time
        // (`src/modules/url/options.rs`) rather than silently accepted and
        // ignored here.
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
                let estimated_size = Self::estimate_output_size(&img, &output_format);
                let mut buf = Cursor::new(Vec::with_capacity(estimated_size));

                let encoder = image::codecs::png::PngEncoder::new_with_quality(
                    &mut buf,
                    image::codecs::png::CompressionType::Best,
                    image::codecs::png::FilterType::Adaptive,
                );
                img.write_with_encoder(encoder)
                    .context(format!("Failed to encode image to {:?}", output_format))?;

                buf.into_inner()
            }
            ImageFormat::Jpeg => {
                let quality = params
                    .jpeg_quality
                    .or(params.quality)
                    .unwrap_or(DEFAULT_JPEG_QUALITY);

                let estimated_size = Self::estimate_output_size(&img, &output_format);
                let mut buf = Cursor::new(Vec::with_capacity(estimated_size));

                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
                img.write_with_encoder(encoder)
                    .context(format!("Failed to encode image to {:?}", output_format))?;

                buf.into_inner()
            }
            // `output_format` is only ever constructed from `params.format`
            // (`crate::models::params::ImageFormat`, three variants: `Jpg`,
            // `Png`, `Webp`) a few lines above, so every value the `match`
            // above doesn't already handle is unreachable in practice - kept
            // as a hard error rather than a silent generic `write_to` fallback
            // so a future fourth format doesn't quietly skip quality handling.
            other => anyhow::bail!("unsupported output format {other:?}"),
        };

        Ok((output_bytes, content_type.to_string()))
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
    fn resize_dimensions(width: u32, height: u32, nwidth: u32, nheight: u32, fill: bool) -> (u32, u32) {
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
            (
                (f64::from(width) * ratio).round().max(1.0) as u32,
                u32::MAX,
            )
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
    /// `DynamicImage::resize_to_fill` (cover `nwidth x nheight`,
    /// preserving aspect ratio, then centre-crop the overflow) - reuses
    /// `image`'s own `DynamicImage::crop` for the crop step (untouched by
    /// this change) and reproduces `resize_to_fill`'s exact crop-offset
    /// arithmetic (image-0.25.10 `src/images/dynimage.rs:943-962`) so the
    /// output is pixel-region-identical to before, just resampled by
    /// `fast_image_resize` (#63 stage 1).
    fn fir_resize_to_fill(
        img: &DynamicImage,
        nwidth: u32,
        nheight: u32,
        filter: FilterType,
    ) -> Result<DynamicImage> {
        let (width2, height2) =
            Self::resize_dimensions(img.width(), img.height(), nwidth, nheight, true);
        let mut intermediate = Self::fir_resize_exact(img, width2, height2, filter)?;
        let (iwidth, iheight) = intermediate.dimensions();
        let ratio = u64::from(iwidth) * u64::from(nheight);
        let nratio = u64::from(nwidth) * u64::from(iheight);

        Ok(if nratio > ratio {
            intermediate.crop(0, (iheight - nheight) / 2, nwidth, nheight)
        } else {
            intermediate.crop((iwidth - nwidth) / 2, 0, nwidth, nheight)
        })
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

    /// Rejects a request whose requested output width/height exceed the
    /// configured maximum, independent of whatever the generated OpenAPI
    /// layer does or does not validate upstream (#26).
    fn check_output_dimensions(params: &ResizeQuery, config: &PerformanceConfig) -> Result<()> {
        if let Some(width) = params.width {
            if width > config.max_output_width {
                anyhow::bail!(
                    "Requested output dimensions too large: width {width} exceeds maximum {}",
                    config.max_output_width
                );
            }
        }

        if let Some(height) = params.height {
            if height > config.max_output_height {
                anyhow::bail!(
                    "Requested output dimensions too large: height {height} exceeds maximum {}",
                    config.max_output_height
                );
            }
        }

        Ok(())
    }

    /// Rejects a decoded *source* resolution above `max_src_resolution_mp`
    /// megapixels. `width`/`height` here come from a header-only peek, not
    /// a full decode - see `peek_dimensions`.
    fn check_source_resolution(width: u32, height: u32, max_src_resolution_mp: u64) -> Result<()> {
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
    fn peek_dimensions(image_bytes: &[u8], format: Option<ImageFormat>) -> Result<(u32, u32)> {
        Self::make_reader(image_bytes, format)?
            .into_dimensions()
            .context("Failed to read image dimensions")
    }

    /// Decodes `image_bytes` with explicit `image::Limits` derived from
    /// `max_src_resolution_mp`, instead of inheriting the crate's
    /// accidental 512MiB `max_alloc` default (#26). This is defense in
    /// depth behind `check_source_resolution`'s header-only check, not a
    /// replacement for it.
    fn decode_with_limits(
        image_bytes: &[u8],
        format: Option<ImageFormat>,
        max_src_resolution_mp: u64,
    ) -> Result<image::DynamicImage> {
        let mut reader = Self::make_reader(image_bytes, format)?;
        reader.limits(Self::build_decode_limits(max_src_resolution_mp));
        reader.decode().context("Failed to decode image")
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
            _ => {
                // Check for WebP
                if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                    Some(ImageFormat::WebP)
                } else {
                    None
                }
            }
        }
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
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background: None,
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
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background,
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
        assert!(b > 200, "expected high blue near a blue background, got {b}");
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

    /// Lossless WebP (`webp_lossless: Some(true)`) must round-trip the
    /// source pixels exactly - no resize/blur/grayscale filter applied, and
    /// a source with no alpha channel so the #34/#60 flatten/normalise
    /// stage is a no-op, isolating this to purely the encoder's own
    /// lossless-ness.
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

        let original = image::load_from_memory(&bytes)
            .expect("source should decode")
            .to_rgba8();
        let decoded = image::load_from_memory(&output)
            .expect("lossless webp output should decode")
            .to_rgba8();

        assert_eq!(decoded.dimensions(), original.dimensions());
        assert_eq!(
            decoded.as_raw(),
            original.as_raw(),
            "lossless webp round-trip must be byte-identical to the source pixels"
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
}
