//! Cache-key throughput benchmark: `CacheService::generate_key`
//! (src/services/cache/handler.rs), called directly and unmodified.

use criterion::{Criterion, criterion_group, criterion_main};
use emgr::models::params::ResizeQuery;
use emgr::services::cache::handler::CacheServiceBuilder;
use gen_server::models::ImageFormat as ApiImageFormat;

fn bench_cache_key(c: &mut Criterion) {
    let cache = CacheServiceBuilder::default()
        .minio_sub_path("bench/".to_string())
        .build()
        .expect("build CacheService");

    let params = ResizeQuery {
        url: "https://images.example.com/some/deep/path/photo.jpg?query=1&other=2".to_string(),
        width: Some(800),
        height: Some(600),
        format: ApiImageFormat::Webp,
        blur_sigma: Some(1.5),
        grayscale: Some(false),
    };

    c.bench_function("cache_key/generate_key", |b| {
        b.iter(|| cache.generate_key(&params));
    });
}

criterion_group!(benches, bench_cache_key);
criterion_main!(benches);
