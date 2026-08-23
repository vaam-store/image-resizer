//! AVIF encode/decode via `libavif` (raw `libavif-sys` FFI), #67/#68.
//!
//! # Why raw `libavif-sys` instead of the high-level `libavif` crate
//!
//! crates.io has both a raw-bindings crate (`libavif-sys`) and a thin
//! high-level wrapper on top of it (`libavif`, same repository/maintainer,
//! `njaard/libavif-rs`). The high-level crate was evaluated first and
//! rejected: its `Encoder` has no EXIF-write API at all (`AvifImage` has no
//! `set_exif`/equivalent - only `avifImageSetMetadataExif` in the raw `sys`
//! bindings), and its `RgbPixels`/`AvifImage` types don't expose the
//! decoder's own `icc`/`exif` fields either. Since this crate needs both
//! (matching every other codec path in `handler.rs`, which forwards EXIF
//! through `encode_jpeg`/`PngEncoder::set_exif_metadata`/the old
//! `AvifEncoder::set_exif_metadata`), the raw bindings are used directly -
//! the same "hand-roll a thin wrapper when the higher-level crate doesn't
//! cover what's needed" choice this codebase already made for JPEG
//! (`mozjpeg`, not a JPEG-specific higher-level crate).
//!
//! # Why `libavif-sys` over other AVIF crates evaluated
//!
//! `avif-rs` (crates.io, `vegidio/avif-rs`, "Encode and decode AVIF images
//! with SVT-AV1 and dav1d, via statically-linked libavif") looked
//! attractive on paper - SVT-AV1 support is exactly what this project's
//! own survey wanted evaluated. It was rejected after reading its
//! `build.rs`: at build time it downloads a prebuilt static-library
//! archive from `https://github.com/vegidio/binaries-avif/releases`, a
//! *different* GitHub account's own binary distribution channel, not built
//! from source during `cargo build` and not distributed via crates.io. For
//! a Docker build of an attacker-facing image-processing service, linking
//! an opaque prebuilt `.a` from a third-party binary release channel -
//! unauditable at build time, no corresponding source visible in the
//! dependency tree `cargo deny`/`cargo audit` actually scan - is a real
//! supply-chain regression versus every other native dependency this crate
//! has (`mozjpeg-sys`/`libwebp-sys`/this module's own `libavif-sys` all
//! compile their C source from the crate's own vendored tree via `cc`/
//! `cmake`/`meson`). Not used.
//!
//! `libavif-sys` (`njaard/libavif-rs`) instead vendors libavif's actual C
//! source (`libavif-sys-*/libavif/`, a full copy of the upstream tree) and
//! builds it via `cmake` in its own `build.rs`, statically
//! (`BUILD_SHARED_LIBS=0`, `cargo:rustc-link-lib=static=avif`) - real
//! source-to-binary provenance, matching this project's other `-sys`
//! dependencies.
//!
//! # Codec backend: AOM only, not AOM+SVT-AV1
//!
//! `libavif-sys`'s own `Cargo.toml` exposes exactly three codec features:
//! `codec-aom` (AV1 encode+decode via `libaom-sys`, itself vendoring and
//! building AOM 3.11.0 from source via `cmake`), `codec-dav1d` (AV1 decode
//! only, via `libdav1d-sys`, vendoring and building dav1d from source via
//! `meson`/`ninja`), and `codec-rav1e` (the same pure-Rust `rav1e` this
//! change removes - see `Cargo.toml`'s own comment on the `image`
//! dependency). There is no `codec-svt`/SVT-AV1 feature: libavif's C build
//! system (`CMakeLists.txt`) does support an `AVIF_CODEC_SVT` option
//! upstream, but `libavif-sys`'s `build.rs` never sets it and has no
//! `libsvtav1-sys`-equivalent dependency to source SVT-AV1 from - wiring
//! it in would mean forking/patching this crate's build script, not just
//! adding a Cargo feature, which is out of scope here. Every other
//! crates.io crate found in this search either wraps the same
//! `libavif-sys` (so inherits the same limitation) or is the
//! prebuilt-binary-download `avif-rs` rejected above (whose binaries *do*
//! include `SvtAv1Enc`, per its own `build.rs`, but at the supply-chain
//! cost documented above). **AOM is therefore the only AV1 encode backend
//! wired in** (`codec-aom` + `codec-dav1d`, `codec-rav1e` left off -
//! `libavif-sys = { default-features = false, features = ["codec-aom",
//! "codec-dav1d"] }` in `Cargo.toml`). `avifEncoderCreate`'s default
//! `codecChoice` is `AVIF_CODEC_CHOICE_AUTO`, which resolves to the one
//! encode-capable codec actually compiled in (AOM) - no explicit codec
//! selection is needed in this module for that reason, and the same is
//! true for decode (`codec-dav1d` is the only decode-capable codec
//! compiled in, so `AUTO` resolves to dav1d there too).
//!
//! One dependency covers both AVIF directions - decode (`codec-dav1d`,
//! #67, the capability actually requested) and encode (`codec-aom`, #68,
//! replacing `ravif`/`rav1e`) - rather than needing two.

