//! Decode-stage benchmarks: JPEG / PNG / WebP / AVIF at a few resolutions,
//! each on two fixture kinds - `synthetic` (`fixtures::photo_like_sized`,
//! kept below) and `photo` (`fixtures::real_photo_sized`, real NASA
//! photographs - see `benches/fixtures/real/ATTRIBUTION.md`).
//!
//! PNG calls `image::load_from_memory_with_format`, the exact call
//! `ImageService::decode_with_image_crate` (src/services/image/handler.rs)
//! makes for that format - i.e. this measures the real decode cost the
//! service pays, not a synthetic stand-in.
//!
//! JPEG instead calls `ImageService::mozjpeg_decode(&bytes, 8)` (#67):
//! every JPEG decode, DCT-scaled or not, now goes through mozjpeg rather
//! than `image`-crate/zune-jpeg - see that function's own doc comment and
//! `decode_jpeg_scaled`'s retired-rationale comment for why. `scale_num =
//! 8` is "no DCT reduction," matching this bench's full-size decode (no
//! resize is requested here), the same call `decode_jpeg_scaled` makes when
//! `select_jpeg_dct_scale` picks no scaling.
//!
//! WebP calls `ImageService::libwebp_decode` (#66) - real libwebp via the
//! `webp` crate, not `image-webp`'s pure-Rust decoder `write_to`/
//! `load_from_memory_with_format` would reach; see that function's own doc
//! comment and `decode_webp_libwebp`'s for why production no longer uses
//! the pure-Rust path.
//!
//! AVIF (#67) calls `avif_codec::decode` - `libavif`+dav1d, this crate's
//! only AVIF decode path (`image`'s own decoder needs the separate
//! `avif-native` feature, not enabled - see `avif_codec`'s own module doc
//! comment). Fixtures come from `fixtures::photo_like_sized_avif`/
//! `fixtures::real_photo_sized_avif`, which encode via `avif_codec::encode`
//! directly since `image::write_to` can't produce AVIF any more either.
//!
//! # Why both a synthetic and a real fixture, not just one
//!
//! `fixtures::photo_like_sized`'s `gradient_noise_rgb` content (smooth
//! gradient + i.i.d. per-pixel noise) is exactly the synthetic-fixture
//! shape `adr/0001-image-engine.md`/`adr/0003-webp-measurement.md`/
//! `adr/0004-avif-measurement.md` all flagged as unrepresentative -
//! high-frequency noise compresses close to a shared incompressible floor
//! regardless of which codec is actually better on real photographic
//! content, and that turns out to distort *decode speed* too, not just
//! encoded size. Two confirmed, reproduced-in-this-repo examples (see this
//! change's own report for the exact numbers from this bench, run on this
//! commit):
//!
//! - **WebP decode direction inverts.** On the synthetic fixture,
//!   `libwebp_decode` (#66) measures *slower* than the pure-Rust
//!   `image-webp` decoder it replaced. On the real photo fixture, it's
//!   faster - matching the real-corpus scratch-crate measurement #66's
//!   implementation was actually justified against (24 Kodak photos,
//!   ~2.24x faster, DSSIM 0.00000000 - pixel-identical).
//! - **AVIF decode is inflated roughly an order of magnitude.** Noise
//!   doesn't compress, so there's far more residual for the decoder to
//!   walk through than a real photo at the same encoded quality produces.
//!
//! The synthetic fixture is **not deleted** - it's still a legitimate
//! worst case (nothing stops a client from uploading noise-like content:
//! screenshots of already-compressed video frames, camera sensor noise in
//! low light, adversarial input) and this bench group has tracked relative
//! regressions against it since before this change. It's just no longer
//! the *only* thing measured - see the `synthetic/*` vs `photo/*` case
//! names below.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use emgr::services::image::avif_codec;
use emgr::services::image::handler::ImageService;
use image::ImageFormat;

const SIZES: [(u32, u32); 3] = [(640, 360), (1280, 720), (1920, 1080)];

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");

    for (w, h) in SIZES {
        let synthetic = fixtures::photo_like_sized(w, h, ImageFormat::Jpeg);
        let photo = fixtures::real_photo_sized(w, h, ImageFormat::Jpeg);
        for (kind, bytes) in [("synthetic", &synthetic), ("photo", &photo)] {
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new("jpeg", format!("{kind}/{w}x{h}")),
                bytes,
                |b, bytes| {
                    b.iter(|| ImageService::mozjpeg_decode(bytes, 8).expect("decode fixture"));
                },
            );
        }
    }

    for (w, h) in SIZES {
        let synthetic = fixtures::photo_like_sized(w, h, ImageFormat::Png);
        let photo = fixtures::real_photo_sized(w, h, ImageFormat::Png);
        for (kind, bytes) in [("synthetic", &synthetic), ("photo", &photo)] {
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new("png", format!("{kind}/{w}x{h}")),
                bytes,
                |b, bytes| {
                    b.iter(|| {
                        image::load_from_memory_with_format(bytes, ImageFormat::Png)
                            .expect("decode fixture")
                    });
                },
            );
        }
    }

    for (w, h) in SIZES {
        let synthetic = fixtures::photo_like_sized(w, h, ImageFormat::WebP);
        let photo = fixtures::real_photo_sized(w, h, ImageFormat::WebP);
        for (kind, bytes) in [("synthetic", &synthetic), ("photo", &photo)] {
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new("webp", format!("{kind}/{w}x{h}")),
                bytes,
                |b, bytes| {
                    b.iter(|| ImageService::libwebp_decode(bytes).expect("decode fixture"));
                },
            );
        }
    }

    for (w, h) in SIZES {
        let synthetic = fixtures::photo_like_sized_avif(w, h, |img| {
            avif_codec::encode(
                img,
                emgr::services::image::handler::DEFAULT_AVIF_QUALITY,
                emgr::services::image::handler::DEFAULT_AVIF_SPEED,
                None,
            )
            .expect("AVIF fixture encoding should never fail")
        });
        let photo = fixtures::real_photo_sized_avif(w, h, |img| {
            avif_codec::encode(
                img,
                emgr::services::image::handler::DEFAULT_AVIF_QUALITY,
                emgr::services::image::handler::DEFAULT_AVIF_SPEED,
                None,
            )
            .expect("AVIF fixture encoding should never fail")
        });
        for (kind, bytes) in [("synthetic", &synthetic), ("photo", &photo)] {
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new("avif", format!("{kind}/{w}x{h}")),
                bytes,
                |b, bytes| {
                    b.iter(|| avif_codec::decode(bytes, 50).expect("decode fixture"));
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
