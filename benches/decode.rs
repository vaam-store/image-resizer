//! Decode-stage benchmarks: JPEG / PNG / WebP at a few resolutions.
//!
//! This calls `image::load_from_memory_with_format`, the exact call
//! `ImageService::process_image_blocking` (src/services/image/handler.rs)
//! makes once it has picked a format hint - i.e. this measures the real
//! decode cost the service pays, not a synthetic stand-in.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use image::ImageFormat;

const SIZES: [(u32, u32); 3] = [(640, 360), (1280, 720), (1920, 1080)];
const FORMATS: [(&str, ImageFormat); 3] = [
    ("jpeg", ImageFormat::Jpeg),
    ("png", ImageFormat::Png),
    ("webp", ImageFormat::WebP),
];

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");

    for (fmt_name, format) in FORMATS {
        for (w, h) in SIZES {
            let bytes = fixtures::photo_like_sized(w, h, format);
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(fmt_name, format!("{w}x{h}")),
                &bytes,
                |b, bytes| {
                    b.iter(|| {
                        image::load_from_memory_with_format(bytes, format)
                            .expect("decode fixture")
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
