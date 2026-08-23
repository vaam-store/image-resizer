//! Encode-stage benchmarks: each output format the service supports, on
//! two source-image kinds - `synthetic` (`fixtures::photo_like`, gradient +
//! i.i.d. noise) and `photo` (`fixtures::real_photo_sized`, a real NASA
//! photograph - see `benches/fixtures/real/ATTRIBUTION.md`). See
//! `benches/decode.rs`'s module doc comment for why both exist rather than
//! just the synthetic one: noise compresses toward an incompressible floor
//! that flattens real differences between codecs, and that distortion
//! turns out to affect encode cost too, not just size.
//!
//! **PNG trap, found and fixed 2026-08-23 (see `.bench-baseline/BASELINE.md`'s
//! "PNG encode correction" section):** this file used to encode PNG via
//! `DynamicImage::write_to`, which uses the `image` crate's *default*
//! `CompressionType` (`Fast`). Production does not use that. #60's
//! `encode_single_image` (`src/services/image/handler.rs`, the `ImageFormat::Png`
//! match arm) builds an explicit
//! `PngEncoder::new_with_quality(.., CompressionType::Best, FilterType::Adaptive)`
//! - confirmed by reading that arm directly, not assumed - specifically to
//! trade encode time for a real size win. Every historical `encode/png`
//! number in the baseline file therefore describes a code path that never
//! executes in production, off by roughly 40x on this corpus. `png_best`
//! below now calls `ImageService::encode_png` directly, the same way the
//! WebP/JPEG cases below call `encode_webp`/`encode_jpeg` - that inline
//! `PngEncoder` construction has since been extracted out of
//! `encode_single_image` into its own `pub fn encode_png`, so this case no
//! longer duplicates the `CompressionType::Best`/`FilterType::Adaptive`
//! settings by hand and can't drift from production again the way it did
//! here. `png_default` is kept alongside it, clearly labelled, so the cost
//! of the `Best` setting stays visible instead of turning into an
//! unexplained jump.
//!
//! WebP calls `ImageService::encode_webp`, the dedicated lossy-WebP path
//! (via the `webp` crate) that `process_image_blocking` actually uses - the
//! `image` crate's own WebP encoder that `write_to` would reach is
//! lossless-only, so benchmarking it here would measure the wrong encoder.
//! JPEG (#76) similarly goes through `ImageService::encode_jpeg` (mozjpeg/
//! libjpeg-turbo) rather than `write_to` - `image`'s own JPEG encoder has
//! no progressive-mode switch and hardcodes 4:2:2 chroma subsampling, so
//! production routes JPEG through mozjpeg instead (see `encode_jpeg`'s own
//! doc comment). `jpeg_baseline`/`jpeg_progressive` below measure exactly
//! that production path, both the encode-time cost and (via
//! `benches/fixtures.rs`'s corpus, cross-checked against the in-repo
//! `photo_like` fixture) the output-size delta #76's issue claims
//! ("typically 2-10% smaller") - see this change's own report for the
//! actual measured numbers on this corpus.
//!
//! GIF is not benched here at all - not a "measures the wrong thing" defect
//! like PNG was, just missing coverage. GIF keeps going through plain
//! `write_to` in production (undisturbed since before #35/#33/#49 carved
//! out explicit encoders for the other formats), so a GIF case here would
//! in fact match `write_to` - it's simply not present yet, flagged rather
//! than added to keep this change scoped.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::services::image::avif_codec;
use emgr::services::image::handler::{
    DEFAULT_AVIF_QUALITY, DEFAULT_AVIF_SPEED, DEFAULT_JPEG_QUALITY, DEFAULT_WEBP_QUALITY,
    ImageService,
};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

/// The two source-image kinds this file benches every encoder against.
const KINDS: [&str; 2] = ["synthetic", "photo"];