use anyhow::{Context, Result};
use image::metadata::Orientation;
use image::DynamicImage;
use libavif_sys as sys;

/// AVIF-specific slice of the decode tuple every other decode path in
/// `handler.rs` returns as `DecodedImage` (`(DynamicImage, Orientation,
/// Option<Vec<u8>>, Option<Vec<u8>>)`), kept as an inline tuple here rather
/// than importing that private type alias across the module boundary.
pub type AvifDecoded = (DynamicImage, Orientation, Option<Vec<u8>>, Option<Vec<u8>>);

/// Converts a non-`AVIF_RESULT_OK` `avifResult` into an `Err` carrying
/// libavif's own human-readable message (`avifResultToString`) - every FFI
/// call in this module that returns `avifResult` is checked through this,
/// the same "translate the C-level error into a normal `Result`" role
/// `.context(..)` plays for the `image`-crate/mozjpeg paths elsewhere in
/// this file.
fn ensure_avif_ok(result: sys::avifResult, what: &str) -> Result<()> {
    if result == sys::AVIF_RESULT_OK {
        return Ok(());
    }
    let msg = unsafe {
        let ptr = sys::avifResultToString(result);
        if ptr.is_null() {
            "unknown".to_string()
        } else {
            std::ffi::CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    };
    anyhow::bail!("libavif: {what} failed: {msg} (code {result})")
}

/// RAII guard around `*mut avifDecoder`. `avifDecoderDestroy` also frees
/// `decoder->image`/internal buffers - nothing else in this module holds a
/// pointer derived from the decoder past this guard's own scope, since
/// every function below copies pixel/exif/icc data into owned `Vec<u8>`s
/// before returning.
struct AvifDecoderGuard(*mut sys::avifDecoder);

impl AvifDecoderGuard {
    fn create() -> Result<Self> {
        let ptr = unsafe { sys::avifDecoderCreate() };
        anyhow::ensure!(!ptr.is_null(), "libavif: avifDecoderCreate returned null");
        Ok(Self(ptr))
    }
}

impl Drop for AvifDecoderGuard {
    fn drop(&mut self) {
        unsafe { sys::avifDecoderDestroy(self.0) }
    }
}

/// RAII guard around `*mut avifImage`.
struct AvifImageGuard(*mut sys::avifImage);

impl AvifImageGuard {
    fn create(width: u32, height: u32, depth: u32, yuv_format: sys::avifPixelFormat) -> Result<Self> {
        let ptr = unsafe { sys::avifImageCreate(width, height, depth, yuv_format) };
        anyhow::ensure!(!ptr.is_null(), "libavif: avifImageCreate returned null");
        Ok(Self(ptr))
    }
}

impl Drop for AvifImageGuard {
    fn drop(&mut self) {
        unsafe { sys::avifImageDestroy(self.0) }
    }
}

/// RAII guard around `*mut avifEncoder`.
struct AvifEncoderGuard(*mut sys::avifEncoder);

impl AvifEncoderGuard {
    fn create() -> Result<Self> {
        let ptr = unsafe { sys::avifEncoderCreate() };
        anyhow::ensure!(!ptr.is_null(), "libavif: avifEncoderCreate returned null");
        Ok(Self(ptr))
    }
}

impl Drop for AvifEncoderGuard {
    fn drop(&mut self) {
        unsafe { sys::avifEncoderDestroy(self.0) }
    }
}

/// RAII guard around an owned `avifRGBImage` whose `pixels` buffer was
/// allocated by libavif itself (`avifRGBImageAllocatePixels`) - as opposed
/// to `encode_avif_inner`'s use of `avifRGBImage`, which points `pixels` at
/// a Rust-owned buffer it never asks libavif to allocate or free.
struct AvifRgbGuard(sys::avifRGBImage);

impl Drop for AvifRgbGuard {
    fn drop(&mut self) {
        unsafe { sys::avifRGBImageFreePixels(&mut self.0) }
    }
}

/// Detects an AVIF source by its ISOBMFF `ftyp` box via libavif's own
/// `avifPeekCompatibleFileType` - kept here (rather than duplicating the
/// box-walking logic `handler.rs::is_avif` does independently as a pure
/// magic-byte check with no FFI) only as the ground truth this module's
/// own tests check that hand-rolled check against; `handler.rs`'s
/// `detect_format_from_bytes` uses its own `is_avif` so that function
/// doesn't need to link against `libavif-sys` just to sniff a format tag.
#[cfg(test)]
pub(crate) fn is_avif(bytes: &[u8]) -> bool {
    let raw = sys::avifROData {
        data: bytes.as_ptr(),
        size: bytes.len(),
    };
    unsafe { sys::avifPeekCompatibleFileType(&raw) == sys::AVIF_TRUE as sys::avifBool }
}

/// Reads only the AVIF container header (`avifDecoderParse`) to get the
/// image's dimensions, without decoding any AV1-coded pixel payload -
/// AVIF's equivalent of `ImageReader::into_dimensions()`, which can't
/// parse AVIF at all without the `avif-native` feature this crate doesn't
/// enable (see this module's own doc comment). Called from
/// `ImageService::peek_dimensions` *before* `check_source_resolution` and
/// any pixel decode, exactly like every other format's header peek.
pub fn peek_dimensions(image_bytes: &[u8]) -> Result<(u32, u32)> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        peek_dimensions_inner(image_bytes)
    }))
    .unwrap_or_else(|payload| Err(panic_to_error(payload, "libavif header parse")))
}

