//! Resize-stage benchmarks: every `FilterType`, both downscale and upscale.
//!
//! Calls `DynamicImage::resize`, the same call
//! `ImageService::process_image_blocking` (src/services/image/handler.rs)
//! makes - here with each filter selected explicitly, instead of going
//! through the service's own size-based filter heuristic, so every filter's
//! individual cost is visible rather than just whichever one the heuristic
//! would have picked.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use image::DynamicImage;
use image::imageops::FilterType;

const FILTERS: [(&str, FilterType); 5] = [
    ("nearest", FilterType::Nearest),
    ("triangle", FilterType::Triangle),
    ("catmull_rom", FilterType::CatmullRom),
    ("gaussian", FilterType::Gaussian),
    ("lanczos3", FilterType::Lanczos3),
];

// Source is the 1920x1080 photo-like fixture.
const DOWNSCALE_TARGET: (u32, u32) = (320, 180);
const UPSCALE_TARGET: (u32, u32) = (3840, 2160);

fn source_image() -> DynamicImage {
    let bytes = fixtures::photo_like();
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .expect("decode photo_like fixture")
}

fn bench_resize(c: &mut Criterion) {
    let img = source_image();
    let mut group = c.benchmark_group("resize");
    // Upscale iterations are expensive (Lanczos3 to 4K); keep sample size
    // reasonable so `cargo bench` finishes in a sane amount of time.
    group.sample_size(20);

    for (name, filter) in FILTERS {
        group.bench_with_input(
            BenchmarkId::new("downscale", name),
            &filter,
            |b, &filter| {
                b.iter(|| img.resize(DOWNSCALE_TARGET.0, DOWNSCALE_TARGET.1, filter));
            },
        );

        group.bench_with_input(BenchmarkId::new("upscale", name), &filter, |b, &filter| {
            b.iter(|| img.resize(UPSCALE_TARGET.0, UPSCALE_TARGET.1, filter));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_resize);
criterion_main!(benches);
