use super::UrlParseError;
use crate::models::params::ResizeType;

/// The set of processing options this service understands, parsed from
/// their `/`-delimited `code:arg1:arg2` path segments. Mirrors imgproxy's
/// own short option codes (`rs`, `q`, `bl`, `g`, `el`) so a client library
/// written for imgproxy's URL format produces something this parser accepts
/// for the capability set #53 keeps: width, height, resize type, blur,
/// grayscale, enlarge, quality (format comes from the trailing
/// `.{extension}` instead - see [`super::source`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProcessingOptions {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// `rs`'s `{type}` slot (#59) - see [`ResizeType`]. Defaults to
    /// `ResizeType::default()` (`Fit`) when no `rs` segment is present at
    /// all, which is harmless since `width`/`height` are then both `None`
    /// too and no resize happens regardless of type.
    pub resize_type: ResizeType,
    pub blur_sigma: Option<f32>,
    pub grayscale: Option<bool>,
    pub enlarge: Option<bool>,
    pub quality: Option<u8>,
}

/// A processing-option path segment always contains a `:` (`rs:fill:300:300`,
/// `q:80`, ...). A base64url-encoded source segment can never contain one -
/// `:` isn't in the base64url alphabet - so this single check is enough to
/// tell "there's another option coming" from "we've reached the source",
/// exactly the boundary imgproxy's own URL format draws.
pub fn looks_like_option(segment: &str) -> bool {
    segment.contains(':')
}

impl ProcessingOptions {
    pub fn parse(segments: &[&str]) -> Result<Self, UrlParseError> {
        let mut opts = Self::default();

        for segment in segments {
            let mut parts = segment.split(':');
            let code = parts.next().unwrap_or_default();
            let args: Vec<&str> = parts.collect();

            match code {
                // rs:{type}:{width}:{height} - `type` (`fit`/`fill`/`force`/
                // `auto`, imgproxy's resizing type) is carried through to
                // `ResizeQuery::resize_type` and drives which of
                // `resize`/`resize_to_fill`/`resize_exact` the resize
                // pipeline (`ImageService::process_image_blocking_with_limits`,
                // src/services/image/handler.rs) uses once both width and
                // height are present (#59). An unrecognised type is
                // rejected with `UrlParseError::InvalidOptionValue` (400)
                // rather than silently falling back to a different one.
                // `0` for width/height means "not set", mirroring
                // imgproxy's own `rs`/`resize` convention.
                "rs" => {
                    let [kind, width, height] = require_args::<3>(&args, segment)?;
                    opts.resize_type = parse_resize_type(kind, segment)?;
                    opts.width = parse_dimension(width, segment)?;
                    opts.height = parse_dimension(height, segment)?;
                }
                "q" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.quality = Some(parse_bounded(value, segment, 0, 100)?);
                }
                "bl" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.blur_sigma = Some(parse_float(value, segment)?);
                }
                "g" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.grayscale = Some(parse_bool(value, segment)?);
                }
                "el" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.enlarge = Some(parse_bool(value, segment)?);
                }
                other => return Err(UrlParseError::UnknownOption(other.to_string())),
            }
        }

        Ok(opts)
    }
}

fn require_args<'a, const N: usize>(
    args: &[&'a str],
    segment: &str,
) -> Result<[&'a str; N], UrlParseError> {
    <[&str; N]>::try_from(args).map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("expected exactly {N} argument(s)"),
    })
}

/// `0` means "not set" (imgproxy's own convention for `rs`/`resize`'s
/// width/height slots), anything else must parse as a positive `u32`.
fn parse_dimension(raw: &str, segment: &str) -> Result<Option<u32>, UrlParseError> {
    let value: u32 = raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid unsigned integer"),
    })?;
    Ok(if value == 0 { None } else { Some(value) })
}

fn parse_bounded(raw: &str, segment: &str, min: u8, max: u8) -> Result<u8, UrlParseError> {
    let value: u8 = raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid integer in [{min}, {max}]"),
    })?;
    if value < min || value > max {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{value} is out of range [{min}, {max}]"),
        });
    }
    Ok(value)
}

fn parse_float(raw: &str, segment: &str) -> Result<f32, UrlParseError> {
    raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid number"),
    })
}

/// Parses `rs`'s `{type}` slot into a [`ResizeType`] (#59). Delegates to
/// `ResizeType::from_str` (which already treats an empty string as "use the
/// default") and re-wraps its `Err(String)` as the same
/// `UrlParseError::InvalidOptionValue` every other malformed `rs` argument
/// produces, so an unsupported type is rejected with 400 exactly like an
/// unparseable width/height rather than silently substituting a different
/// resize behaviour.
fn parse_resize_type(raw: &str, segment: &str) -> Result<ResizeType, UrlParseError> {
    raw.parse()
        .map_err(|reason| UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason,
        })
}