fn peek_dimensions_inner(image_bytes: &[u8]) -> Result<(u32, u32)> {
    let decoder = AvifDecoderGuard::create()?;
    unsafe {
        let res = sys::avifDecoderSetIOMemory(decoder.0, image_bytes.as_ptr(), image_bytes.len());
        ensure_avif_ok(res, "avifDecoderSetIOMemory")?;

        let res = sys::avifDecoderParse(decoder.0);
        ensure_avif_ok(res, "avifDecoderParse")?;

        let image = (*decoder.0).image;
        anyhow::ensure!(!image.is_null(), "libavif: parsed decoder has no image");
        Ok(((*image).width, (*image).height))
    }
}

/// Decodes an AVIF source, enforcing the same resolution guards every
/// other format's decode path does (#67):
/// - `imageDimensionLimit`/`imageSizeLimit` are set on the `avifDecoder`
///   itself *before* `avifDecoderParse`, so libavif rejects an
///   oversized-by-header source at parse time - libavif's own equivalent
///   of `build_decode_limits`'s defense-in-depth role for the `image`
///   crate paths.
/// - `check_source_resolution` is re-run against the parsed header
///   dimensions immediately after `avifDecoderParse` and *before*
///   `avifDecoderNextImage` (the call that actually decodes the AV1
///   payload) - this is what makes an AVIF decompression bomb fail the
///   same way a JPEG/PNG/WebP one does: rejected from a cheap header read,
///   never reaching the expensive pixel decode. The *primary* guard
///   (`ImageService::peek_dimensions` -> `check_source_resolution`, run by
///   the caller before `decode_with_limits`/this function is ever reached)
///   already covers this - the check here is defense in depth, matching
///   every other decode path's "primary check upstream, guard repeated at
///   the point of actual decode" structure.
///
/// # Panics (caught, not propagated)
///
/// The whole FFI sequence (`avifDecoderParse` through `avifImageYUVToRGB`)
/// runs inside one `catch_unwind`, same defensive spirit as
/// `mozjpeg_decode`/`libwebp_decode` in `handler.rs`: nothing in
/// `libavif-sys`'s raw bindings is expected to panic on malformed input,
/// but `Cargo.toml`'s `panic = "unwind"` (kept for #29, decoding untrusted
/// input) is what makes any unexpected panic here catchable rather than
/// fatal to the whole worker thread, and this function is reached with
/// attacker-supplied bytes on every AVIF-source request.
pub fn decode(image_bytes: &[u8], max_src_resolution_mp: u64) -> Result<AvifDecoded> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_inner(image_bytes, max_src_resolution_mp)
    }))
    .unwrap_or_else(|payload| Err(panic_to_error(payload, "libavif decode")))
}

