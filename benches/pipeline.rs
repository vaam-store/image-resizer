//! Full-pipeline benchmark: `ImageService::process_image_blocking`
//! (src/services/image/handler.rs), called directly and unmodified (it was
//! only made `pub` - a visibility-only change - so this bench could reach
//! it as an external crate). This is decode + resize + optional filters +
//! encode, exactly as production runs it, for a few representative fixture
//! / parameter combinations.
//!
//! `photo_real/*` cases mirror their `photo_like/*`/`photo_4k/*` synthetic
//! counterparts but decode a real NASA photograph instead of
//! `gradient_noise_rgb` - see `benches/decode.rs`'s module doc comment for
//! why the synthetic-only corpus this file used to have gives a distorted
//! picture of decode cost, and `benches/fixtures/real/ATTRIBUTION.md` for
//! the fixtures' provenance/licence. `photo_real_large` uses the primary
//! real source at its native 2200x1100 (the largest size available without
//! upscaling a committed fixture - see that file's own doc comment) rather
//! than a true 3840x2160 like `photo_4k`; `photo_real_earthrise` uses the
//! second, structurally different real source (near-black space + lunar
//! regolith texture) so this comparison isn't resting on one photo.

#[path = "fixtures.rs"]
mod fixtures;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use emgr::models::params::ImageFormat as ApiImageFormat;
use emgr::models::params::{ResizeQuery, ResizeType};
use emgr::services::image::handler::ImageService;
use image::ImageFormat;

fn query(width: Option<u32>, height: Option<u32>, format: ApiImageFormat) -> ResizeQuery {
    ResizeQuery {
        url: "https://images.example.com/photo.jpg".to_string(),
        width,
        height,
        resize_type: ResizeType::Fit,
        format,
        ..Default::default()
    }
}

/// Same as `query`, but with `strip_metadata` set explicitly rather than
/// left at its `Default` (`true`) - used by the #88 `photo_like_with_exif`
/// cases below to cover both the default "strip" path and `sm:0` ("keep")
/// against the same EXIF-carrying source.
fn query_with_metadata(
    width: Option<u32>,
    height: Option<u32>,
    format: ApiImageFormat,
    strip_metadata: bool,
) -> ResizeQuery {
    ResizeQuery {
        strip_metadata,
        ..query(width, height, format)
    }
}

fn bench_pipeline(c: &mut Criterion) {
    let photo = fixtures::photo_like();
    let flat = fixtures::flat();
    let alpha = fixtures::alpha();
    // #63 stage 2: a large (4K) source downscaled to a small thumbnail -
    // the exact shape of request the mozjpeg DCT-scaled decode targets, and
    // the one the pre-stage-2 fixture set (topping out at 1920x1080, the
    // `photo_like/thumbnail_jpg` case above) didn't cover. Matches the
    // prototype measurement quoted in the #63 issue thread: 4K -> 200x113,
    // decode + resize, 58.03ms (full decode) vs 26.21ms (1/8-scale decode).
    let photo_4k = fixtures::photo_like_sized(3840, 2160, ImageFormat::Jpeg);
    // #88: the only fixture in the corpus carrying a realistic (~45KB)
    // EXIF blob - see `fixtures::photo_like_with_exif`'s own doc comment
    // for why every other case above is structurally blind to
    // metadata-handling cost.
    let photo_with_exif = fixtures::photo_like_with_exif();
    // Real-photo counterparts (see this file's own module doc comment and
    // `benches/fixtures/real/ATTRIBUTION.md`).
    let photo_real = fixtures::real_photo_sized(1920, 1080, ImageFormat::Jpeg);
    let photo_real_large = fixtures::real_photo_sized(2200, 1100, ImageFormat::Jpeg);
    let photo_real_earthrise = fixtures::real_photo_secondary_sized(1280, 1280, ImageFormat::Jpeg);

    let cases: [(&str, &[u8], ResizeQuery); 9] = [
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
        (
            "photo_4k/large_downscale_thumbnail_jpg",
            &photo_4k,
            query(Some(200), Some(113), ApiImageFormat::Jpg),
        ),
        // #88: decode + resize + encode of the same EXIF-carrying source,
        // strip (default `sm`) vs keep (`sm:0`) - the pair this issue asks
        // to make measurable. Both resize to the same thumbnail box as
        // `photo_like/thumbnail_jpg` above so the *only* deliberate
        // variable between this pair is `strip_metadata`.
        (
            "photo_with_exif/strip_metadata_default",
            &photo_with_exif,
            query_with_metadata(Some(300), Some(300), ApiImageFormat::Jpg, true),
        ),
        (
            "photo_with_exif/keep_metadata_sm0",
            &photo_with_exif,
            query_with_metadata(Some(300), Some(300), ApiImageFormat::Jpg, false),
        ),
        (
            "photo_real/thumbnail_jpg",
            &photo_real,
            query(Some(300), Some(300), ApiImageFormat::Jpg),
        ),
        (
            "photo_real_large/large_downscale_thumbnail_jpg",
            &photo_real_large,
            query(Some(200), Some(113), ApiImageFormat::Jpg),
        ),
        (
            "photo_real_earthrise/thumbnail_jpg",
            &photo_real_earthrise,
            query(Some(300), Some(300), ApiImageFormat::Jpg),
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
