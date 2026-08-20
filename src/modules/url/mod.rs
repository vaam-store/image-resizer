//! imgproxy-compatible signed URL grammar (#53, #27):
//!
//! ```text
//! /{signature}/{processing_options}/{plain|base64 source}.{extension}
//! ```
//!
//! `{signature}` is either a base64url HMAC signature or the literal
//! `unsigned` escape hatch (`crate::modules::signing`). `{processing_options}`
//! is zero or more `/`-delimited option segments (`rs:fill:300:300`, `q:80`,
//! `bl:5`, `g:true`, `el:1` - see [`options`]), exactly mirroring how
//! imgproxy itself encodes its own advanced URL format rather than being one
//! opaque blob. The trailing segment(s) carry the source URL and, via its
//! mandatory `.{extension}` suffix, the output format (see [`source`]).
//!
//! Parsing is deliberately split into two steps so a caller can verify the
//! signature *before* paying for full grammar parsing (mirrors imgproxy's
//! own order of operations, and avoids handing an unauthenticated caller
//! detailed parse-error feedback):
//!
//! 1. [`split`] - cheap, infallible-shaped extraction of the signature
//!    segment and the exact byte string that was signed.
//! 2. [`SignedRequest::parse`] - the full options+source grammar, only
//!    called once the signature has checked out.

pub mod options;
pub mod presets;
pub mod source;

use crate::models::params::{ResizeQuery, WatermarkQuery};
use options::ProcessingOptions;
use presets::{AllowedOptions, PresetRegistry};
use source::SourceSpec;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UrlParseError {
    #[error("malformed signed URL, expected /{{signature}}/{{options}}/{{source}}: {0:?}")]
    Malformed(String),
    #[error("unknown processing option {0:?}")]
    UnknownOption(String),
    #[error("invalid value for processing option {option:?}: {reason}")]
    InvalidOptionValue { option: String, reason: String },
    #[error("source URL segment is missing or empty")]
    EmptySource,
    #[error("invalid source: {0}")]
    InvalidSource(String),
    /// #52: `code` is a real, recognised option, but this deployment's
    /// `ALLOWED_PROCESSING_OPTIONS` doesn't include it. Never raised for an
    /// option that only appears *inside* a preset's own expansion - see
    /// `presets::AllowedOptions`'s doc comment for why.
    #[error("processing option {0:?} is not allowed by this deployment's configuration")]
    OptionNotAllowed(String),
    /// #52: a `pr:{name}` segment named a preset that isn't configured.
    #[error("unknown preset {0:?}")]
    UnknownPreset(String),
}

/// The signature segment plus the exact byte string that was (or should
/// have been) signed, extracted from a raw request path without attempting
/// to understand anything past that boundary yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedRequest<'a> {
    pub signature_segment: &'a str,
    /// Everything after the signature segment, exactly as received on the
    /// wire (still percent-encoded, leading `/` included) - the byte string
    /// `crate::modules::signing::verify::verify_signature` computes the
    /// HMAC over, matching imgproxy's `salt + path` scheme.
    pub signed_path: String,
    remainder_segments: Vec<&'a str>,
}

/// Splits a raw request path (`uri.path()`, still percent-encoded - deliberately
/// *not* axum's `Path` extractor, which would percent-decode segments before
/// this code ever saw them, silently changing what gets signed) into its
/// signature segment and the rest.
pub fn split(raw_path: &str) -> Result<SignedRequest<'_>, UrlParseError> {
    let trimmed = raw_path.strip_prefix('/').unwrap_or(raw_path);

    let (signature_segment, remainder) = trimmed
        .split_once('/')
        .filter(|(_, rest)| !rest.is_empty())
        .ok_or_else(|| UrlParseError::Malformed(raw_path.to_string()))?;

    Ok(SignedRequest {
        signature_segment,
        signed_path: format!("/{remainder}"),
        remainder_segments: remainder.split('/').collect(),
    })
}

