//! Encode-stage benchmarks: each output format the service supports.
//!
//! PNG calls `DynamicImage::write_to` - the same call
//! `ImageService::process_image_blocking` (src/services/image/handler.rs)
//! makes for that format's final encode step. WebP instead calls
//! `ImageService::encode_webp`, the dedicated lossy-WebP path (via the
//! `webp` crate) that `process_image_blocking` actually uses - the `image`
//! crate's own WebP encoder that `write_to` would reach is lossless-only,
//! so benchmarking it here would measure the wrong encoder. JPEG (#76)
//! similarly goes through `ImageService::encode_jpeg` (mozjpeg/
//! libjpeg-turbo) rather than `write_to` - `image`'s own JPEG encoder has
//! no progressive-mode switch and hardcodes 4:2:2 chroma subsampling, so
//! production routes JPEG through mozjpeg instead (see `encode_jpeg`'s own
//! doc comment). `jpeg_baseline`/`jpeg_progressive` below measure exactly
//! that production path, both the encode-time cost and (via
//! `benches/fixtures.rs`'s corpus, cross-checked against the in-repo
//! `photo_like` fixture) the output-size delta #76's issue claims
//! ("typically 2-10% smaller") - see this change's own report for the
//! actual measured numbers on this corpus.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::services::image::handler::{DEFAULT_JPEG_QUALITY, DEFAULT_WEBP_QUALITY, ImageService};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

const FORMATS: [(&str, ImageFormat); 2] = [("png", ImageFormat::Png), ("webp", ImageFormat::WebP)];

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

    for (name, format) in FORMATS {
        group.bench_with_input(BenchmarkId::from_parameter(name), &format, |b, &format| {
            b.iter(|| {
                if format == ImageFormat::WebP {
                    ImageService::encode_webp(&img, DEFAULT_WEBP_QUALITY, false)
                        .expect("encode fixture")
                } else {
                    let mut buf = Cursor::new(Vec::new());
                    img.write_to(&mut buf, format).expect("encode fixture");
                    buf.into_inner()
                }
            });
        });
    }

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
                    )
                    .expect("encode fixture")
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