fn decode_inner(image_bytes: &[u8], max_src_resolution_mp: u64) -> Result<AvifDecoded> {
    let decoder = AvifDecoderGuard::create()?;
    unsafe {
        (*decoder.0).imageDimensionLimit = 65_535;
        let max_pixels = max_src_resolution_mp
            .saturating_mul(1_000_000)
            .min(u64::from(u32::MAX));
        (*decoder.0).imageSizeLimit = max_pixels as u32;
        // Still images only - every other decode path in this crate treats
        // AVIF as non-animatable (only Gif/Webp are, see
        // `decode_animation_source`'s doc comment), so a limit of 1 keeps a
        // multi-image AVIF sequence from being parsed any further than its
        // first frame, mirroring `collect_frames_capped`'s bomb-guard spirit
        // for the one format that actually is treated as animatable.
        (*decoder.0).imageCountLimit = 1;

        let res = sys::avifDecoderSetIOMemory(decoder.0, image_bytes.as_ptr(), image_bytes.len());
        ensure_avif_ok(res, "avifDecoderSetIOMemory")?;

        let res = sys::avifDecoderParse(decoder.0);
        ensure_avif_ok(res, "avifDecoderParse")?;

        let image_ptr = (*decoder.0).image;
        anyhow::ensure!(!image_ptr.is_null(), "libavif: parsed decoder has no image");
        let (width, height) = ((*image_ptr).width, (*image_ptr).height);

        // Defense in depth - see this function's own doc comment; the
        // primary check already ran in `ImageService::peek_dimensions`
        // against these same header-parsed dimensions.
        crate::services::image::handler::ImageService::check_source_resolution(
            width,
            height,
            max_src_resolution_mp,
        )?;

        let res = sys::avifDecoderNextImage(decoder.0);
        ensure_avif_ok(res, "avifDecoderNextImage")?;

        // Re-read: `avifDecoderNextImage` can reallocate `decoder->image`.
        let image_ptr = (*decoder.0).image;
        anyhow::ensure!(!image_ptr.is_null(), "libavif: decoded decoder has no image");

        let orientation = avif_orientation(
            (*image_ptr).transformFlags,
            (*image_ptr).irot.angle,
            (*image_ptr).imir.axis,
        );

        let icc_profile = avif_rw_data_to_vec(&(*image_ptr).icc);
        let exif_metadata = avif_rw_data_to_vec(&(*image_ptr).exif);

        let mut rgb_guard = AvifRgbGuard(std::mem::zeroed());
        sys::avifRGBImageSetDefaults(&mut rgb_guard.0, image_ptr);
        rgb_guard.0.format = sys::AVIF_RGB_FORMAT_RGBA;
        rgb_guard.0.depth = 8;

        let res = sys::avifRGBImageAllocatePixels(&mut rgb_guard.0);
        ensure_avif_ok(res, "avifRGBImageAllocatePixels")?;

        let res = sys::avifImageYUVToRGB(image_ptr, &mut rgb_guard.0);
        ensure_avif_ok(res, "avifImageYUVToRGB")?;

        let expected_row_bytes = (width as usize)
            .checked_mul(4)
            .context("libavif: row byte count overflow")?;
        let row_bytes = rgb_guard.0.rowBytes as usize;
        anyhow::ensure!(
            row_bytes >= expected_row_bytes,
            "libavif: decoded row stride ({row_bytes}) shorter than expected ({expected_row_bytes})"
        );

        let mut pixels = vec![
            0u8;
            expected_row_bytes
                .checked_mul(height as usize)
                .context("libavif: decoded buffer size overflow")?
        ];
        for y in 0..height as usize {
            let src = std::slice::from_raw_parts(rgb_guard.0.pixels.add(y * row_bytes), expected_row_bytes);
            pixels[y * expected_row_bytes..(y + 1) * expected_row_bytes].copy_from_slice(src);
        }

        let buf = image::RgbaImage::from_raw(width, height, pixels)
            .context("libavif: decoded RGBA buffer size mismatch")?;

        Ok((DynamicImage::ImageRgba8(buf), orientation, icc_profile, exif_metadata))
    }
}