fn parse_bool(raw: &str, segment: &str) -> Result<bool, UrlParseError> {
    match raw {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        other => Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{other:?} is not a valid boolean (expected true/false/1/0)"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resize_option() {
        let opts = ProcessingOptions::parse(&["rs:fill:300:300"]).unwrap();
        assert_eq!(opts.width, Some(300));
        assert_eq!(opts.height, Some(300));
        assert_eq!(opts.resize_type, ResizeType::Fill);
    }

    #[test]
    fn parses_every_resize_type() {
        for (token, expected) in [
            ("fit", ResizeType::Fit),
            ("fill", ResizeType::Fill),
            ("force", ResizeType::Force),
            ("auto", ResizeType::Auto),
        ] {
            let segment = format!("rs:{token}:300:300");
            let opts = ProcessingOptions::parse(&[&segment]).unwrap();
            assert_eq!(opts.resize_type, expected, "type token {token:?}");
        }
    }

    #[test]
    fn empty_resize_type_slot_defaults_to_fit() {
        let opts = ProcessingOptions::parse(&["rs::300:300"]).unwrap();
        assert_eq!(opts.resize_type, ResizeType::Fit);
    }

    #[test]
    fn unknown_resize_type_is_rejected_with_400_shaped_error() {
        let err = ProcessingOptions::parse(&["rs:crop:300:300"]).unwrap_err();
        assert!(matches!(err, UrlParseError::InvalidOptionValue { .. }));
    }

    #[test]
    fn resize_zero_means_unset() {
        let opts = ProcessingOptions::parse(&["rs:fill:0:300"]).unwrap();
        assert_eq!(opts.width, None);
        assert_eq!(opts.height, Some(300));
    }

    #[test]
    fn parses_quality() {
        let opts = ProcessingOptions::parse(&["q:80"]).unwrap();
        assert_eq!(opts.quality, Some(80));
    }

    #[test]
    fn quality_out_of_range_is_rejected() {
        assert!(ProcessingOptions::parse(&["q:101"]).is_err());
    }

    #[test]
    fn parses_blur() {
        let opts = ProcessingOptions::parse(&["bl:5"]).unwrap();
        assert_eq!(opts.blur_sigma, Some(5.0));
    }

    #[test]
    fn parses_grayscale_true_and_false() {
        assert_eq!(
            ProcessingOptions::parse(&["g:true"]).unwrap().grayscale,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["g:false"]).unwrap().grayscale,
            Some(false)
        );
        assert_eq!(
            ProcessingOptions::parse(&["g:1"]).unwrap().grayscale,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["g:0"]).unwrap().grayscale,
            Some(false)
        );
    }

    #[test]
    fn parses_enlarge() {
        assert_eq!(
            ProcessingOptions::parse(&["el:1"]).unwrap().enlarge,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["el:0"]).unwrap().enlarge,
            Some(false)
        );
    }

    #[test]
    fn combines_multiple_options() {
        let opts =
            ProcessingOptions::parse(&["rs:fill:300:300", "q:80", "bl:5", "g:true", "el:1"])
                .unwrap();
        assert_eq!(opts.width, Some(300));
        assert_eq!(opts.height, Some(300));
        assert_eq!(opts.resize_type, ResizeType::Fill);
        assert_eq!(opts.quality, Some(80));
        assert_eq!(opts.blur_sigma, Some(5.0));
        assert_eq!(opts.grayscale, Some(true));
        assert_eq!(opts.enlarge, Some(true));
    }

    #[test]
    fn unknown_option_code_is_rejected() {
        assert!(ProcessingOptions::parse(&["zz:1"]).is_err());
    }

    #[test]
    fn wrong_argument_count_is_rejected() {
        assert!(ProcessingOptions::parse(&["rs:fill:300"]).is_err());
        assert!(ProcessingOptions::parse(&["q:80:extra"]).is_err());
    }

    #[test]
    fn empty_options_is_valid() {
        assert_eq!(ProcessingOptions::parse(&[]).unwrap(), ProcessingOptions::default());
    }

    #[test]
    fn looks_like_option_distinguishes_options_from_base64() {
        assert!(looks_like_option("q:80"));
        assert!(looks_like_option("rs:fill:300:300"));
        // base64url never contains ':'.
        assert!(!looks_like_option("aHR0cHM6Ly9leGFtcGxlLmNvbQ"));
        assert!(!looks_like_option("plain"));
    }
}
