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
pub mod source;

use crate::models::params::ResizeQuery;
use options::ProcessingOptions;
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
    pub fn parse(&self) -> Result<ParsedRequest, UrlParseError> {
        let segments = &self.remainder_segments;

        let source_index = segments
            .iter()
            .position(|seg| *seg == "plain" || !options::looks_like_option(seg))
            .unwrap_or(segments.len());

        let options = ProcessingOptions::parse(&segments[..source_index])?;
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
            "/SIG/rs:fill:300:300/q:80/fq:webp:90/webpo:lossless/bl:5/g:true/el:1/bg:255:0:0/ar:0/{encoded}.webp"
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
}