/// Copies an `avifRWData` (used for both `avifImage.icc` and
/// `avifImage.exif`) into an owned `Vec<u8>`, or `None` if empty - mirrors
/// the `.ok().flatten()` "nothing to forward" convention every other
/// decode path in `handler.rs` uses for `icc_profile`/`exif_metadata`.
///
/// # Safety
/// `data` must point to at least `size` initialized bytes, or be null/zero
/// (in which case nothing is read) - true for any `avifRWData` still owned
/// by a live `avifImage`, which is the only way this is ever called.
unsafe fn avif_rw_data_to_vec(data: &sys::avifRWData) -> Option<Vec<u8>> {
    if data.size == 0 || data.data.is_null() {
        None
    } else {
        Some(unsafe { std::slice::from_raw_parts(data.data, data.size) }.to_vec())
    }
}

/// Maps AVIF's `irot`/`imir` transform boxes to this crate's EXIF-shaped
/// `Orientation` enum, so a decoded AVIF's transform flows through the
/// exact same `params.autorotate`-gated `img.apply_orientation(orientation)`
/// call in `handler.rs` that every EXIF-oriented JPEG/PNG source already
/// uses, instead of a second, parallel "apply the transform" code path.
///
/// The (`irot.angle`, `imir.axis`) -> EXIF-orientation-number table below
/// is not derived independently - it's the *exact inverse* of libavif's
/// own `avifImageExtractExifOrientationToIrotImir`
/// (`libavif-sys-*/libavif/src/exif.c`), which converts an EXIF
/// orientation tag (1-8) to `irot`/`imir` for encoding. Reading that
/// function directly (rather than re-deriving the HEIF/MIAF transform-order
/// spec by hand) is what pins down the one detail otherwise easy to get
/// backwards: when both `irot` and `imir` are present (EXIF 5 and 7 only),
/// libavif's own comment states `irot` is "applied before imir according
/// to MIAF spec ISO/IEC 28002-12:2021 - section 7.3.6.7" - i.e. rotate
/// first, then mirror, for encoding; this function only needs to
/// recognise which EXIF orientation a given (angle, axis, flags) triple
/// came from, not re-apply the transform order itself, since
/// `DynamicImage::apply_orientation` already implements the *decode*-side
/// (correcting) transform for whichever `Orientation` variant is returned.
///
/// A `(transform_flags, angle, axis)` combination outside all eight
/// canonical cases (possible only from a hand-crafted or non-libavif-
/// authored AVIF, never from anything libavif itself writes) falls back to
/// `Orientation::NoTransforms`, the same "malformed metadata isn't worth
/// failing the whole request over" default `decoder.orientation()
///     .unwrap_or(Orientation::NoTransforms)` already uses for every other
/// format in `handler.rs`.
fn avif_orientation(transform_flags: sys::avifTransformFlags, angle: u8, axis: u8) -> Orientation {
    const IROT: sys::avifTransformFlags = 1 << 2; // AVIF_TRANSFORM_IROT
    const IMIR: sys::avifTransformFlags = 1 << 3; // AVIF_TRANSFORM_IMIR

    let effective_angle = if transform_flags & IROT != 0 { angle } else { 0 };
    let effective_mirror = if transform_flags & IMIR != 0 {
        Some(axis)
    } else {
        None
    };

    let exif_orientation = match (effective_angle, effective_mirror) {
        (0, None) => 1,
        (0, Some(1)) => 2,
        (2, None) => 3,
        (0, Some(0)) => 4,
        (1, Some(0)) => 5,
        (3, None) => 6,
        (3, Some(0)) => 7,
        (1, None) => 8,
        // Not producible by libavif's own encoder-side conversion - see
        // this function's own doc comment.
        _ => 1,
    };

    Orientation::from_exif(exif_orientation).unwrap_or(Orientation::NoTransforms)
}

fn panic_to_error(payload: Box<dyn std::any::Any + Send>, what: &str) -> anyhow::Error {
    let msg = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
        .unwrap_or_else(|| format!("{what} panicked with a non-string payload"));
    anyhow::anyhow!("{what} panicked: {msg}")
}

