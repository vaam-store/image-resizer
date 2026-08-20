//! Resize-stage benchmarks: every `FilterType`, both downscale and upscale.
//!
//! The `resize` group calls `DynamicImage::resize`, the *old* (pre-#63
//! stage-1) resampling path - each filter selected explicitly, instead of
//! going through the service's own size-based filter heuristic, so every
//! filter's individual cost is visible rather than just whichever one the
//! heuristic would have picked. Kept unchanged (not repointed at
//! `fast_image_resize`) specifically so it stays a stable "what would the
//! `image` crate alone cost here" reference.
//!
//! The `resize_fir` group is the #63 stage-1 apples-to-apples counterpart:
//! same source, same targets, same filter *names*, but resampled via
//! `fast_image_resize` using the exact same filter mapping
//! `ImageService::fir_resize_alg` uses in production
//! (src/services/image/handler.rs) - Triangle -> Bilinear (same kernel,
//! different name in each crate), everything else name-for-name. This is
//! what the service's real code path (`Self::fir_resize` /
//! `fir_resize_exact`) now does under the hood, isolated from decode/
//! encode/alpha-flatten so the resampling-kernel win is directly visible
//! instead of only inferable from the full `pipeline/*` benchmark deltas.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use fast_image_resize as fir;
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

/// Same mapping as `ImageService::fir_resize_alg`
/// (src/services/image/handler.rs) - duplicated here rather than shared
/// because that function is a private associated function of a service
/// type in the `emgr` binary crate, not reachable from a `[[bench]]`
/// target.
fn fir_resize_alg(filter: FilterType) -> fir::ResizeAlg {
    match filter {
        FilterType::Nearest => fir::ResizeAlg::Nearest,
        FilterType::Triangle => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        FilterType::CatmullRom => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        FilterType::Gaussian => fir::ResizeAlg::Convolution(fir::FilterType::Gaussian),
        FilterType::Lanczos3 => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
    }
}

/// Same resample-only helper as `ImageService::fir_resize_exact`, minus
/// the zero-dimension floor and same-size short-circuit (never hit by
/// this benchmark's fixed targets), so this measures exactly the
/// `fast_image_resize::Resizer::resize` call the production code makes.
fn fir_resize_exact(img: &DynamicImage, nwidth: u32, nheight: u32, filter: FilterType) -> DynamicImage {
    let mut dst = DynamicImage::new(nwidth, nheight, img.color());
    let mut resizer = fir::Resizer::new();
    let options = fir::ResizeOptions::new().resize_alg(fir_resize_alg(filter));
    resizer
        .resize(img, &mut dst, &options)
        .expect("fast_image_resize resize");
    dst
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

fn bench_resize_fir(c: &mut Criterion) {
    let img = source_image();
    let mut group = c.benchmark_group("resize_fir");
    group.sample_size(20);

    for (name, filter) in FILTERS {
        group.bench_with_input(
            BenchmarkId::new("downscale", name),
            &filter,
            |b, &filter| {
                b.iter(|| fir_resize_exact(&img, DOWNSCALE_TARGET.0, DOWNSCALE_TARGET.1, filter));
            },
        );

        group.bench_with_input(BenchmarkId::new("upscale", name), &filter, |b, &filter| {
            b.iter(|| fir_resize_exact(&img, UPSCALE_TARGET.0, UPSCALE_TARGET.1, filter));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_resize, bench_resize_fir);
criterion_main!(benches);
