use crate::config::performance::PerformanceConfig;
use crate::models::params::ResizeQuery;
use crate::services::image::source_guard;
use anyhow::{Context, Result};
use bytes::Bytes;
use derive_builder::Builder;
use futures::StreamExt;
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use reqwest::redirect::Policy;
use reqwest::{Client, Response};
use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Semaphore;
use url::Url;

#[derive(Clone, Builder)]
pub struct ImageService {
    // Limit concurrent downloads to prevent memory exhaustion
    download_semaphore: Arc<Semaphore>,
    // Custom thread pool for CPU-intensive work
    cpu_pool: Arc<rayon::ThreadPool>,
    config: PerformanceConfig,
}

impl ImageService {
    pub fn new() -> Result<Self> {
        Self::with_config(PerformanceConfig::default())
    }

    pub fn with_config(config: PerformanceConfig) -> Result<Self> {
        // Limit concurrent downloads based on configuration
        let download_semaphore = Arc::new(Semaphore::new(config.max_concurrent_downloads));

        // Create custom thread pool for CPU work
        let cpu_pool_size = config.get_cpu_thread_pool_size();
        let cpu_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(cpu_pool_size)
                .thread_name(|i| format!("image-cpu-{}", i))
                .build()
                .context("Failed to create CPU thread pool")?,
        );

        Ok(Self {
            download_semaphore,
            cpu_pool,
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
    async fn fetch_validated(&self, url: &str) -> Result<Response> {
        let mut current = Url::parse(url).context("Invalid source URL")?;

        // `max_redirects` redirects means `max_redirects + 1` requests: the
        // original attempt plus up to `max_redirects` hops.
        for _ in 0..=self.config.max_redirects {
            source_guard::validate_scheme(&current)?;

            if let Some(allowed) = &self.config.allowed_sources {
                if !allowed.is_empty() && !source_guard::is_allowed_source(&current, allowed) {
                    return Err(source_guard::SourceRejected::NotAllowlisted {
                        url: current.to_string(),
                    }
                    .into());
                }
            }

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
    pub async fn download_image(&self, url: &str) -> Result<Vec<u8>> {
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
        let mut buffer: Vec<u8> = Vec::with_capacity(capacity_hint);
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

        Ok(buffer)
    }

    /// Process image using custom thread pool with CPU affinity
    pub async fn process_image(
        &self,
        image_bytes: &[u8],
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        let image_bytes = Bytes::copy_from_slice(image_bytes);
        let params = params.clone();
        let cpu_pool = Arc::clone(&self.cpu_pool);
        let config = self.config.clone();

        // Use custom thread pool instead of tokio's spawn_blocking
        let (tx, rx) = tokio::sync::oneshot::channel();

        cpu_pool.spawn(move || {
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
        Self::process_image_blocking_with_limits(
            image_bytes,
            params,
            &PerformanceConfig::default(),
        )
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

        // Use faster resize algorithms for different scenarios
        let filter = match (params.width, params.height) {
            // For thumbnails, use faster Triangle filter
            (Some(w), Some(h)) if w <= 300 && h <= 300 => FilterType::Triangle,
            // For high quality, use Lanczos3
            _ => FilterType::Lanczos3,
        };

        // Resize image with optimized logic
        let img = match (params.width, params.height) {
            (Some(w), None) => img.resize(w, u32::MAX, filter),
            (None, Some(h)) => img.resize(u32::MAX, h, filter),
            (Some(w), Some(h)) => {
                // Optimize resize-to-fill + crop operation
                let img = img.resize_to_fill(w, h, filter);
                let (current_width, current_height) = img.dimensions();

                if current_width == w && current_height == h {
                    img // No cropping needed
                } else {
                    let crop_x = (current_width.saturating_sub(w)) / 2;
                    let crop_y = (current_height.saturating_sub(h)) / 2;
                    img.crop_imm(crop_x, crop_y, w.min(current_width), h.min(current_height))
                }
            }
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
        let (output_format, content_type) = match params.format {
            gen_server::models::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            gen_server::models::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            gen_server::models::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
        };

        // Pre-allocate buffer based on estimated size
        let estimated_size = Self::estimate_output_size(&img, &output_format);
        let mut output_bytes = Cursor::new(Vec::with_capacity(estimated_size));

        img.write_to(&mut output_bytes, output_format)
            .context(format!("Failed to encode image to {:?}", output_format))?;

        Ok((output_bytes.into_inner(), content_type.to_string()))
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
    use gen_server::models::ImageFormat as ApiImageFormat;

    fn query(width: Option<u32>, height: Option<u32>) -> ResizeQuery {
        ResizeQuery {
            url: "https://images.example.com/photo.jpg".to_string(),
            width,
            height,
            format: ApiImageFormat::Jpg,
            blur_sigma: None,
            grayscale: None,
        }
    }

    #[test]
    fn output_dimensions_within_limits_pass() {
        let config = PerformanceConfig::default();
        assert!(ImageService::check_output_dimensions(&query(Some(800), Some(600)), &config).is_ok());
        assert!(ImageService::check_output_dimensions(&query(None, None), &config).is_ok());
    }

    #[test]
    fn output_width_over_limit_is_rejected() {
        let config = PerformanceConfig::default();
        let err = ImageService::check_output_dimensions(&query(Some(5000), None), &config)
            .unwrap_err();
        assert!(err.to_string().to_lowercase().contains("too large"));
    }

    #[test]
    fn output_height_over_limit_is_rejected() {
        let config = PerformanceConfig::default();
        let err = ImageService::check_output_dimensions(&query(None, Some(5000)), &config)
            .unwrap_err();
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
            max_image_size: 64 * 1024, // 64KB - far under the ~8MB on offer
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
            err.to_string().to_lowercase().contains("too many redirects"),
            "unexpected error: {err}"
        );

        server.abort();
    }
}