/// Encodes `img` to AVIF via `libavif`+AOM (#68), replacing the pure-Rust
/// `ravif`/`rav1e` encoder `image::codecs::avif::AvifEncoder` used before
/// this change - see this module's own doc comment for why AOM is the only
/// backend wired in, and `handler.rs`'s `ImageFormat::Avif` match arm for
/// how this replaces the old `AvifEncoder::new_with_speed_quality` call
/// directly, same `quality`/`DEFAULT_AVIF_SPEED` inputs as before.
///
/// `quality`/`speed` map directly onto `avifEncoder`'s own `quality`
/// (0-100, 100 = lossless) and `speed` (0-10, 10 = fastest) fields - the
/// same numeric ranges `DEFAULT_AVIF_QUALITY`/`DEFAULT_AVIF_SPEED` already
/// used against `ravif`'s equivalent knobs (`AvifEncoder::
/// new_with_speed_quality`'s own `speed`/`quality` parameters used
/// identical 0-10/0-100 ranges), so neither constant needed to change.
/// Alpha quality is set equal to `quality` - this crate's request surface
/// has no separate alpha-quality knob, same as before this change.
///
/// `exif_metadata` (#5) is written via `avifImageSetMetadataExif` -
/// libavif's real EXIF-write API, matching what the old
/// `AvifEncoder::set_exif_metadata` provided. ICC is intentionally not
/// threaded through here, preserving this crate's pre-existing AVIF
/// behaviour (see `encode_single_image`'s own ICC comment in
/// `handler.rs`) - `avifImageSetProfileICC` exists and could carry it in a
/// future change, but that's new scope this change doesn't take on.
///
/// `pub` (matching `encode_webp`/`encode_jpeg` in `handler.rs`) so
/// `benches/encode.rs` can benchmark the exact path production uses.
pub fn encode(img: &DynamicImage, quality: u8, speed: u8, exif_metadata: Option<&[u8]>) -> Result<Vec<u8>> {
    let has_alpha = img.color().has_alpha();
    let rgba = img.to_rgba8();
    let exif_owned = exif_metadata.map(<[u8]>::to_vec);

    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        encode_inner(&rgba, has_alpha, quality, speed, exif_owned.as_deref())
    }))
    .unwrap_or_else(|payload| Err(panic_to_error(payload, "libavif encode")))
}

