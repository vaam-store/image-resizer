// TEMPORARY, throwaway harness for manual DSSIM verification of #63 stage 2
// (mozjpeg DCT-scaled decode). Not part of the permanent test suite - dumps
// pipeline output for a real photograph to a path named by the DSSIM_OUT env
// var, for offline comparison with the `dssim` CLI. Delete before merging.
use emgr::models::params::{ImageFormat as ApiImageFormat, ResizeQuery, ResizeType};
use emgr::services::image::handler::ImageService;

#[test]
#[ignore]
fn dump_dssim_comparison_png() {
    let bytes = std::fs::read("bench-imgproxy/fixtures/corpus/photo_4k.jpg")
        .expect("real photo corpus should be present at bench-imgproxy/fixtures/corpus/photo_4k.jpg");

    let width: u32 = std::env::var("DSSIM_WIDTH")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    let height: u32 = std::env::var("DSSIM_HEIGHT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(113);

    let params = ResizeQuery {
        url: "https://example.com/photo.jpg".to_string(),
        width: Some(width),
        height: Some(height),
        resize_type: ResizeType::Fit,
        format: ApiImageFormat::Png,
        ..Default::default()
    };

    let (bytes_out, _) = ImageService::process_image_blocking(&bytes, &params).expect("process");

    let out_path = std::env::var("DSSIM_OUT").unwrap_or_else(|_| "/tmp/dssim_out.png".to_string());
    std::fs::write(&out_path, bytes_out).expect("write output");
    eprintln!("wrote {out_path}");
}
