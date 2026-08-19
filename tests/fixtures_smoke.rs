//! Smoke tests for the deterministic fixture corpus shared with the
//! criterion benches (`benches/fixtures.rs`) and the `benchmark` load-test
//! bin. Confirms the generator produces valid, decodable images with the
//! expected properties, and that generation is byte-identical across runs.

#[path = "../benches/fixtures.rs"]
mod fixtures;

use std::io::Cursor;

#[test]
fn photo_like_decodes_and_is_deterministic() {
    let a = fixtures::photo_like();
    let b = fixtures::photo_like();
    assert_eq!(a, b, "fixture generation must be deterministic");

    let img = image::load_from_memory_with_format(&a, image::ImageFormat::Jpeg)
        .expect("photo_like fixture should decode as JPEG");
    assert_eq!(
        (img.width(), img.height()),
        (fixtures::PHOTO_LIKE_W, fixtures::PHOTO_LIKE_H)
    );
}

#[test]
fn flat_is_tiny_on_disk() {
    let bytes = fixtures::flat();
    // 1920x1080 solid colour should compress to a few KB via DEFLATE.
    assert!(
        bytes.len() < 50_000,
        "flat fixture unexpectedly large: {} bytes",
        bytes.len()
    );

    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .expect("flat fixture should decode as PNG");
    assert_eq!(
        (img.width(), img.height()),
        (fixtures::PHOTO_LIKE_W, fixtures::PHOTO_LIKE_H)
    );
}

#[test]
fn alpha_has_transparent_border_with_garbage_rgb() {
    let bytes = fixtures::alpha();
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .expect("alpha fixture should decode as PNG")
        .to_rgba8();

    let border_px = img.get_pixel(0, 0);
    assert_eq!(border_px[3], 0, "border pixel should be fully transparent");
    assert!(
        border_px[0] != 0 || border_px[1] != 0 || border_px[2] != 0,
        "border RGB should be garbage, not zeroed out alongside alpha"
    );

    let center_px = img.get_pixel(fixtures::ALPHA_SIZE / 2, fixtures::ALPHA_SIZE / 2);
    assert_eq!(center_px[3], 255, "center pixel should be fully opaque");
}

#[test]
fn tiny_is_actually_tiny() {
    let bytes = fixtures::tiny();
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
        .expect("tiny fixture should decode as JPEG");
    assert_eq!(
        (img.width(), img.height()),
        (fixtures::TINY_SIZE, fixtures::TINY_SIZE)
    );
}

#[test]
fn bomb_is_small_on_disk_but_decodes_huge() {
    let bytes = fixtures::bomb();
    // DEFLATE's 258-byte max match length puts a floor under how far a
    // constant-colour image can shrink (~300KB in practice here) - but that
    // is still a >900x compression ratio against the 300MB raw RGB buffer
    // (10000x10000x3), which is the property this fixture exists to exercise.
    assert!(
        bytes.len() < 1_000_000,
        "bomb fixture should be small on disk: {} bytes",
        bytes.len()
    );

    // Read dimensions from the header only - decoding a 10000x10000 image
    // fully in a test would defeat the point of it being a "bomb".
    let (w, h) = image::ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .expect("guess bomb fixture format")
        .into_dimensions()
        .expect("read bomb fixture dimensions");
    assert_eq!((w, h), (fixtures::BOMB_SIDE, fixtures::BOMB_SIDE));
}

#[test]
fn by_name_matches_direct_accessors() {
    assert_eq!(
        fixtures::by_name("photo_like"),
        Some(fixtures::photo_like())
    );
    assert_eq!(fixtures::by_name("flat"), Some(fixtures::flat()));
    assert_eq!(fixtures::by_name("alpha"), Some(fixtures::alpha()));
    assert_eq!(fixtures::by_name("tiny"), Some(fixtures::tiny()));
    assert_eq!(fixtures::by_name("bomb"), Some(fixtures::bomb()));
    assert_eq!(fixtures::by_name("nonexistent"), None);
}