fn encode_inner(
    rgba: &image::RgbaImage,
    has_alpha: bool,
    quality: u8,
    speed: u8,
    exif_metadata: Option<&[u8]>,
) -> Result<Vec<u8>> {
    let (width, height) = rgba.dimensions();
    anyhow::ensure!(
        width > 0 && height > 0,
        "libavif: cannot encode a zero-sized image"
    );

    // 4:2:0 - libavif/`avifenc`'s own default chroma subsampling for
    // lossy photographic content, and what most AVIF encoders (including
    // the reference `avifenc` CLI this crate's `DEFAULT_AVIF_QUALITY` doc
    // comment already cites) use unless told otherwise. This is a real,
    // reportable output-bytes change versus the old `ravif`/`rav1e` path
    // (a different encoder, different chroma handling, different AV1
    // encoder implementation entirely) - see this change's own report.
    let image = AvifImageGuard::create(width, height, 8, sys::AVIF_PIXEL_FORMAT_YUV420)?;

    unsafe {
        let mut rgb: sys::avifRGBImage = std::mem::zeroed();
        sys::avifRGBImageSetDefaults(&mut rgb, image.0);
        rgb.format = sys::AVIF_RGB_FORMAT_RGBA;
        rgb.depth = 8;
        rgb.ignoreAlpha = if has_alpha { sys::AVIF_FALSE } else { sys::AVIF_TRUE } as sys::avifBool;
        // Borrows `rgba`'s own buffer for the duration of `avifImageRGBToYUV`
        // below - not allocated or freed by libavif, unlike `decode_inner`'s
        // `AvifRgbGuard`, so no guard/free call is needed for `rgb` here.
        rgb.pixels = rgba.as_raw().as_ptr().cast_mut();
        rgb.rowBytes = width
            .checked_mul(4)
            .context("libavif: row byte count overflow")?;

        let res = sys::avifImageRGBToYUV(image.0, &rgb);
        ensure_avif_ok(res, "avifImageRGBToYUV")?;

        if let Some(exif) = exif_metadata.filter(|e| !e.is_empty()) {
            // Best-effort, same spirit as the ICC/EXIF `let _ =` branches
            // elsewhere in `handler.rs`: a metadata-write failure (e.g. a
            // malformed EXIF payload libavif's own parser rejects) isn't
            // worth failing the whole encode over.
            let _ = sys::avifImageSetMetadataExif(image.0, exif.as_ptr(), exif.len());
        }

        let encoder = AvifEncoderGuard::create()?;
        (*encoder.0).quality = i32::from(quality.min(100));
        (*encoder.0).qualityAlpha = i32::from(quality.min(100));
        (*encoder.0).speed = i32::from(speed.min(10));

        let mut out: sys::avifRWData = std::mem::zeroed();
        let res = sys::avifEncoderWrite(encoder.0, image.0, &mut out);
        if res != sys::AVIF_RESULT_OK {
            sys::avifRWDataFree(&mut out);
            return ensure_avif_ok(res, "avifEncoderWrite").map(|()| unreachable!());
        }

        let bytes = std::slice::from_raw_parts(out.data, out.size).to_vec();
        sys::avifRWDataFree(&mut out);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    /// `handler.rs::ImageService::is_avif` is a hand-rolled, FFI-free
    /// `ftyp`-box magic-byte check (kept dependency-free so
    /// `detect_format_from_bytes` doesn't need to link `libavif-sys` just
    /// to sniff a format tag - see that function's own doc comment). This
    /// pins it against libavif's *own* `avifPeekCompatibleFileType` (this
    /// module's `is_avif`) on a real encoded AVIF, so the two can't
    /// silently drift apart.
    #[test]
    fn handler_is_avif_agrees_with_libavif_peek_compatible_file_type() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4));
        let avif_bytes = encode(&img, 50, 8, None).expect("AVIF encode should succeed");

        assert!(is_avif(&avif_bytes));
        assert!(crate::services::image::handler::ImageService::is_avif(
            &avif_bytes
        ));

        // Neither should misdetect an unrelated format.
        let not_avif = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x00garbage!!!!";
        assert!(!is_avif(not_avif));
        assert!(!crate::services::image::handler::ImageService::is_avif(
            not_avif
        ));
    }

    #[test]
    fn encode_then_decode_round_trips_dimensions() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            32,
            24,
            image::Rgb([120, 60, 200]),
        ));
        let avif_bytes = encode(&img, 70, 8, None).expect("AVIF encode should succeed");

        let (decoded, orientation, _icc, _exif) =
            decode(&avif_bytes, 50).expect("AVIF decode should succeed");

        assert_eq!(decoded.dimensions(), (32, 24));
        assert_eq!(orientation, Orientation::NoTransforms);
    }

    #[test]
    fn encode_writes_exif_metadata_readable_on_decode() {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(4, 4));
        // Minimal well-formed EXIF TIFF header (no IFD entries) - enough
        // for `avifImageSetMetadataExif`'s own parser to accept without
        // needing a real orientation tag.
        let exif: &[u8] = b"II*\0\x08\0\0\0\0\0\0\0";
        let avif_bytes = encode(&img, 70, 8, Some(exif)).expect("AVIF encode should succeed");

        let (_decoded, _orientation, _icc, exif_out) =
            decode(&avif_bytes, 50).expect("AVIF decode should succeed");

        assert_eq!(exif_out.as_deref(), Some(exif));
    }

    #[test]
    fn avif_orientation_maps_every_exif_orientation_libavif_can_produce() {
        const IROT: sys::avifTransformFlags = 1 << 2;
        const IMIR: sys::avifTransformFlags = 1 << 3;

        // (transform_flags, angle, axis) -> expected EXIF orientation,
        // taken directly from libavif's own
        // avifImageExtractExifOrientationToIrotImir table (see this
        // module's `avif_orientation` doc comment).
        let cases: &[(sys::avifTransformFlags, u8, u8, u8)] = &[
            (0, 0, 0, 1),
            (IMIR, 0, 1, 2),
            (IROT, 2, 0, 3),
            (IMIR, 0, 0, 4),
            (IROT | IMIR, 1, 0, 5),
            (IROT, 3, 0, 6),
            (IROT | IMIR, 3, 0, 7),
            (IROT, 1, 0, 8),
        ];

        for &(flags, angle, axis, expected_exif) in cases {
            let expected = Orientation::from_exif(expected_exif).unwrap();
            assert_eq!(
                avif_orientation(flags, angle, axis),
                expected,
                "flags={flags} angle={angle} axis={axis}"
            );
        }
    }
}
