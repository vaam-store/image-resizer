use super::UrlParseError;
use crate::models::params::ImageFormat;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The decoded source URL plus the output format taken from the grammar's
/// trailing `.{extension}` (`/{signature}/{processing_options}/{plain|base64
/// source}.{extension}`) - the extension is mandatory per that grammar, not
/// an optional override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpec {
    pub url: String,
    pub format: ImageFormat,
}

const KNOWN_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// Parses the trailing path segment(s) that carry the source: either
/// `plain/{literal URL}.{extension}` (imgproxy's escape hatch for a
/// human-readable, percent-encoded URL - can span multiple `/`-delimited
/// segments, since the URL itself may contain slashes) or a single
/// `{base64url source}.{extension}` segment (no slashes: base64url doesn't
/// produce any).
pub fn parse_source(segments: &[&str]) -> Result<SourceSpec, UrlParseError> {
    match segments.split_first() {
        Some((&"plain", rest)) => parse_plain_source(rest),
        Some(_) => parse_base64_source(segments),
        None => Err(UrlParseError::EmptySource),
    }
}

fn parse_plain_source(rest: &[&str]) -> Result<SourceSpec, UrlParseError> {
    if rest.is_empty() {
        return Err(UrlParseError::EmptySource);
    }

    // Rejoining with '/' is lossless here: `rest` was produced by splitting
    // this same suffix of the raw request path on '/', and plain mode never
    // percent-encodes '/' out of the URL, so no information was lost.
    let joined = rest.join("/");
    let (raw_url, format) = split_extension(&joined)?;

    let url = urlencoding::decode(raw_url)
        .map(|c| c.into_owned())
        .map_err(|e| UrlParseError::InvalidSource(format!("invalid percent-encoding: {e}")))?;

    if url.is_empty() {
        return Err(UrlParseError::EmptySource);
    }

    Ok(SourceSpec { url, format })
}

fn parse_base64_source(segments: &[&str]) -> Result<SourceSpec, UrlParseError> {
    let [segment] = segments else {
        return Err(UrlParseError::InvalidSource(
            "base64-encoded source must be a single path segment (no unescaped '/')".to_string(),
        ));
    };

    let (encoded, format) = split_extension(segment)?;

    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| UrlParseError::InvalidSource(format!("invalid base64url source: {e}")))?;

    let url = String::from_utf8(decoded)
        .map_err(|e| UrlParseError::InvalidSource(format!("source is not valid UTF-8: {e}")))?;

    if url.is_empty() {
        return Err(UrlParseError::EmptySource);
    }

    Ok(SourceSpec { url, format })
}

/// Splits `s` at its final `.` into `(everything before, the extension)`,
/// requiring the extension to be one of [`KNOWN_EXTENSIONS`] - the grammar's
/// trailing `.{extension}` is mandatory, not optional.
fn split_extension(s: &str) -> Result<(&str, ImageFormat), UrlParseError> {
    let (base, ext) = s.rsplit_once('.').ok_or_else(|| {
        UrlParseError::InvalidSource(format!("missing required .<extension> suffix in {s:?}"))
    })?;

    if !KNOWN_EXTENSIONS.contains(&ext) {
        return Err(UrlParseError::InvalidSource(format!(
            "unrecognized extension {ext:?} (expected one of {KNOWN_EXTENSIONS:?})"
        )));
    }

    let format: ImageFormat = ext.parse().map_err(UrlParseError::InvalidSource)?;
    Ok((base, format))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base64_source_with_extension() {
        // base64url("https://example.com/img.jpg")
        let encoded = URL_SAFE_NO_PAD.encode("https://example.com/img.jpg");
        let segment = format!("{encoded}.png");
        let spec = parse_source(&[&segment]).unwrap();
        assert_eq!(spec.url, "https://example.com/img.jpg");
        assert_eq!(spec.format, ImageFormat::Png);
    }

    #[test]
    fn parses_plain_source_with_extension() {
        // The grammar's trailing `.{extension}` is always stripped off the
        // source, in both plain and base64 mode - the URL underneath is
        // "https://example.com/img", not "...img.png".
        let spec = parse_source(&["plain", "https:", "", "example.com", "img.png"]).unwrap();
        assert_eq!(spec.url, "https://example.com/img");
        assert_eq!(spec.format, ImageFormat::Png);
    }

    #[test]
    fn plain_source_extension_is_stripped_even_when_it_duplicates_the_urls_own() {
        // The grammar's trailing extension always wins; whatever the source
        // URL's own path looks like is irrelevant to it.
        let spec = parse_source(&["plain", "https:", "", "example.com", "img.jpg.webp"]).unwrap();
        assert_eq!(spec.url, "https://example.com/img.jpg");
        assert_eq!(spec.format, ImageFormat::Webp);
    }

    #[test]
    fn plain_source_percent_decodes() {
        let spec =
            parse_source(&["plain", "https:", "", "example.com", "a%20b.jpg"]).unwrap();
        assert_eq!(spec.url, "https://example.com/a b");
    }

    #[test]
    fn missing_extension_is_rejected() {
        let encoded = URL_SAFE_NO_PAD.encode("https://example.com/img.jpg");
        assert!(parse_source(&[&encoded]).is_err());
    }

    #[test]
    fn unrecognized_extension_is_rejected() {
        let encoded = URL_SAFE_NO_PAD.encode("https://example.com/img.jpg");
        let segment = format!("{encoded}.gif");
        assert!(parse_source(&[&segment]).is_err());
    }

    #[test]
    fn base64_source_spanning_multiple_segments_is_rejected() {
        let encoded = URL_SAFE_NO_PAD.encode("https://example.com/img.jpg");
        let segment = format!("{encoded}.png");
        assert!(parse_source(&[&segment, "extra"]).is_err());
    }

    #[test]
    fn invalid_base64_is_rejected() {
        assert!(parse_source(&["not-valid-base64!!!.png"]).is_err());
    }

    #[test]
    fn empty_source_is_rejected() {
        assert!(parse_source(&[]).is_err());
        assert!(parse_source(&["plain"]).is_err());
    }
}
