//! Encode-stage benchmarks: each output format the service supports.
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
//! below now calls that exact encoder configuration; `png_default` is kept
//! alongside it, clearly labelled, so the cost of the `Best` setting stays
//! visible instead of turning into an unexplained jump. There is no
//! dedicated `ImageService::encode_png` function to call directly the way
//! the WebP/JPEG cases below do (the PNG encoder is built inline inside
//! `encode_single_image`, not factored out) - see this change's own report
//! for a follow-up suggestion to extract one so this can't drift again
//! silently.
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
//! AVIF and GIF are not benched here at all - not a "measures the wrong
//! thing" defect like PNG was, just missing coverage. GIF keeps going
//! through plain `write_to` in production (undisturbed since before #35/
//! #33/#49 carved out explicit encoders for the other formats), so a GIF
//! case here would in fact match `write_to` - it's simply not present yet.
//! AVIF goes through `AvifEncoder::new_with_speed_quality` with its own
//! quality/speed defaults and has no case here either. Both are flagged in
//! this change's report rather than added, to keep this change scoped to
//! fixing the PNG defect and auditing for others of the same kind.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::services::image::avif_codec;
use emgr::services::image::handler::{
    DEFAULT_AVIF_QUALITY, DEFAULT_AVIF_SPEED, DEFAULT_JPEG_QUALITY, DEFAULT_WEBP_QUALITY,
    ImageService,
};
use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

/// A resized (800x450) photo-like image, representative of what actually
/// reaches the encode step in production (post-download, post-resize).
fn source_image() -> DynamicImage {
    let bytes = fixtures::photo_like();
    image::load_from_memory_with_format(&bytes, ImageFormat::Jpeg)
        .expect("decode photo_like fixture")
        .resize(800, 450, image::imageops::FilterType::Triangle)
}

fn bench_encode(c: &mut Criterion) {
    let img = source_image();
    let mut group = c.benchmark_group("encode");

    // `png_default`: the `image` crate's default `CompressionType` (`Fast`)
    // via `write_to`. Not what production runs - kept only so the cost of
    // `Best` below stays visible as a delta rather than an unexplained
    // number.
    group.bench_function(BenchmarkId::from_parameter("png_default"), |b| {
        b.iter(|| {
            let mut buf = Cursor::new(Vec::new());
            img.write_to(&mut buf, ImageFormat::Png)
                .expect("encode fixture");
            buf.into_inner()
        });
    });

    // `png_best`: exactly `encode_single_image`'s `ImageFormat::Png` arm
    // (`src/services/image/handler.rs`) - `CompressionType::Best` +
    // `FilterType::Adaptive`, no ICC/EXIF payload set (this fixture carries
    // neither, matching the common case those calls are skipped for). This
    // is the production path; see the module doc comment above for why it's
    // duplicated here rather than called through a shared function.
    group.bench_function(BenchmarkId::from_parameter("png_best"), |b| {
        b.iter(|| {
            let mut buf = Cursor::new(Vec::new());
            let encoder = PngEncoder::new_with_quality(
                &mut buf,
                CompressionType::Best,
                PngFilterType::Adaptive,
            );
            img.write_with_encoder(encoder).expect("encode fixture");
            buf.into_inner()
        });
    });

    group.bench_function(BenchmarkId::from_parameter("webp"), |b| {
        b.iter(|| {
            ImageService::encode_webp(&img, DEFAULT_WEBP_QUALITY, false).expect("encode fixture")
        });
    });

    // #76: JPEG now goes through `ImageService::encode_jpeg` (mozjpeg), not
    // `write_to` - see this file's own doc comment. Four variants cover the
    // two knobs #76 adds, each independently: baseline (pre-#76-equivalent
    // default: 4:2:2, sequential), progressive (4:2:2 + progressive scans),
    // and 4:4:4 (no_subsampling) at both scan modes, so the encode-time and
    // output-size cost of each knob can be read off independently rather
    // than conflated into one number.
    for (name, progressive, no_subsampling) in [
        ("jpeg_baseline", false, false),
        ("jpeg_progressive", true, false),
        ("jpeg_444", false, true),
        ("jpeg_444_progressive", true, true),
    ] {
        group.bench_with_input(
            BenchmarkId::from_parameter(name),
            &(progressive, no_subsampling),
            |b, &(progressive, no_subsampling)| {
                b.iter(|| {
                    ImageService::encode_jpeg(
                        &img,
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
    group.bench_with_input(BenchmarkId::from_parameter("avif"), &img, |b, img| {
        b.iter(|| {
            avif_codec::encode(img, DEFAULT_AVIF_QUALITY, DEFAULT_AVIF_SPEED, None)
                .expect("encode fixture")
        });
    });

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