impl<'a> SignedRequest<'a> {
    /// Parses the processing-options + source grammar out of everything
    /// after the signature segment. Only call this once the signature (or
    /// the `unsigned` escape) has already been accepted.
    ///
    /// No presets, no allowlist restriction - equivalent to
    /// `parse_with_config(&PresetRegistry::empty(), &AllowedOptions::unrestricted())`.
    /// Kept as the zero-config entry point so every pre-#52 call site (and
    /// test) is unaffected.
    pub fn parse(&self) -> Result<ParsedRequest, UrlParseError> {
        self.parse_with_config(&PresetRegistry::empty(), &AllowedOptions::unrestricted())
    }

    /// [`Self::parse`], but with presets expanded and the processing-option
    /// allowlist enforced (#52).
    ///
    /// Only the *directly-present* option segments in the request are
    /// checked against `allowed` and eligible to be a `pr:{name}` preset
    /// invocation - a preset's own expansion is spliced in verbatim,
    /// neither re-checked against `allowed` (imgproxy's own documented
    /// behaviour, see `presets::AllowedOptions`) nor eligible to itself
    /// contain a `pr:` segment (rejected at config-load time,
    /// `PresetRegistry::parse`, so there's nothing to recurse into here).
    ///
    /// A configured `default` preset is prepended ahead of the request's
    /// own segments, so a request can still override any field the default
    /// sets (`ProcessingOptions::parse` processes segments in order; a
    /// later assignment to the same field wins).
    pub fn parse_with_config(
        &self,
        presets: &PresetRegistry,
        allowed: &AllowedOptions,
    ) -> Result<ParsedRequest, UrlParseError> {
        let segments = &self.remainder_segments;

        let source_index = segments
            .iter()
            .position(|seg| *seg == "plain" || !options::looks_like_option(seg))
            .unwrap_or(segments.len());

        let mut expanded: Vec<String> = Vec::new();
        if let Some(default_segments) = presets.default_preset() {
            expanded.extend(default_segments.iter().cloned());
        }

        for seg in &segments[..source_index] {
            let code = seg.split(':').next().unwrap_or_default();
            if !allowed.is_allowed(code) {
                return Err(UrlParseError::OptionNotAllowed(code.to_string()));
            }

            if code == "pr" {
                for name in seg.split(':').skip(1) {
                    let preset_segments = presets
                        .get(name)
                        .ok_or_else(|| UrlParseError::UnknownPreset(name.to_string()))?;
                    expanded.extend(preset_segments.iter().cloned());
                }
            } else {
                expanded.push((*seg).to_string());
            }
        }

        let expanded_refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
        let options = ProcessingOptions::parse(&expanded_refs)?;
        let source = source::parse_source(&segments[source_index..])?;

        Ok(ParsedRequest { options, source })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRequest {
    pub options: ProcessingOptions,
    pub source: SourceSpec,
}

impl ParsedRequest {
    pub fn into_resize_query(self) -> ResizeQuery {
        // #52: watermarking is only enabled once `wm:` supplied an opacity
        // - every other `watermark_*` field is a modifier that's inert
        // without it (see `options::ProcessingOptions`'s doc comment).
        let watermark = self.options.watermark_opacity.map(|opacity| WatermarkQuery {
            opacity,
            position: self.options.watermark_position,
            x_offset: self.options.watermark_x_offset,
            y_offset: self.options.watermark_y_offset,
            scale: self.options.watermark_scale,
            url: self.options.watermark_url,
            size: self.options.watermark_size,
            rotate: self.options.watermark_rotate,
            shadow: self.options.watermark_shadow,
        });

        ResizeQuery {
            url: self.source.url,
            width: self.options.width,
            height: self.options.height,
            resize_type: self.options.resize_type,
            format: self.source.format,
            blur_sigma: self.options.blur_sigma,
            grayscale: self.options.grayscale,
            enlarge: self.options.enlarge.unwrap_or(false),
            quality: self.options.quality,
            jpeg_quality: self.options.jpeg_quality,
            webp_quality: self.options.webp_quality,
            webp_lossless: self.options.webp_lossless,
            background: self.options.background,
            autorotate: self.options.autorotate.unwrap_or(true),
            crop: self.options.crop,
            gravity: self.options.gravity,
            // #51: geometry operations - see `ProcessingOptions`'s doc
            // comments (`src/modules/url/options.rs`) for each option's
            // grammar and default.
            rotate: self.options.rotate,
            flip_horizontal: self.options.flip_horizontal,
            flip_vertical: self.options.flip_vertical,
            trim: self.options.trim,
            extend: self.options.extend.unwrap_or(false),
            padding: self.options.padding,
            zoom_x: self.options.zoom_x,
            zoom_y: self.options.zoom_y,
            dpr: self.options.dpr,
            min_width: self.options.min_width,
            min_height: self.options.min_height,
            watermark,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::params::ImageFormat;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    fn b64(s: &str) -> String {
        URL_SAFE_NO_PAD.encode(s)
    }

    #[test]
    fn split_extracts_signature_and_signed_path() {
        let path = "/SIGNATURE/rs:fill:300:300/aHR0cHM6Ly9leGFtcGxlLmNvbQ.png";
        let signed = split(path).unwrap();
        assert_eq!(signed.signature_segment, "SIGNATURE");
        assert_eq!(
            signed.signed_path,
            "/rs:fill:300:300/aHR0cHM6Ly9leGFtcGxlLmNvbQ.png"
        );
    }

    #[test]
    fn split_rejects_missing_leading_slash_content() {
        assert!(split("").is_err());
        assert!(split("/").is_err());
        assert!(split("/onlysignature").is_err());
        assert!(split("/onlysignature/").is_err());
    }

    #[test]
    fn full_grammar_round_trips_every_capability() {
        let encoded = b64("https://example.com/img.jpg");
        // Exercises every option the grammar accepts in one path, so a new
        // option that silently fails to round-trip shows up here.
        let path = format!(
            "/SIG/rs:fill:300:300/q:80/fq:webp:90/webpo:lossless/bl:5/g:true/el:1/bg:255:0:0/ar:0/c:150:150:noea/gr:sowe/{encoded}.webp"
        );
        let signed = split(&path).unwrap();
        let parsed = signed.parse().unwrap();
        let query = parsed.into_resize_query();

        assert_eq!(query.url, "https://example.com/img.jpg");
        assert_eq!(query.width, Some(300));
        assert_eq!(query.height, Some(300));
        assert_eq!(query.resize_type, crate::models::params::ResizeType::Fill);
        assert_eq!(query.quality, Some(80));
        assert_eq!(query.webp_quality, Some(90));
        assert_eq!(query.webp_lossless, Some(true));
        assert_eq!(query.jpeg_quality, None);
        assert_eq!(query.blur_sigma, Some(5.0));
        assert_eq!(query.grayscale, Some(true));
        assert!(query.enlarge);
        assert_eq!(query.format, ImageFormat::Webp);
        assert_eq!(query.background, Some([255, 0, 0]));
        assert!(!query.autorotate, "ar:0 in the URL must disable autorotate");

        // #50: `c:`'s own gravity token (`noea`) wins over the top-level
        // `gr:` value (`sowe`) for the crop itself, but the top-level
        // `gravity` field (which also drives `Fill`'s cover-crop) reflects
        // `gr:` directly - see `crop_with_its_own_gravity_overrides_the_top_level_gravity_option`
        // in `modules::url::options::tests` for the same assertion at the
        // `ProcessingOptions` layer.
        let crop = query.crop.expect("crop should be set");
        assert_eq!(
            crop.width,
            crate::models::params::CropDimension::Absolute(150)
        );
        assert_eq!(
            crop.height,
            crate::models::params::CropDimension::Absolute(150)
        );
        assert_eq!(crop.gravity, crate::models::params::Gravity::NorthEast);
        assert_eq!(query.gravity, crate::models::params::Gravity::SouthWest);
    }

    #[test]
    fn no_processing_options_defaults_everything() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/{encoded}.jpg");
        let parsed = split(&path).unwrap().parse().unwrap();
        let query = parsed.into_resize_query();

        assert_eq!(query.width, None);
        assert_eq!(query.height, None);
        assert_eq!(
            query.resize_type,
            crate::models::params::ResizeType::default()
        );
        assert_eq!(query.quality, None);
        assert_eq!(query.jpeg_quality, None);
        assert_eq!(query.webp_quality, None);
        assert_eq!(query.webp_lossless, None);
        assert_eq!(query.blur_sigma, None);
        assert_eq!(query.grayscale, None);
        assert!(!query.enlarge);
        assert_eq!(query.background, None);
        assert!(
            query.autorotate,
            "autorotate must default to true when no `ar` segment is present"
        );
        assert_eq!(query.crop, None);
        assert_eq!(query.gravity, crate::models::params::Gravity::default());

        // #51 defaults: every geometry option is "no effect" when absent.
        assert_eq!(query.rotate, 0);
        assert!(!query.flip_horizontal);
        assert!(!query.flip_vertical);
        assert_eq!(query.trim, None);
        assert!(!query.extend);
        assert_eq!(query.padding, None);
        assert_eq!(query.zoom_x, 1.0);
        assert_eq!(query.zoom_y, 1.0);
        assert_eq!(query.dpr, 1.0);
        assert_eq!(query.min_width, None);
        assert_eq!(query.min_height, None);
    }

    /// #51: every new geometry option, parsed from a signed URL end to end
    /// through `into_resize_query` - the same "does the wiring actually
    /// reach `ResizeQuery`" check `full_grammar_round_trips_every_capability`
    /// already does for #53's original option set.
    #[test]
    fn full_grammar_round_trips_every_51_geometry_option() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!(
            "/SIG/rs:fill:300:300/rot:90/fl:1:1/t:5:ffffff:1:1/ex:1/pd:1:2:3:4/z:2/dpr:2/mw:50/mh:60/{encoded}.jpg"
        );
        let signed = split(&path).unwrap();
        let query = signed.parse().unwrap().into_resize_query();

        assert_eq!(query.rotate, 90);
        assert!(query.flip_horizontal);
        assert!(query.flip_vertical);
        let trim = query.trim.expect("trim should be set");
        assert_eq!(trim.threshold, 5.0);
        assert_eq!(trim.color, Some([255, 255, 255]));
        assert!(trim.equal_hor);
        assert!(trim.equal_ver);
        assert!(query.extend);
        assert_eq!(
            query.padding,
            Some(crate::models::params::Padding {
                top: 1,
                right: 2,
                bottom: 3,
                left: 4
            })
        );
        assert_eq!(query.zoom_x, 2.0);
        assert_eq!(query.zoom_y, 2.0);
        assert_eq!(query.dpr, 2.0);
        assert_eq!(query.min_width, Some(50));
        assert_eq!(query.min_height, Some(60));
    }

    #[test]
    fn plain_source_form_parses() {
        let path = "/SIG/q:80/plain/https://example.com/img.png".to_string();
        let parsed = split(&path).unwrap().parse().unwrap();
        // The grammar's trailing `.{extension}` is always stripped off the
        // source - see the equivalent note in `source::tests`.
        assert_eq!(parsed.source.url, "https://example.com/img");
        assert_eq!(parsed.source.format, ImageFormat::Png);
        assert_eq!(parsed.options.quality, Some(80));
    }

    #[test]
    fn unsigned_literal_is_a_valid_signature_segment_shape() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/unsigned/{encoded}.jpg");
        let signed = split(&path).unwrap();
        assert_eq!(signed.signature_segment, "unsigned");
    }

    #[test]
    fn malformed_processing_option_surfaces_as_parse_error() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/rs:fill:notanumber:300/{encoded}.jpg");
        let signed = split(&path).unwrap();
        assert!(signed.parse().is_err());
    }

