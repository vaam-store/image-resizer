//! Hand-written request models (#53: replaces the OpenAPI-codegen'd
//! `gen_server::models` this crate used to depend on).
//!
//! [`ResizeQuery`] is built directly by [`crate::modules::url`]'s signed-URL
//! grammar parser instead of being converted (via `o2o`) from a generated
//! `ResizeQueryParams` - there is no generated struct left to convert from.

/// Output image format - the imgproxy-style path grammar's trailing
/// `.{extension}` (`crate::modules::url::source`), not a query parameter.
///
/// Kept as exactly `{Jpg, Png, Webp}` (#53 explicitly keeps `Webp`): another
/// agent is concurrently adding lossy WebP encoding on top of this enum in
/// `src/services/image/handler.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Jpg,
    Png,
    Webp,
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ImageFormat::Jpg => "jpg",
            ImageFormat::Png => "png",
            ImageFormat::Webp => "webp",
        })
    }
}

impl std::str::FromStr for ImageFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "jpg" | "jpeg" => Ok(ImageFormat::Jpg),
            "png" => Ok(ImageFormat::Png),
            "webp" => Ok(ImageFormat::Webp),
            other => Err(format!(
                "unsupported image format {other:?} (expected jpg, jpeg, png or webp)"
            )),
        }
    }
}

/// imgproxy's `rs:{type}:{width}:{height}` resizing type (#59) - how the
/// source is fit into the `width`x`height` box once both dimensions are
/// present. Semantics match imgproxy's own documented behaviour
/// (<https://docs.imgproxy.net/usage/processing#resizing-type>):
///
/// - `Fit` - scale to fit *inside* the box, preserving aspect ratio; neither
///   output dimension exceeds the requested one. `DynamicImage::resize`.
/// - `Fill` - scale to *cover* the box preserving aspect ratio, then crop
///   the overflow. `DynamicImage::resize_to_fill`.
/// - `Force` - stretch to exactly `width`x`height`, ignoring aspect ratio.
///   `DynamicImage::resize_exact`.
/// - `Auto` - `Fill` when the source and requested boxes share the same
///   orientation (both landscape-or-square, or both portrait), `Fit`
///   otherwise - imgproxy's own documented rule for `auto`.
///
/// `Fit` is the default (both here and in imgproxy itself) when the `rs`
/// segment's type slot is empty (`rs::800:600`) - chosen deliberately so it
/// stays consistent with the existing single-dimension behaviour (a lone
/// width or height already "fits" by construction, never crops) rather
/// than the crop-happy `Fill` this crate silently always did before #59.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResizeType {
    #[default]
    Fit,
    Fill,
    Force,
    Auto,
}

impl std::fmt::Display for ResizeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ResizeType::Fit => "fit",
            ResizeType::Fill => "fill",
            ResizeType::Force => "force",
            ResizeType::Auto => "auto",
        })
    }
}

impl std::str::FromStr for ResizeType {
    type Err = String;

    /// An empty string (the `rs` segment's type slot left blank, e.g.
    /// `rs::800:600`) maps to the default (`Fit`), mirroring imgproxy's own
    /// "empty positional argument means use the default" convention for
    /// this option. Anything else that isn't one of the four recognised
    /// names is an error - #59 requires rejecting an unsupported type with
    /// 400 rather than silently substituting a different one.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "" | "fit" => Ok(ResizeType::Fit),
            "fill" => Ok(ResizeType::Fill),
            "force" => Ok(ResizeType::Force),
            "auto" => Ok(ResizeType::Auto),
            other => Err(format!(
                "unsupported resize type {other:?} (expected fit, fill, force or auto)"
            )),
        }
    }
}

/// The fully-parsed set of resize parameters, built by
/// [`crate::modules::url::ParsedRequest::into_resize_query`] from a signed
/// URL's processing-options segments and source. Consumed by
/// `ResizeService`/`ImageService` (`src/services/**`, owned separately) and
/// `CacheService::generate_key` unchanged from before #53 - only how this
/// struct gets constructed changed (hand-written URL grammar instead of an
/// `o2o` conversion off a generated `ResizeQueryParams`).
#[derive(Clone, PartialEq, Debug)]
pub struct ResizeQuery {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// How `width`/`height` are applied when both are present (#59) - see
    /// [`ResizeType`]. Irrelevant (and ignored by
    /// `ImageService::process_image_blocking_with_limits`) when only one of
    /// `width`/`height` is set, since a single dimension already resizes
    /// aspect-ratio-preserving with no cropping regardless of this field.
    pub resize_type: ResizeType,
    pub format: ImageFormat,
    pub blur_sigma: Option<f32>,
    pub grayscale: Option<bool>,

    /// Opt-in permission to upscale past the source image's resolution
    /// (imgproxy's `enlarge` processing option, `el:1`/`el:0` in the URL
    /// grammar). Defaults to `false` when not present in the URL, so
    /// upscaling stays refused unless a caller explicitly opts in - see
    /// `ImageService::process_image_blocking_with_limits`
    /// (`src/services/image/handler.rs`) for the guard this drives.
    pub enlarge: bool,

    /// Output encode quality (imgproxy's `q:{0-100}` processing option).
    /// `None` lets the encoder pick its own default
    /// (`ImageService::DEFAULT_JPEG_QUALITY` / `DEFAULT_WEBP_QUALITY`,
    /// `src/services/image/handler.rs`). Overridden per-format by
    /// `jpeg_quality`/`webp_quality` below when those are set (#35) -
    /// mirrors imgproxy's own `q` (global) vs `format_quality` (per-format)
    /// precedence (<https://docs.imgproxy.net/usage/processing#quality>).
    pub quality: Option<u8>,

