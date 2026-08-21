//! Decode-stage benchmarks: JPEG / PNG / WebP at a few resolutions.
//!
//! PNG and WebP call `image::load_from_memory_with_format`, the exact call
//! `ImageService::decode_with_image_crate` (src/services/image/handler.rs)
//! makes for those formats - i.e. this measures the real decode cost the
//! service pays, not a synthetic stand-in.
//!
//! JPEG instead calls `ImageService::mozjpeg_decode(&bytes, 8)` (#67):
//! every JPEG decode, DCT-scaled or not, now goes through mozjpeg rather
//! than `image`-crate/zune-jpeg - see that function's own doc comment and
//! `decode_jpeg_scaled`'s retired-rationale comment for why. `scale_num =
//! 8` is "no DCT reduction," matching this bench's full-size decode (no
//! resize is requested here), the same call `decode_jpeg_scaled` makes when
//! `select_jpeg_dct_scale` picks no scaling.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use emgr::services::image::handler::ImageService;
use image::ImageFormat;

const SIZES: [(u32, u32); 3] = [(640, 360), (1280, 720), (1920, 1080)];
const NON_JPEG_FORMATS: [(&str, ImageFormat); 2] =
    [("png", ImageFormat::Png), ("webp", ImageFormat::WebP)];

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode");

    for (w, h) in SIZES {
        let bytes = fixtures::photo_like_sized(w, h, ImageFormat::Jpeg);
        group.throughput(Throughput::Bytes(bytes.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("jpeg", format!("{w}x{h}")),
            &bytes,
            |b, bytes| {
                b.iter(|| ImageService::mozjpeg_decode(bytes, 8).expect("decode fixture"));
            },
        );
    }

    for (fmt_name, format) in NON_JPEG_FORMATS {
        for (w, h) in SIZES {
            let bytes = fixtures::photo_like_sized(w, h, format);
            group.throughput(Throughput::Bytes(bytes.len() as u64));
            group.bench_with_input(
                BenchmarkId::new(fmt_name, format!("{w}x{h}")),
                &bytes,
                |b, bytes| {
                    b.iter(|| {
                        image::load_from_memory_with_format(bytes, format).expect("decode fixture")
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);
