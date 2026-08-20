//! Encode-stage benchmarks: each output format the service supports.
//!
//! JPEG/PNG call `DynamicImage::write_to` - the same call
//! `ImageService::process_image_blocking` (src/services/image/handler.rs)
//! makes for those formats' final encode step. WebP instead calls
//! `ImageService::encode_webp`, the dedicated lossy-WebP path (via the
//! `webp` crate) that `process_image_blocking` actually uses - the `image`
//! crate's own WebP encoder that `write_to` would reach is lossless-only,
//! so benchmarking it here would measure the wrong encoder.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::services::image::handler::{DEFAULT_WEBP_QUALITY, ImageService};
use image::{DynamicImage, ImageFormat};
use std::io::Cursor;

const FORMATS: [(&str, ImageFormat); 3] = [
    ("jpeg", ImageFormat::Jpeg),
    ("png", ImageFormat::Png),
    ("webp", ImageFormat::WebP),
];

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

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