    #[test]
    fn watermark_option_round_trips_into_resize_query() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/wm:0.8:soea:10:20:0.3/{encoded}.jpg");
        let query = split(&path).unwrap().parse().unwrap().into_resize_query();

        let wm = query.watermark.expect("watermark should be set");
        assert_eq!(wm.opacity, 0.8);
        assert_eq!(
            wm.position,
            crate::models::params::WatermarkPosition::SouthEast
        );
        assert_eq!(wm.x_offset, 10.0);
        assert_eq!(wm.y_offset, 20.0);
        assert_eq!(wm.scale, 0.3);
        assert_eq!(wm.url, None);
    }

    #[test]
    fn no_watermark_option_means_no_watermark() {
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/{encoded}.jpg");
        let query = split(&path).unwrap().parse().unwrap().into_resize_query();
        assert_eq!(query.watermark, None);
    }

    /// #52's core preset guarantee: `pr:{name}` must resolve to exactly the
    /// same `ResizeQuery` as writing the preset's own options out explicitly
    /// in the URL.
    #[test]
    fn preset_expands_to_the_same_resize_query_as_explicit_options() {
        let presets =
            presets::PresetRegistry::parse("thumb=rs:fill:300:300/q:80").expect("valid presets");
        let allowed = presets::AllowedOptions::unrestricted();

        let encoded = b64("https://example.com/img.jpg");
        let preset_path = format!("/SIG/pr:thumb/{encoded}.jpg");
        let explicit_path = format!("/SIG/rs:fill:300:300/q:80/{encoded}.jpg");

        let via_preset = split(&preset_path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap()
            .into_resize_query();
        let via_explicit = split(&explicit_path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap()
            .into_resize_query();

        assert_eq!(via_preset, via_explicit);
    }

    /// A request's own explicit options, placed after `pr:`, must be able
    /// to override what the preset set - segments are applied in order.
    #[test]
    fn explicit_option_after_preset_overrides_it() {
        let presets = presets::PresetRegistry::parse("thumb=q:50").expect("valid presets");
        let allowed = presets::AllowedOptions::unrestricted();
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/pr:thumb/q:90/{encoded}.jpg");

        let query = split(&path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap()
            .into_resize_query();

        assert_eq!(query.quality, Some(90));
    }

    /// A configured `default` preset applies even when the request names no
    /// preset at all, and can still be overridden by an explicit option.
    #[test]
    fn default_preset_applies_automatically() {
        let presets = presets::PresetRegistry::parse("default=el:1").expect("valid presets");
        let allowed = presets::AllowedOptions::unrestricted();
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/{encoded}.jpg");

        let query = split(&path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap()
            .into_resize_query();

        assert!(query.enlarge);
    }

    #[test]
    fn unknown_preset_name_is_a_parse_error() {
        let presets = presets::PresetRegistry::empty();
        let allowed = presets::AllowedOptions::unrestricted();
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/pr:nonexistent/{encoded}.jpg");

        let err = split(&path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap_err();
        assert!(matches!(err, UrlParseError::UnknownPreset(name) if name == "nonexistent"));
    }

    /// #52's allowlist requirement: an option excluded by
    /// `ALLOWED_PROCESSING_OPTIONS` is rejected outright when used directly.
    #[test]
    fn option_not_in_allowlist_is_rejected() {
        let presets = presets::PresetRegistry::empty();
        let allowed = presets::AllowedOptions::parse("rs,q");
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/bl:5/{encoded}.jpg");

        let err = split(&path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap_err();
        assert!(matches!(err, UrlParseError::OptionNotAllowed(code) if code == "bl"));
    }

    /// The security point of #52's allowlist design: a preset can still use
    /// an option that's excluded from direct use, since presets aren't
    /// checked against the allowlist - this is what lets an operator hand
    /// out a restricted set of presets while forbidding the raw options
    /// they're built from.
    #[test]
    fn allowlist_does_not_apply_to_options_inside_a_preset() {
        let presets = presets::PresetRegistry::parse("thumb=bl:5/q:80").expect("valid presets");
        let allowed = presets::AllowedOptions::parse("pr,q"); // "bl" not directly allowed
        let encoded = b64("https://example.com/img.jpg");
        let path = format!("/SIG/pr:thumb/{encoded}.jpg");

        let query = split(&path)
            .unwrap()
            .parse_with_config(&presets, &allowed)
            .unwrap()
            .into_resize_query();

        assert_eq!(query.blur_sigma, Some(5.0));
    }
}