/// A resized (800x450) source image, representative of what actually
/// reaches the encode step in production (post-download, post-resize).
/// `"synthetic"` resizes the in-repo generated `photo_like` fixture with a
/// plain `Triangle` filter (as before this change); `"photo"` goes through
/// `fixtures::real_photo_sized`, which cover-crops the embedded real NASA
/// photograph (Lanczos3) to the same 800x450 box - see
/// `benches/fixtures/real/ATTRIBUTION.md`.
fn source_image(kind: &str) -> DynamicImage {
    match kind {
        "synthetic" => {
            let bytes = fixtures::photo_like();
            image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
                .expect("decode photo_like fixture")
                .resize(800, 450, image::imageops::FilterType::Triangle)
        }
        "photo" => {
            let bytes = fixtures::real_photo_sized(800, 450, ImageFormat::Jpeg);
            image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
                .expect("decode real_photo fixture")
        }
        other => panic!("unknown source kind {other:?} (expected \"synthetic\" or \"photo\")"),
    }
}

fn bench_encode(c: &mut Criterion) {
    let imgs: Vec<(&str, DynamicImage)> = KINDS.iter().map(|&k| (k, source_image(k))).collect();
    let mut group = c.benchmark_group("encode");

    for (kind, img) in &imgs {
        // `png_default`: the `image` crate's default `CompressionType`
        // (`Fast`) via `write_to`. Not what production runs - kept only so
        // the cost of `Best` below stays visible as a delta rather than an
        // unexplained number.
        group.bench_function(BenchmarkId::new("png_default", *kind), |b| {
            b.iter(|| {
                let mut buf = Cursor::new(Vec::new());
                img.write_to(&mut buf, ImageFormat::Png)
                    .expect("encode fixture");
                buf.into_inner()
            });
        });

        // `png_best`: `ImageService::encode_png`, the exact function
        // `encode_single_image`'s `ImageFormat::Png` arm
        // (`src/services/image/handler.rs`) calls - `CompressionType::Best`
        // + `FilterType::Adaptive`, no ICC/EXIF payload here (`None`/`None`,
        // matching the common case where those calls are skipped). This is
        // the production path, reached through the same shared function
        // production uses - see the module doc comment above for why that
        // matters.
        group.bench_function(BenchmarkId::new("png_best", *kind), |b| {
            b.iter(|| ImageService::encode_png(img, None, None).expect("encode fixture"));
        });

        group.bench_function(BenchmarkId::new("webp", *kind), |b| {
            b.iter(|| {
                ImageService::encode_webp(img, DEFAULT_WEBP_QUALITY, false)
                    .expect("encode fixture")
            });
        });

        // #76: JPEG now goes through `ImageService::encode_jpeg` (mozjpeg),
        // not `write_to` - see this file's own doc comment. Four variants
        // cover the two knobs #76 adds, each independently: baseline
        // (pre-#76-equivalent default: 4:2:2, sequential), progressive
        // (4:2:2 + progressive scans), and 4:4:4 (no_subsampling) at both
        // scan modes, so the encode-time and output-size cost of each knob
        // can be read off independently rather than conflated into one
        // number.
        for (name, progressive, no_subsampling) in [
            ("jpeg_baseline", false, false),
            ("jpeg_progressive", true, false),
            ("jpeg_444", false, true),
            ("jpeg_444_progressive", true, true),
        ] {
            group.bench_with_input(
                BenchmarkId::new(name, *kind),
                &(progressive, no_subsampling),
                |b, &(progressive, no_subsampling)| {
                    b.iter(|| {
                        ImageService::encode_jpeg(
                            img,
                            DEFAULT_JPEG_QUALITY,
                            progressive,
                            no_subsampling,
                            None,
                            None,
                        )
                        .expect("encode fixture")
                    });
                },
            );
        }

        // #68: AVIF via `avif_codec::encode` (`libavif`+AOM), replacing
        // `image::codecs::avif::AvifEncoder` (`ravif`/`rav1e`, removed
        // entirely) - see that function's own doc comment and
        // `DEFAULT_AVIF_SPEED`'s in `handler.rs` for why its value changed
        // from what `ravif` used.
        group.bench_with_input(BenchmarkId::new("avif", *kind), img, |b, img| {
            b.iter(|| {
                avif_codec::encode(img, DEFAULT_AVIF_QUALITY, DEFAULT_AVIF_SPEED, None)
                    .expect("encode fixture")
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
