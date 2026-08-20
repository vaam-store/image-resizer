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
    /// `None` lets the encoder pick its own default. Threaded through for
    /// the concurrently-landing lossy-WebP encoding work
    /// (`src/services/image/handler.rs`, owned by another agent).
    pub quality: Option<u8>,
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
}
