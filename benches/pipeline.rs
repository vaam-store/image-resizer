//! Full-pipeline benchmark: `ImageService::process_image_blocking`
//! (src/services/image/handler.rs), called directly and unmodified (it was
//! only made `pub` - a visibility-only change - so this bench could reach
//! it as an external crate). This is decode + resize + optional filters +
//! encode, exactly as production runs it, for a few representative fixture
//! / parameter combinations.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::models::params::ImageFormat as ApiImageFormat;
use emgr::models::params::{ResizeQuery, ResizeType};
use emgr::services::image::handler::ImageService;

fn query(width: Option<u32>, height: Option<u32>, format: ApiImageFormat) -> ResizeQuery {
    ResizeQuery {
        url: "https://images.example.com/photo.jpg".to_string(),
        width,
        height,
        resize_type: ResizeType::Fit,
        format,
        blur_sigma: None,
        grayscale: None,
        enlarge: false,
        quality: None,
        jpeg_quality: None,
        webp_quality: None,
        webp_lossless: None,
        background: None,
        autorotate: true,
    }
}

fn bench_pipeline(c: &mut Criterion) {
    let photo = fixtures::photo_like();
    let flat = fixtures::flat();
    let alpha = fixtures::alpha();

    let cases: [(&str, &[u8], ResizeQuery); 3] = [
        (
            "photo_like/thumbnail_jpg",
            &photo,
            query(Some(300), Some(300), ApiImageFormat::Jpg),
        ),
        (
            "flat/resize_png",
            &flat,
            query(Some(800), None, ApiImageFormat::Png),
        ),
        (
            "alpha/resize_webp",
            &alpha,
            query(Some(256), Some(256), ApiImageFormat::Webp),
        ),
    ];

    let mut group = c.benchmark_group("pipeline/process_image_blocking");

    for (name, bytes, params) in cases {
        group.bench_with_input(BenchmarkId::from_parameter(name), &params, |b, params| {
            b.iter(|| ImageService::process_image_blocking(bytes, params).expect("process"));
        });
    }

    group.finish();
}

criterion_group!(benches, bench_pipeline);
criterion_main!(benches);