    /// Per-format quality override (imgproxy's `format_quality`/`fq:{format}:
    /// {quality}:...` processing option, #35). Takes precedence over
    /// `quality` for JPEG output when set - see
    /// `ImageService::process_image_blocking_with_limits`
    /// (`src/services/image/handler.rs`) for the exact precedence.
    pub jpeg_quality: Option<u8>,

    /// Per-format quality override for WebP output - see `jpeg_quality`
    /// above for the general shape; same precedence over `quality`.
    pub webp_quality: Option<u8>,

    /// Encode WebP losslessly instead of lossily (imgproxy's `webp_options`/
    /// `webpo:{compression}` processing option, #35 - only the `compression`
    /// slot is implemented, see [`crate::modules::url::options`] for why).
    /// `None`/`Some(false)` keeps the existing lossy path
    /// (`ImageService::encode_webp`'s `lossless` parameter); `Some(true)`
    /// encodes losslessly and ignores `quality`/`webp_quality` entirely,
    /// matching `encode_webp`'s own "quality is used only when lossless is
    /// false" contract. Meaningless (silently ignored) for non-WebP output,
    /// same as every other format-specific option in this struct.
    pub webp_lossless: Option<bool>,

    /// Background colour, as an `[R, G, B]` triple (imgproxy's
    /// `background`/`bg:{R}:{G}:{B}` or `bg:{hex}` processing option, #34).
    /// `None` means "use the default" - `ImageService` (`src/services/image/handler.rs`)
    /// defaults to opaque white, not imgproxy's own "disabled" default,
    /// since this crate always flattens alpha before encoding to a format
    /// without an alpha channel rather than treating flattening as opt-in.
    /// Used two ways there: to flatten transparency against when encoding
    /// to a format with no alpha channel (JPEG), and as the fill colour for
    /// fully-transparent pixels when encoding to a format that keeps alpha
    /// (PNG/WebP), so invisible pixels compress instead of carrying whatever
    /// garbage RGB the source had under `alpha=0` (#60).
    pub background: Option<[u8; 3]>,

    /// Whether to rotate/flip the decoded image according to its EXIF
    /// `Orientation` tag before any resize happens (#33; imgproxy's
    /// `auto_rotate`/`ar` processing option -
    /// <https://docs.imgproxy.net/usage/processing#auto-rotate>).
    ///
    /// Defaults to `true` - unlike every other boolean option on this
    /// struct (`enlarge`, `grayscale`), matching imgproxy's own documented
    /// default (`IMGPROXY_AUTO_ROTATE: true`): a phone photo's pixels are
    /// stored sideways/upside-down as a matter of course, so leaving them
    /// that way unless a caller opts in would produce the wrong output for
    /// the common case, not just an unusual one.
    ///
    /// `ImageService::process_image_blocking_with_limits`
    /// (`src/services/image/handler.rs`) applies this via
    /// `DynamicImage::apply_orientation` immediately after decode and
    /// before any resize/crop math - order matters, since a `Rotate90`/
    /// `Rotate270` orientation swaps width and height, and applying it
    /// after a crop would compose the crop against the wrong axes.
    pub autorotate: bool,
}

/// Path parameter for the (unsigned, key-validated) download route
/// `GET /api/images/files/{key}`. Replaces the generated
/// `gen_server::models::DownloadPathParams` one-for-one - same single
/// `key: String` field, nothing else changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadPathParams {
    pub key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_format_round_trips_through_display_and_from_str() {
        for (fmt, text) in [
            (ImageFormat::Jpg, "jpg"),
            (ImageFormat::Png, "png"),
            (ImageFormat::Webp, "webp"),
        ] {
            assert_eq!(fmt.to_string(), text);
            assert_eq!(text.parse::<ImageFormat>().unwrap(), fmt);
        }
    }

    #[test]
    fn image_format_accepts_jpeg_alias() {
        assert_eq!("jpeg".parse::<ImageFormat>().unwrap(), ImageFormat::Jpg);
    }

    #[test]
    fn image_format_rejects_unknown_values() {
        assert!("gif".parse::<ImageFormat>().is_err());
        assert!("".parse::<ImageFormat>().is_err());
    }

    #[test]
    fn image_format_default_is_jpg() {
        assert_eq!(ImageFormat::default(), ImageFormat::Jpg);
    }

    #[test]
    fn resize_type_round_trips_through_display_and_from_str() {
        for (kind, text) in [
            (ResizeType::Fit, "fit"),
            (ResizeType::Fill, "fill"),
            (ResizeType::Force, "force"),
            (ResizeType::Auto, "auto"),
        ] {
            assert_eq!(kind.to_string(), text);
            assert_eq!(text.parse::<ResizeType>().unwrap(), kind);
        }
    }

    #[test]
    fn resize_type_empty_string_means_default() {
        assert_eq!("".parse::<ResizeType>().unwrap(), ResizeType::Fit);
    }

    #[test]
    fn resize_type_rejects_unknown_values() {
        assert!("crop".parse::<ResizeType>().is_err());
        assert!("Fill".parse::<ResizeType>().is_err());
    }

    #[test]
    fn resize_type_default_is_fit() {
        assert_eq!(ResizeType::default(), ResizeType::Fit);
    }
}
