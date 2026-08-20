//! Encode-stage benchmarks: each output format the service supports.
//!
//! Calls `DynamicImage::write_to`, the same call
//! `ImageService::process_image_blocking` (src/services/image/handler.rs)
//! makes for its final encode step.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
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
                let mut buf = Cursor::new(Vec::new());
                img.write_to(&mut buf, format).expect("encode fixture");
                buf.into_inner()
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
