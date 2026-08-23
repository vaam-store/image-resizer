//! Decode-stage benchmarks: JPEG / PNG / WebP / AVIF at a few resolutions.
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
//! comment). Fixtures come from `fixtures::photo_like_sized_avif`, which
//! encodes via `avif_codec::encode` directly since `image::write_to` can't
//! produce AVIF any more either.
//!
//! # A known caveat this group inherits, not introduces
//!
//! `fixtures::photo_like_sized`'s `gradient_noise_rgb` content (smooth
//! gradient + i.i.d. per-pixel noise) is exactly the synthetic-fixture
//! shape `adr/0001-image-engine.md`/`adr/0003-webp-measurement.md` already
//! flagged as unrepresentative for *encoder* size comparisons - high-
//! frequency noise compresses close to a shared incompressible floor
//! regardless of which codec is actually better on real photographic
//! content. The same flaw turns out to affect *decode speed* too, not
//! just encoded size: on this synthetic fixture, `decode/webp` measured
//! `libwebp_decode` (#66) ~34% *slower* than the pre-#66 `image-webp`
//! decoder it replaced; on the real Kodak photo corpus (24 real
//! photographs, a standalone scratch-crate measurement - not this bench),
//! `libwebp_decode` measured ~2.24x *faster*, with DSSIM delta
//! 0.00000000 (pixel-identical) against `image-webp`'s output on every
//! image. See this change's own report for that measurement in full; the
//! real-photo number is the one #66's implementation was justified
//! against, not this bench's synthetic one. This bench is kept (and
//! extended to WebP/AVIF) for its original purpose - tracking relative
//! regressions in *this exact synthetic fixture* across future changes -
//! not as a source of truth for cross-codec real-world speed comparisons.

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
        let bytes = fixtures::photo_like_sized(w, h, ImageFormat::Jpeg);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("jpeg", format!("{w}x{h}")),
            &bytes,
            |b, bytes| {
                b.iter(|| ImageService::mozjpeg_decode(bytes, 8).expect("decode fixture"));
            },
        );
    }

    for (w, h) in SIZES {
        let bytes = fixtures::photo_like_sized(w, h, ImageFormat::Png);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("png", format!("{w}x{h}")),
            &bytes,
            |b, bytes| {
                b.iter(|| {
                    image::load_from_memory_with_format(bytes, ImageFormat::Png)
                        .expect("decode fixture")
                });
            },
        );
    }

    for (w, h) in SIZES {
        let bytes = fixtures::photo_like_sized(w, h, ImageFormat::WebP);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("webp", format!("{w}x{h}")),
            &bytes,
            |b, bytes| {
                b.iter(|| ImageService::libwebp_decode(bytes).expect("decode fixture"));
            },
        );
    }

    for (w, h) in SIZES {
        let bytes = fixtures::photo_like_sized_avif(w, h, |img| {
            avif_codec::encode(
                img,
                emgr::services::image::handler::DEFAULT_AVIF_QUALITY,
                emgr::services::image::handler::DEFAULT_AVIF_SPEED,
                None,
            )
            .expect("AVIF fixture encoding should never fail")
        });
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("avif", format!("{w}x{h}")),
            &bytes,
            |b, bytes| {
                b.iter(|| avif_codec::decode(bytes, 50).expect("decode fixture"));
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
