use super::UrlParseError;
use crate::models::params::{
    Crop, CropDimension, Gravity, Padding, ResizeType, TrimOptions, WatermarkPosition,
};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The set of processing options this service understands, parsed from
/// their `/`-delimited `code:arg1:arg2` path segments. Mirrors imgproxy's
/// own short option codes (`rs`, `q`, `bl`, `g`, `el`) so a client library
/// written for imgproxy's URL format produces something this parser accepts
/// for the capability set #53 keeps: width, height, resize type, blur,
/// grayscale, enlarge, quality, per-format quality override, webp lossless,
/// autorotate, explicit crop + gravity (#50) (format comes from the
/// trailing `.{extension}` instead - see [`super::source`]), plus #51's
/// geometry operations (rotate, flip, trim, extend, padding, zoom, dpr,
/// min-width/min-height).
#[derive(Debug, Clone, PartialEq)]
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
    /// `fq`'s parsed `jpg`/`jpeg` slot (#35) - see
    /// [`crate::models::params::ResizeQuery::jpeg_quality`].
    pub jpeg_quality: Option<u8>,
    /// `fq`'s parsed `webp` slot (#35) - see
    /// [`crate::models::params::ResizeQuery::webp_quality`].
    pub webp_quality: Option<u8>,
    /// `webpo`'s parsed `compression` slot (#35) - see
    /// [`crate::models::params::ResizeQuery::webp_lossless`].
    pub webp_lossless: Option<bool>,
    /// `bg`'s parsed `[R, G, B]` triple (#34) - see
    /// [`crate::models::params::ResizeQuery::background`] for how it's
    /// consumed.
    pub background: Option<[u8; 3]>,
    /// `ar`'s parsed boolean (#33) - see
    /// [`crate::models::params::ResizeQuery::autorotate`]. `None` means
    /// "use the default", which is `true` (autorotate stays on) unlike
    /// every other `Option<bool>` field here - see `ResizeQuery::autorotate`'s
    /// own doc comment for why.
    pub autorotate: Option<bool>,

    /// `c`'s parsed crop region (#50) - see [`Crop`]. A `c:` segment whose
    /// own gravity token is omitted resolves its `Crop::gravity` from
    /// `Self::gravity` (below) once the whole segment list has been parsed
    /// (`ProcessingOptions::parse`'s post-loop resolution step), so a `gr:`
    /// segment appearing *after* `c:` in the URL still applies to it -
    /// there's no positional ordering requirement between the two.
    pub crop: Option<Crop>,

    /// `gr`'s parsed gravity (#50) - see [`Gravity`]. Always present (not
    /// `Option<Gravity>`), defaulting to `Gravity::default()` (`Center`,
    /// imgproxy's own default) when no `gr:` segment is present, mirroring
    /// `resize_type` above.
    pub gravity: Gravity,

    // --- #51 additions start: kept as one contiguous block, mirroring the
    // matching block in `src/services/cache/handler.rs`'s `generate_key`,
    // so the two stay easy to cross-reference.
    /// `rotate`/`rot`'s `{angle}` slot - see
    /// [`crate::models::params::ResizeQuery::rotate`]. Always normalised to
    /// `0`/`90`/`180`/`270` here (imgproxy only requires a multiple of 90,
    /// including negative angles - `parse_rotate_angle` below does the
    /// `rem_euclid(360)` normalisation).
    pub rotate: i32,
    /// `flip`/`fl`'s `{horizontal}:{vertical}` slots - see
    /// [`crate::models::params::ResizeQuery::flip_horizontal`].
    pub flip_horizontal: bool,
    pub flip_vertical: bool,
    /// `trim`/`t`'s parsed arguments - see
    /// [`crate::models::params::ResizeQuery::trim`].
    pub trim: Option<TrimOptions>,
    /// `extend`/`ex`'s `{enabled}` slot - see
    /// [`crate::models::params::ResizeQuery::extend`]. `None` (not present
    /// in the URL) and `Some(false)` (present but `0`/`false`) both mean
    /// "disabled" - collapsed to a plain `bool` by
    /// `ParsedRequest::into_resize_query`, same pattern as `enlarge`.
    pub extend: Option<bool>,
    /// `padding`/`pd`'s CSS-shorthand arguments - see
    /// [`crate::models::params::Padding`].
    pub padding: Option<Padding>,
    /// `zoom`/`z`'s `{zoom_x}:{zoom_y}` slots - see
    /// [`crate::models::params::ResizeQuery::zoom_x`]. Default `1.0`
    /// (neutral multiplier), not `0.0` - set directly in [`Self::default`]
    /// rather than via `#[derive(Default)]`, which would give every field
    /// its type's zero value.
    pub zoom_x: f32,
    pub zoom_y: f32,
    /// `dpr`'s `{value}` slot - see
    /// [`crate::models::params::ResizeQuery::dpr`]. Default `1.0`, same
    /// reasoning as `zoom_x`/`zoom_y`.
    pub dpr: f32,
    /// `min-width`/`mw` and `min-height`/`mh`'s `{width}`/`{height}` slots -
    /// see [`crate::models::params::ResizeQuery::min_width`]. `0` means
    /// "not set", same convention as `width`/`height` themselves.
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    // --- #51 additions end.

    // #52 watermarking. These mirror imgproxy's `wm`/`wmu`/`wms`/`wmr`/
    // `wmsh` short option codes. `watermark_opacity` (from `wm`'s required
    // first slot) is what actually *enables* watermarking -
    // `into_resize_query` only builds a `WatermarkQuery` when it's
    // `Some(..)`; every other `watermark_*` field here is a modifier that's
    // inert unless `wm:` is also present, matching imgproxy's own grammar
    // (`wmu`/`wms`/`wmr`/`wmsh` are meaningless without `wm`).
    pub watermark_opacity: Option<f32>,
    pub watermark_position: WatermarkPosition,
    pub watermark_x_offset: f32,
    pub watermark_y_offset: f32,
    pub watermark_scale: f32,
    /// `wmu`'s decoded URL (imgproxy Pro's arbitrary-URL watermark) - still
    /// unvalidated at this layer; SSRF validation happens where it's
    /// fetched (`ImageService::process_image`, #21/#57), same as the main
    /// source URL.
    pub watermark_url: Option<String>,
    pub watermark_size: Option<(u32, u32)>,
    pub watermark_rotate: f32,
    pub watermark_shadow: Option<f32>,
}

impl Default for ProcessingOptions {
    /// Hand-written (rather than `#[derive(Default)]`) solely because
    /// `zoom_x`/`zoom_y`/`dpr` need to default to `1.0` (the neutral
    /// multiplier), not `f32`'s zero value - every other field keeps the
    /// same "unset" default `#[derive(Default)]` would have produced.
    fn default() -> Self {
        Self {
            width: None,
            height: None,
            resize_type: ResizeType::default(),
            blur_sigma: None,
            grayscale: None,
            enlarge: None,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background: None,
            autorotate: None,
            crop: None,
            gravity: Gravity::default(),
            rotate: 0,
            flip_horizontal: false,
            flip_vertical: false,
            trim: None,
            extend: None,
            padding: None,
            zoom_x: 1.0,
            zoom_y: 1.0,
            dpr: 1.0,
            min_width: None,
            min_height: None,
            watermark_opacity: None,
            watermark_position: WatermarkPosition::default(),
            watermark_x_offset: 0.0,
            watermark_y_offset: 0.0,
            watermark_scale: 0.0,
            watermark_url: None,
            watermark_size: None,
            watermark_rotate: 0.0,
            watermark_shadow: None,
        }
    }
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

        // Raw crop dimensions plus an *optional* explicit gravity, captured
        // during the loop below but not resolved into `opts.crop` until
        // every segment has been seen - see `crop`'s doc comment above for
        // why this two-step resolution exists (order-independence between
        // `c:` and `gr:`).
        let mut crop_raw: Option<(CropDimension, CropDimension, Option<Gravity>)> = None;

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
                // fq:{format1}:{quality1}:{format2}:{quality2}:... - imgproxy's
                // `format_quality`/`fq` option (#35,
                // <https://docs.imgproxy.net/usage/processing#format-quality>):
                // "adds or redefines format_quality values" for the current
                // request, on top of `q`'s global default. Only `jpg`/`jpeg`
                // and `webp` are accepted - PNG output always uses this
                // crate's fixed `CompressionType::Best` (no continuous 0-100
                // quality knob exists to override), so `fq:png:N` is rejected
                // with 400 rather than silently ignored. Repeating a format
                // (`fq:jpg:80:jpg:90`) lets the later pair win, same as
                // imgproxy's own "redefines" wording implies.
                "fq" => {
                    if args.is_empty() || !args.len().is_multiple_of(2) {
                        return Err(UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: "expected one or more format:quality pairs".to_string(),
                        });
                    }
                    for pair in args.chunks(2) {
                        let (format, value) = (pair[0], pair[1]);
                        let quality = parse_bounded(value, segment, 0, 100)?;
                        match format {
                            "jpg" | "jpeg" => opts.jpeg_quality = Some(quality),
                            "webp" => opts.webp_quality = Some(quality),
                            "png" => {
                                return Err(UrlParseError::InvalidOptionValue {
                                    option: segment.to_string(),
                                    reason: "png has no adjustable quality in this encoder \
                                             (PNG output always uses fixed CompressionType::Best)"
                                        .to_string(),
                                });
                            }
                            other => {
                                return Err(UrlParseError::InvalidOptionValue {
                                    option: segment.to_string(),
                                    reason: format!(
                                        "unsupported format {other:?} for format_quality \
                                         (expected jpg, jpeg or webp)"
                                    ),
                                });
                            }
                        }
                    }
                }
                // webpo:{compression} - a deliberately partial implementation
                // of imgproxy's `webp_options`/`webpo:{compression}:
                // {smart_subsample}:{preset}` option (#35,
                // <https://docs.imgproxy.net/usage/processing#webp-options>).
                // Only the `compression` slot is implemented - the `webp`
                // crate (0.3.1) this project depends on exposes exactly two
                // encode modes, `Encoder::encode` (lossy) and
                // `Encoder::encode_lossless`, with no `smart_subsample` or
                // `preset` knob at all, and no `mixed` mode either. Requiring
                // exactly one argument (rather than silently accepting and
                // ignoring imgproxy's other two slots) is deliberate: a
                // caller who actually needs `smart_subsample`/`preset` gets a
                // clear 400 here instead of a silently-ignored parameter.
                "webpo" => {
                    let [compression] = require_args::<1>(&args, segment)?;
                    opts.webp_lossless = Some(match compression {
                        "lossy" => false,
                        "lossless" => true,
                        other => {
                            return Err(UrlParseError::InvalidOptionValue {
                                option: segment.to_string(),
                                reason: format!(
                                    "{other:?} is not a supported webp compression mode \
                                     (expected lossy or lossless - this encoder has no \
                                     mixed mode, and does not support imgproxy's \
                                     smart_subsample/preset slots)"
                                ),
                            });
                        }
                    });
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
                // bg:{R}:{G}:{B} or bg:{hex} (imgproxy's `background`/`bg`
                // option, #34/#60). See `parse_background` for the accepted
                // argument shapes.
                "bg" => {
                    opts.background = Some(parse_background(&args, segment)?);
                }
                // ar:{0|1|true|false} - imgproxy's `auto_rotate`/`ar`
                // processing option (#33,
                // <https://docs.imgproxy.net/usage/processing#auto-rotate>):
                // rotates/flips the decoded image per its EXIF orientation
                // tag before any resize happens. Carried through to
                // `ResizeQuery::autorotate`, defaulting to `true` when this
                // segment is absent (see that field's doc comment).
                "ar" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.autorotate = Some(parse_bool(value, segment)?);
                }
                // c:{width}:{height}[:{gravity_tokens...}] - imgproxy's
                // explicit crop (#50, <https://docs.imgproxy.net/usage/processing#crop>).
                // `width`/`height` follow the same 0/absolute/relative
                // convention as `rs`'s dimensions, generalised to allow a
                // fractional (`(0, 1)`) value too - see `parse_crop_dimension`.
                // A trailing gravity (`no`/`so`/.../`fp:{x}:{y}`) is
                // optional; when absent, resolution to the top-level `gr:`
                // value happens after this loop (`crop_raw` above).
                "c" => {
                    if args.len() < 2 {
                        return Err(UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: "expected at least 2 arguments (width:height[:gravity...])"
                                .to_string(),
                        });
                    }
                    let width = parse_crop_dimension(args[0], segment)?;
                    let height = parse_crop_dimension(args[1], segment)?;
                    let gravity = if args.len() > 2 {
                        Some(parse_gravity(&args[2..], segment)?)
                    } else {
                        None
                    };
                    crop_raw = Some((width, height, gravity));
                }
                // gr:{type}[:{x}:{y}] - imgproxy's `g:` gravity option (#50,
                // <https://docs.imgproxy.net/usage/processing#gravity>),
                // under the `gr` code rather than `g` since this crate's `g`
                // is already grayscale (see `ProcessingOptions`'s doc
                // comment). Controls the `ResizeType::Fill`/`Auto`-as-fill
                // cover-crop's anchor, and doubles as `c:`'s default anchor
                // when `c:` doesn't name its own gravity.
                "gr" => {
                    if args.is_empty() {
                        return Err(UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: "expected at least 1 argument (gravity type)".to_string(),
                        });
                    }
                    opts.gravity = parse_gravity(&args, segment)?;
                }
                // --- #51 additions start (kept contiguous, matching the
                // `generate_key` block in `src/services/cache/handler.rs`).
                //
                // rot:{angle} (imgproxy's `rotate`/`rot`). Only a multiple
                // of 90 (imgproxy's own restriction) is accepted; negative
                // angles are allowed (`rot:-90`) and normalised to
                // 0/90/180/270 via `rem_euclid`, same as imgproxy's own
                // `%180`-based checks internally treat them.
                "rot" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.rotate = parse_rotate_angle(value, segment)?;
                }
                // fl:{horizontal}:{vertical} (imgproxy's `flip`/`fl`). Both
                // slots are optional/independent bools, default false -
                // `fl:1` flips only horizontally, `fl:1:1` flips both.
                "fl" => {
                    if args.len() > 2 {
                        return Err(UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: "expected at most 2 arguments (horizontal:vertical)"
                                .to_string(),
                        });
                    }
                    if let Some(h) = args.first().filter(|s| !s.is_empty()) {
                        opts.flip_horizontal = parse_bool(h, segment)?;
                    }
                    if let Some(v) = args.get(1).filter(|s| !s.is_empty()) {
                        opts.flip_vertical = parse_bool(v, segment)?;
                    }
                }
                // t:{threshold}:{color}:{equal_hor}:{equal_ver} (imgproxy's
                // `trim`/`t`). Only `threshold` is required; `color`
                // defaults to auto-detecting the background from the
                // image's top-left corner pixel (see
                // `ImageService::apply_trim`), `equal_hor`/`equal_ver`
                // default to `false`.
                "t" => {
                    opts.trim = Some(parse_trim(&args, segment)?);
                }
                // ex:{enabled} (imgproxy's `extend`/`ex`). Exactly 1
                // argument - imgproxy's optional second `:gravity`
                // argument is rejected (400) rather than silently ignored,
                // since gravity/crop as a whole is out of #51's scope (see
                // `ResizeQuery::extend`'s doc comment).
                "ex" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.extend = Some(parse_bool(value, segment)?);
                }
                // pd:{top}:{right}:{bottom}:{left} (imgproxy's `padding`/
                // `pd`), CSS-style cascading-fallback shorthand - see
                // `parse_padding`.
                "pd" => {
                    opts.padding = Some(parse_padding(&args, segment)?);
                }
                // z:{zoom} or z:{zoom_x}:{zoom_y} (imgproxy's `zoom`/`z`).
                // A single argument sets both axes equally.
                "z" => {
                    let (zoom_x, zoom_y) = parse_zoom(&args, segment)?;
                    opts.zoom_x = zoom_x;
                    opts.zoom_y = zoom_y;
                }
                // dpr:{value} (imgproxy's `dpr`, no short alias).
                "dpr" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.dpr = parse_positive_nonzero_float(value, segment)?;
                }
                // mw:{width} / mh:{height} (imgproxy's `min-width`/`mw` and
                // `min-height`/`mh`). `0` means "not set", same convention
                // `parse_dimension` already uses for `width`/`height`.
                "mw" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.min_width = parse_dimension(value, segment)?;
                }
                "mh" => {
                    let [value] = require_args::<1>(&args, segment)?;
                    opts.min_height = parse_dimension(value, segment)?;
                }
                // --- #51 additions end.
                // wm:{opacity}[:{position}[:{x_offset}[:{y_offset}[:{scale}]]]]
                // (#52, imgproxy's `watermark`/`wm`). Only `opacity` is
                // required; every trailing slot may be omitted (shorter
                // segment) or left blank (`wm:0.5::10`) to keep its
                // default, mirroring `rs`'s empty-slot convention.
                "wm" => parse_watermark(&args, segment, &mut opts)?,
                // wmu:{base64url} (#52, imgproxy Pro's `watermark_url`) -
                // an arbitrary per-request watermark source. Decoded here;
                // SSRF-validated later, at fetch time
                // (`ImageService::process_image`), through the same guard
                // as the main source URL.
                "wmu" => {
                    let [encoded] = require_args::<1>(&args, segment)?;
                    let decoded = URL_SAFE_NO_PAD.decode(encoded).map_err(|e| {
                        UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: format!("invalid base64url watermark URL: {e}"),
                        }
                    })?;
                    let url = String::from_utf8(decoded).map_err(|e| {
                        UrlParseError::InvalidOptionValue {
                            option: segment.to_string(),
                            reason: format!("watermark URL is not valid UTF-8: {e}"),
                        }
                    })?;
                    opts.watermark_url = Some(url);
                }
                // wms:{width}:{height} (#52, imgproxy Pro's
                // `watermark_size`) - either may be `0`, meaning "derive
                // from the other by the watermark's own aspect ratio".
                "wms" => {
                    let [w, h] = require_args::<2>(&args, segment)?;
                    let width = parse_dimension_allow_zero(w, segment)?;
                    let height = parse_dimension_allow_zero(h, segment)?;
                    opts.watermark_size = Some((width, height));
                }
                // wmr:{angle} (#52, imgproxy Pro's `watermark_rotate`) -
                // clockwise degrees.
                "wmr" => {
                    let [angle] = require_args::<1>(&args, segment)?;
                    opts.watermark_rotate = parse_float(angle, segment)?;
                }
                // wmsh:{sigma} (#52, imgproxy Pro's `watermark_shadow`).
                "wmsh" => {
                    let [sigma] = require_args::<1>(&args, segment)?;
                    opts.watermark_shadow = Some(parse_float(sigma, segment)?);
                }
                other => return Err(UrlParseError::UnknownOption(other.to_string())),
            }
        }

        opts.crop = crop_raw.map(|(width, height, gravity)| Crop {
            width,
            height,
            gravity: gravity.unwrap_or(opts.gravity),
        });

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

/// Like [`parse_dimension`] but keeps `0` as `0` instead of mapping it to
/// `None` - `wms`'s "derive from the other dimension" convention needs to
/// distinguish "explicitly zero" from "positive", not collapse it to "unset"
/// the way `rs`'s width/height do.
fn parse_dimension_allow_zero(raw: &str, segment: &str) -> Result<u32, UrlParseError> {
    raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid unsigned integer"),
    })
}

/// Parses `wm`'s variable-length argument list
/// (`{opacity}[:{position}[:{x_offset}[:{y_offset}[:{scale}]]]]`, #52) into
/// `opts`'s `watermark_*` fields. Every slot past `opacity` may be omitted
/// entirely (a shorter segment) or left blank (`wm:0.5::10`) to keep its
/// default - the same "empty positional argument means use the default"
/// convention `rs`'s type slot already uses.
fn parse_watermark(
    args: &[&str],
    segment: &str,
    opts: &mut ProcessingOptions,
) -> Result<(), UrlParseError> {
    if args.is_empty() || args.len() > 5 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "expected 1 to 5 arguments: opacity[:position[:x_offset[:y_offset[:scale]]]]"
                .to_string(),
        });
    }

    opts.watermark_opacity = Some(parse_float(args[0], segment)?);

    if let Some(position) = args.get(1).filter(|s| !s.is_empty()) {
        opts.watermark_position = parse_watermark_position(position, segment)?;
    }
    if let Some(x_offset) = args.get(2).filter(|s| !s.is_empty()) {
        opts.watermark_x_offset = parse_float(x_offset, segment)?;
    }
    if let Some(y_offset) = args.get(3).filter(|s| !s.is_empty()) {
        opts.watermark_y_offset = parse_float(y_offset, segment)?;
    }
    if let Some(scale) = args.get(4).filter(|s| !s.is_empty()) {
        opts.watermark_scale = parse_float(scale, segment)?;
    }

    Ok(())
}

/// Parses `wm`'s position slot (#52), imgproxy's own short codes
/// (<https://docs.imgproxy.net/usage/processing#watermark>). `re`
/// (repeat/tile) and `ch` (chessboard) are documented but not implemented -
/// rejected the same way an unsupported `rs` type is, rather than silently
/// falling back to a different position.
fn parse_watermark_position(raw: &str, segment: &str) -> Result<WatermarkPosition, UrlParseError> {
    match raw {
        "ce" => Ok(WatermarkPosition::Center),
        "no" => Ok(WatermarkPosition::North),
        "so" => Ok(WatermarkPosition::South),
        "ea" => Ok(WatermarkPosition::East),
        "we" => Ok(WatermarkPosition::West),
        "noea" => Ok(WatermarkPosition::NorthEast),
        "nowe" => Ok(WatermarkPosition::NorthWest),
        "soea" => Ok(WatermarkPosition::SouthEast),
        "sowe" => Ok(WatermarkPosition::SouthWest),
        other => Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!(
                "{other:?} is not a supported watermark position (expected ce, no, so, ea, we, \
                 noea, nowe, soea or sowe - re/ch tiling is not supported)"
            ),
        }),
    }
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

/// Parses `bg`'s argument list into an `[R, G, B]` triple. imgproxy accepts
/// two shapes for this option
/// (<https://docs.imgproxy.net/usage/processing#background>):
/// - `bg:{R}:{G}:{B}` - three separate 0-255 channel values (3 args here).
/// - `bg:{hex_color}` - a single hex-coded colour (1 arg here). Accepts
///   3-digit (`fff`) or 6-digit (`ffffff`) hex, case-insensitively, with no
///   leading `#` - a literal `#` would need percent-encoding to survive as
///   a path segment, so this mirrors how imgproxy's own examples write hex
///   colours in URLs.
///
/// Any other argument count is rejected the same way `require_args` rejects
/// a wrong count for the fixed-arity options above.
fn parse_background(args: &[&str], segment: &str) -> Result<[u8; 3], UrlParseError> {
    match args {
        [r, g, b] => Ok([
            parse_bounded(r, segment, 0, 255)?,
            parse_bounded(g, segment, 0, 255)?,
            parse_bounded(b, segment, 0, 255)?,
        ]),
        [hex] => parse_hex_color(hex, segment),
        _ => Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "expected either 3 arguments (R:G:B) or 1 argument (hex colour)".to_string(),
        }),
    }
}

/// Parses a 3-digit or 6-digit hex colour (no leading `#`) into `[R, G, B]`.
fn parse_hex_color(hex: &str, segment: &str) -> Result<[u8; 3], UrlParseError> {
    let invalid = || UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{hex:?} is not a valid hex colour (expected 3 or 6 hex digits)"),
    };

    // Guard against non-ASCII input before any byte-index slicing below -
    // `hex.len()` is a byte length, and slicing on a non-char boundary would
    // panic rather than fall through to the length check.
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid());
    }

    let expand = |c: char| -> Result<u8, UrlParseError> {
        let digit = c.to_digit(16).ok_or_else(invalid)? as u8;
        Ok(digit * 16 + digit)
    };

    match hex.len() {
        3 => {
            let mut chars = hex.chars();
            let r = expand(chars.next().ok_or_else(invalid)?)?;
            let g = expand(chars.next().ok_or_else(invalid)?)?;
            let b = expand(chars.next().ok_or_else(invalid)?)?;
            Ok([r, g, b])
        }
        6 => {
            let channel = |slice: &str| -> Result<u8, UrlParseError> {
                u8::from_str_radix(slice, 16).map_err(|_| invalid())
            };
            Ok([
                channel(&hex[0..2])?,
                channel(&hex[2..4])?,
                channel(&hex[4..6])?,
            ])
        }
        _ => Err(invalid()),
    }
}

/// Parses one of `c:`'s `width`/`height` slots (#50). `0` means "use the
/// full source dimension on this axis" (imgproxy's own convention, mirrored
/// from `parse_dimension` above but generalised to allow the `(0, 1)`
/// relative-fraction form `rs` doesn't have).
fn parse_crop_dimension(raw: &str, segment: &str) -> Result<CropDimension, UrlParseError> {
    let value: f64 = raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid number"),
    })?;

    if !value.is_finite() || value < 0.0 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{value} must be a non-negative, finite number"),
        });
    }

    Ok(if value == 0.0 {
        CropDimension::Full
    } else if value < 1.0 {
        CropDimension::Relative(value)
    } else {
        CropDimension::Absolute(value.round() as u32)
    })
}

/// Parses a gravity-type token list (#50) - shared by `c:`'s optional
/// trailing gravity and `gr:`'s own arguments. `tokens` is never empty at
/// the call sites (both check first, with a message naming the *outer*
/// option), so an empty slice here is an internal-logic error rather than a
/// user-facing one - it still returns a well-formed `UrlParseError` instead
/// of panicking, in case that invariant is ever broken by a future edit.
///
/// Deliberately does **not** accept `sm` (smart/saliency) or `obj`/`objw`
/// (object-detection, imgproxy Pro-only) - see [`crate::models::params::Gravity`]'s
/// doc comment for why. Both fall into the trailing `[other, ..]` arm and
/// are rejected exactly like any other unrecognised token, not silently
/// aliased to a real gravity.
fn parse_gravity(tokens: &[&str], segment: &str) -> Result<Gravity, UrlParseError> {
    let invalid = |reason: String| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason,
    };

    match tokens {
        ["ce"] => Ok(Gravity::Center),
        ["no"] => Ok(Gravity::North),
        ["so"] => Ok(Gravity::South),
        ["ea"] => Ok(Gravity::East),
        ["we"] => Ok(Gravity::West),
        ["noea"] => Ok(Gravity::NorthEast),
        ["nowe"] => Ok(Gravity::NorthWest),
        ["soea"] => Ok(Gravity::SouthEast),
        ["sowe"] => Ok(Gravity::SouthWest),
        ["fp", x, y] => {
            let x: f64 = x
                .parse()
                .map_err(|_| invalid(format!("{x:?} is not a valid focus-point x")))?;
            let y: f64 = y
                .parse()
                .map_err(|_| invalid(format!("{y:?} is not a valid focus-point y")))?;
            if !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
                return Err(invalid(format!(
                    "focus point ({x}, {y}) must be within [0, 1] on both axes"
                )));
            }
            Ok(Gravity::FocusPoint { x, y })
        }
        [] => Err(invalid("expected a gravity type".to_string())),
        [other, ..] => Err(invalid(format!(
            "unsupported gravity type {other:?} (expected no/so/ea/we/noea/nowe/soea/sowe/ce/fp:x:y; \
             smart gravity (sm) is not yet supported)"
        ))),
    }
}

// --- #51 additions start: parsers for rotate/flip/trim/extend/padding/
// zoom/dpr/min-width/min-height. Kept as one contiguous block after the
// pre-existing parsers above, for the same integration-diff-legibility
// reason as the other #51 blocks in this file.

/// Parses `rotate`/`rot`'s `{angle}` argument: any integer (imgproxy
/// accepts negative angles too), required to be a multiple of 90, then
/// normalised into `0..360` via `rem_euclid` so
/// `ImageService::apply_rotate` only ever has to match on exactly
/// `0`/`90`/`180`/`270`.
fn parse_rotate_angle(raw: &str, segment: &str) -> Result<i32, UrlParseError> {
    let angle: i32 = raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid integer"),
    })?;

    if angle % 90 != 0 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{angle} is not a multiple of 90"),
        });
    }

    Ok(angle.rem_euclid(360))
}

/// Parses `zoom`/`z`'s `{zoom_x}` or `{zoom_x}:{zoom_y}` arguments. A
/// single argument sets both axes equally (imgproxy: "if only the first
/// value is set, imgproxy will use it for both axes").
fn parse_zoom(args: &[&str], segment: &str) -> Result<(f32, f32), UrlParseError> {
    match args {
        [zoom] => {
            let z = parse_positive_nonzero_float(zoom, segment)?;
            Ok((z, z))
        }
        [zoom_x, zoom_y] => Ok((
            parse_positive_nonzero_float(zoom_x, segment)?,
            parse_positive_nonzero_float(zoom_y, segment)?,
        )),
        _ => Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "expected 1 argument (zoom) or 2 arguments (zoom_x:zoom_y)".to_string(),
        }),
    }
}

/// Parses a strictly-positive (non-zero) `f32` option value - `zoom`'s and
/// `dpr`'s shared shape (imgproxy: "the value must be greater than 0" for
/// both).
fn parse_positive_nonzero_float(raw: &str, segment: &str) -> Result<f32, UrlParseError> {
    let value = parse_float(raw, segment)?;
    if value > 0.0 {
        Ok(value)
    } else {
        Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{value} is not a positive number"),
        })
    }
}

/// Parses `padding`/`pd`'s `{top}:{right}:{bottom}:{left}` arguments,
/// reproducing imgproxy's own CSS-shorthand-like cascading-fallback parse
/// (`options/parser/apply.go`'s `applyPaddingOption`, verified against the
/// `imgproxy/imgproxy` v4 source at the time of writing) *exactly*,
/// including which slot falls back to which when omitted (an empty
/// positional slot, not just a trailing-argument omission, also triggers
/// the fallback - `pd:10::30` behaves the same as `pd:10:` followed by an
/// explicit `30` for bottom):
///   - `right` falls back to `top` when omitted/empty.
///   - `bottom` falls back to `top` when omitted/empty (not `right`).
///   - `left` falls back to `right` (its own already-resolved value, which
///     may itself have fallen back to `top`) when omitted/empty.
/// This is what reproduces CSS's familiar 1/2/3/4-value shorthand
/// (`pd:10` -> all sides 10; `pd:10:20` -> top/bottom 10, left/right 20;
/// `pd:10:20:30` -> top 10, left/right 20, bottom 30) even though the
/// underlying parse is positional-with-fallback, not a value-count switch.
/// At least one of the (up to 4) arguments must be present and non-empty.
fn parse_padding(args: &[&str], segment: &str) -> Result<Padding, UrlParseError> {
    if args.is_empty() || args.len() > 4 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "expected 1 to 4 arguments (top:right:bottom:left)".to_string(),
        });
    }

    let slot = |value: Option<&&str>| -> Result<Option<u32>, UrlParseError> {
        match value.filter(|s| !s.is_empty()) {
            Some(s) => Ok(Some(parse_non_negative_int(s, segment)?)),
            None => Ok(None),
        }
    };

    let top = slot(args.first())?.ok_or_else(|| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: "at least the top argument must be set".to_string(),
    })?;
    let right = slot(args.get(1))?.unwrap_or(top);
    let bottom = slot(args.get(2))?.unwrap_or(top);
    let left = slot(args.get(3))?.unwrap_or(right);

    Ok(Padding {
        top,
        right,
        bottom,
        left,
    })
}

fn parse_non_negative_int(raw: &str, segment: &str) -> Result<u32, UrlParseError> {
    raw.parse().map_err(|_| UrlParseError::InvalidOptionValue {
        option: segment.to_string(),
        reason: format!("{raw:?} is not a valid non-negative integer"),
    })
}

/// Parses `trim`/`t`'s `{threshold}:{color}:{equal_hor}:{equal_ver}`
/// arguments. `threshold` is required; `color`/`equal_hor`/`equal_ver` are
/// each optional and independently omittable (an empty positional slot
/// keeps that field at its default), matching imgproxy's own `applyTrimOption`
/// argument handling.
fn parse_trim(args: &[&str], segment: &str) -> Result<TrimOptions, UrlParseError> {
    if args.is_empty() || args.len() > 4 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "expected 1 to 4 arguments (threshold:color:equal_hor:equal_ver)".to_string(),
        });
    }

    let threshold_raw = args[0];
    if threshold_raw.is_empty() {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: "threshold is required".to_string(),
        });
    }
    let threshold = parse_float(threshold_raw, segment)?;
    if threshold < 0.0 {
        return Err(UrlParseError::InvalidOptionValue {
            option: segment.to_string(),
            reason: format!("{threshold} is not a non-negative number"),
        });
    }

    let color = match args.get(1).filter(|s| !s.is_empty()) {
        Some(hex) => Some(parse_hex_color(hex, segment)?),
        None => None,
    };

    let equal_hor = match args.get(2).filter(|s| !s.is_empty()) {
        Some(v) => parse_bool(v, segment)?,
        None => false,
    };

    let equal_ver = match args.get(3).filter(|s| !s.is_empty()) {
        Some(v) => parse_bool(v, segment)?,
        None => false,
    };

    Ok(TrimOptions {
        threshold,
        color,
        equal_hor,
        equal_ver,
    })
}
// --- #51 additions end.

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
    fn parses_format_quality_for_jpeg_and_webp() {
        let opts = ProcessingOptions::parse(&["fq:jpg:80:webp:90"]).unwrap();
        assert_eq!(opts.jpeg_quality, Some(80));
        assert_eq!(opts.webp_quality, Some(90));
    }

    #[test]
    fn format_quality_accepts_jpeg_alias() {
        let opts = ProcessingOptions::parse(&["fq:jpeg:70"]).unwrap();
        assert_eq!(opts.jpeg_quality, Some(70));
    }

    #[test]
    fn format_quality_later_pair_overrides_earlier_for_same_format() {
        let opts = ProcessingOptions::parse(&["fq:jpg:80:jpg:40"]).unwrap();
        assert_eq!(opts.jpeg_quality, Some(40));
    }

    #[test]
    fn format_quality_rejects_png() {
        assert!(ProcessingOptions::parse(&["fq:png:80"]).is_err());
    }

    #[test]
    fn format_quality_rejects_unknown_format() {
        assert!(ProcessingOptions::parse(&["fq:avif:80"]).is_err());
    }

    #[test]
    fn format_quality_rejects_odd_argument_count() {
        assert!(ProcessingOptions::parse(&["fq:jpg:80:webp"]).is_err());
    }

    #[test]
    fn format_quality_rejects_out_of_range_value() {
        assert!(ProcessingOptions::parse(&["fq:jpg:101"]).is_err());
    }

    #[test]
    fn format_quality_rejects_empty_args() {
        assert!(ProcessingOptions::parse(&["fq"]).is_err());
    }

    #[test]
    fn parses_webp_lossless() {
        assert_eq!(
            ProcessingOptions::parse(&["webpo:lossless"])
                .unwrap()
                .webp_lossless,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["webpo:lossy"])
                .unwrap()
                .webp_lossless,
            Some(false)
        );
    }

    #[test]
    fn webp_lossless_rejects_unsupported_mode() {
        assert!(ProcessingOptions::parse(&["webpo:mixed"]).is_err());
        assert!(ProcessingOptions::parse(&["webpo:nope"]).is_err());
    }

    #[test]
    fn webp_lossless_rejects_extra_arguments() {
        // imgproxy's own 3-slot grammar (compression:smart_subsample:preset)
        // - this crate only implements the first slot, and rejects rather
        // than silently ignoring the other two (see the `webpo` match arm's
        // doc comment).
        assert!(ProcessingOptions::parse(&["webpo:lossless::4"]).is_err());
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
    fn parses_autorotate() {
        assert_eq!(
            ProcessingOptions::parse(&["ar:1"]).unwrap().autorotate,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["ar:0"]).unwrap().autorotate,
            Some(false)
        );
        assert_eq!(
            ProcessingOptions::parse(&["ar:true"]).unwrap().autorotate,
            Some(true)
        );
        assert_eq!(
            ProcessingOptions::parse(&["ar:false"]).unwrap().autorotate,
            Some(false)
        );
    }

    #[test]
    fn autorotate_defaults_to_none_when_absent() {
        let opts = ProcessingOptions::parse(&[]).unwrap();
        assert_eq!(opts.autorotate, None);
    }

    #[test]
    fn combines_multiple_options() {
        let opts = ProcessingOptions::parse(&[
            "rs:fill:300:300",
            "q:80",
            "fq:webp:90",
            "webpo:lossless",
            "bl:5",
            "g:true",
            "el:1",
            "bg:255:0:0",
            "ar:0",
        ])
        .unwrap();
        assert_eq!(opts.width, Some(300));
        assert_eq!(opts.height, Some(300));
        assert_eq!(opts.resize_type, ResizeType::Fill);
        assert_eq!(opts.quality, Some(80));
        assert_eq!(opts.webp_quality, Some(90));
        assert_eq!(opts.webp_lossless, Some(true));
        assert_eq!(opts.blur_sigma, Some(5.0));
        assert_eq!(opts.grayscale, Some(true));
        assert_eq!(opts.enlarge, Some(true));
        assert_eq!(opts.background, Some([255, 0, 0]));
        assert_eq!(opts.autorotate, Some(false));
    }

    #[test]
    fn parses_background_as_rgb_triple() {
        let opts = ProcessingOptions::parse(&["bg:255:128:0"]).unwrap();
        assert_eq!(opts.background, Some([255, 128, 0]));
    }

    #[test]
    fn parses_background_as_six_digit_hex() {
        let opts = ProcessingOptions::parse(&["bg:ff8000"]).unwrap();
        assert_eq!(opts.background, Some([255, 128, 0]));
    }

    #[test]
    fn parses_background_as_six_digit_hex_uppercase() {
        let opts = ProcessingOptions::parse(&["bg:FF8000"]).unwrap();
        assert_eq!(opts.background, Some([255, 128, 0]));
    }

    #[test]
    fn parses_background_as_three_digit_hex_shorthand() {
        // `f80` expands digit-doubled to `ff8800`, imgproxy/CSS shorthand
        // convention.
        let opts = ProcessingOptions::parse(&["bg:f80"]).unwrap();
        assert_eq!(opts.background, Some([255, 136, 0]));
    }

    #[test]
    fn background_defaults_to_none_when_absent() {
        let opts = ProcessingOptions::parse(&[]).unwrap();
        assert_eq!(opts.background, None);
    }

    #[test]
    fn background_rgb_out_of_range_is_rejected() {
        assert!(ProcessingOptions::parse(&["bg:256:0:0"]).is_err());
    }

    #[test]
    fn background_invalid_hex_is_rejected() {
        assert!(ProcessingOptions::parse(&["bg:zzzzzz"]).is_err());
        assert!(ProcessingOptions::parse(&["bg:ff"]).is_err());
        assert!(ProcessingOptions::parse(&["bg:1234567"]).is_err());
    }

    #[test]
    fn background_wrong_argument_count_is_rejected() {
        assert!(ProcessingOptions::parse(&["bg:1:2"]).is_err());
        assert!(ProcessingOptions::parse(&["bg:1:2:3:4"]).is_err());
        assert!(ProcessingOptions::parse(&["bg"]).is_err());
    }

    #[test]
    fn background_rejects_non_ascii_without_panicking() {
        // A multi-byte UTF-8 character here must not panic on byte-index
        // slicing inside `parse_hex_color` - it should just be rejected.
        assert!(ProcessingOptions::parse(&["bg:é"]).is_err());
        assert!(ProcessingOptions::parse(&["bg:ééé"]).is_err());
    }

    #[test]
    fn parses_watermark_opacity_only() {
        let opts = ProcessingOptions::parse(&["wm:0.5"]).unwrap();
        assert_eq!(opts.watermark_opacity, Some(0.5));
        assert_eq!(opts.watermark_position, WatermarkPosition::Center);
        assert_eq!(opts.watermark_x_offset, 0.0);
        assert_eq!(opts.watermark_y_offset, 0.0);
        assert_eq!(opts.watermark_scale, 0.0);
    }

    #[test]
    fn parses_watermark_with_every_slot() {
        let opts = ProcessingOptions::parse(&["wm:0.8:soea:10:-5:0.2"]).unwrap();
        assert_eq!(opts.watermark_opacity, Some(0.8));
        assert_eq!(opts.watermark_position, WatermarkPosition::SouthEast);
        assert_eq!(opts.watermark_x_offset, 10.0);
        assert_eq!(opts.watermark_y_offset, -5.0);
        assert_eq!(opts.watermark_scale, 0.2);
    }

    #[test]
    fn parses_every_watermark_position() {
        for (token, expected) in [
            ("ce", WatermarkPosition::Center),
            ("no", WatermarkPosition::North),
            ("so", WatermarkPosition::South),
            ("ea", WatermarkPosition::East),
            ("we", WatermarkPosition::West),
            ("noea", WatermarkPosition::NorthEast),
            ("nowe", WatermarkPosition::NorthWest),
            ("soea", WatermarkPosition::SouthEast),
            ("sowe", WatermarkPosition::SouthWest),
        ] {
            let segment = format!("wm:1:{token}");
            let opts = ProcessingOptions::parse(&[&segment]).unwrap();
            assert_eq!(opts.watermark_position, expected, "position token {token:?}");
        }
    }

    #[test]
    fn watermark_empty_trailing_slots_keep_defaults() {
        let opts = ProcessingOptions::parse(&["wm:0.5::10"]).unwrap();
        assert_eq!(opts.watermark_opacity, Some(0.5));
        assert_eq!(opts.watermark_position, WatermarkPosition::Center);
        assert_eq!(opts.watermark_x_offset, 10.0);
    }

    #[test]
    fn watermark_missing_opacity_is_rejected() {
        assert!(ProcessingOptions::parse(&["wm"]).is_err());
    }

    #[test]
    fn watermark_blank_opacity_is_rejected() {
        // Unlike the trailing modifier slots, opacity is required and not
        // exempt from the "blank means default" convention - there is no
        // sane default opacity to fall back to.
        assert!(ProcessingOptions::parse(&["wm:"]).is_err());
    }

    #[test]
    fn watermark_too_many_arguments_is_rejected() {
        assert!(ProcessingOptions::parse(&["wm:1:ce:0:0:1:extra"]).is_err());
    }

    #[test]
    fn watermark_unknown_position_is_rejected() {
        assert!(ProcessingOptions::parse(&["wm:1:re"]).is_err()); // tiling not supported
        assert!(ProcessingOptions::parse(&["wm:1:bogus"]).is_err());
    }

    #[test]
    fn parses_watermark_url_from_base64() {
        let encoded = URL_SAFE_NO_PAD.encode("https://example.com/logo.png");
        let segment = format!("wmu:{encoded}");
        let opts = ProcessingOptions::parse(&[&segment]).unwrap();
        assert_eq!(
            opts.watermark_url,
            Some("https://example.com/logo.png".to_string())
        );
    }

    #[test]
    fn watermark_url_invalid_base64_is_rejected() {
        assert!(ProcessingOptions::parse(&["wmu:not-valid-base64!!!"]).is_err());
    }

    #[test]
    fn parses_watermark_size() {
        let opts = ProcessingOptions::parse(&["wms:100:0"]).unwrap();
        assert_eq!(opts.watermark_size, Some((100, 0)));
    }

    #[test]
    fn watermark_size_wrong_argument_count_is_rejected() {
        assert!(ProcessingOptions::parse(&["wms:100"]).is_err());
    }

    #[test]
    fn parses_watermark_rotate() {
        let opts = ProcessingOptions::parse(&["wmr:45"]).unwrap();
        assert_eq!(opts.watermark_rotate, 45.0);
    }

    #[test]
    fn parses_watermark_shadow() {
        let opts = ProcessingOptions::parse(&["wmsh:3.5"]).unwrap();
        assert_eq!(opts.watermark_shadow, Some(3.5));
    }

    #[test]
    fn watermark_fields_default_to_disabled() {
        let opts = ProcessingOptions::parse(&[]).unwrap();
        assert_eq!(opts.watermark_opacity, None);
        assert_eq!(opts.watermark_url, None);
        assert_eq!(opts.watermark_size, None);
        assert_eq!(opts.watermark_rotate, 0.0);
        assert_eq!(opts.watermark_shadow, None);
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
        assert_eq!(
            ProcessingOptions::parse(&[]).unwrap(),
            ProcessingOptions::default()
        );
    }

    #[test]
    fn looks_like_option_distinguishes_options_from_base64() {
        assert!(looks_like_option("q:80"));
        assert!(looks_like_option("rs:fill:300:300"));
        // base64url never contains ':'.
        assert!(!looks_like_option("aHR0cHM6Ly9leGFtcGxlLmNvbQ"));
        assert!(!looks_like_option("plain"));
    }

    // ---- #50: crop (`c`) and gravity (`gr`) ----

    #[test]
    fn parses_every_directional_and_corner_gravity() {
        for (token, expected) in [
            ("ce", Gravity::Center),
            ("no", Gravity::North),
            ("so", Gravity::South),
            ("ea", Gravity::East),
            ("we", Gravity::West),
            ("noea", Gravity::NorthEast),
            ("nowe", Gravity::NorthWest),
            ("soea", Gravity::SouthEast),
            ("sowe", Gravity::SouthWest),
        ] {
            let segment = format!("gr:{token}");
            let opts = ProcessingOptions::parse(&[&segment]).unwrap();
            assert_eq!(opts.gravity, expected, "gravity token {token:?}");
        }
    }

    #[test]
    fn parses_focus_point_gravity() {
        let opts = ProcessingOptions::parse(&["gr:fp:0.25:0.75"]).unwrap();
        assert_eq!(opts.gravity, Gravity::FocusPoint { x: 0.25, y: 0.75 });
    }

    #[test]
    fn focus_point_out_of_range_is_rejected() {
        assert!(ProcessingOptions::parse(&["gr:fp:1.5:0.5"]).is_err());
        assert!(ProcessingOptions::parse(&["gr:fp:0.5:-0.1"]).is_err());
    }

    #[test]
    fn gravity_defaults_to_center_when_absent() {
        let opts = ProcessingOptions::parse(&[]).unwrap();
        assert_eq!(opts.gravity, Gravity::Center);
    }

    #[test]
    fn gravity_missing_type_argument_is_rejected() {
        assert!(ProcessingOptions::parse(&["gr:"]).is_err());
    }

    /// #50: smart gravity is explicitly out of scope (see
    /// [`crate::models::params::Gravity`]'s doc comment) - `sm` must be
    /// rejected exactly like any other unrecognised token, not silently
    /// aliased to a real gravity.
    #[test]
    fn smart_and_object_gravity_are_rejected_as_not_yet_supported() {
        assert!(ProcessingOptions::parse(&["gr:sm"]).is_err());
        assert!(ProcessingOptions::parse(&["gr:obj:face"]).is_err());
        assert!(ProcessingOptions::parse(&["gr:objw:face:1"]).is_err());
    }

    #[test]
    fn parses_crop_with_absolute_dimensions_and_no_gravity() {
        let opts = ProcessingOptions::parse(&["c:300:200"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.width, CropDimension::Absolute(300));
        assert_eq!(crop.height, CropDimension::Absolute(200));
        // No gravity token on `c:` and no top-level `gr:` either -> falls
        // back to `Gravity::default()` (`Center`).
        assert_eq!(crop.gravity, Gravity::Center);
    }

    #[test]
    fn parses_crop_with_its_own_gravity_token() {
        let opts = ProcessingOptions::parse(&["c:300:200:noea"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.gravity, Gravity::NorthEast);
    }

    #[test]
    fn parses_crop_with_focus_point_gravity() {
        let opts = ProcessingOptions::parse(&["c:300:200:fp:0.1:0.9"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.gravity, Gravity::FocusPoint { x: 0.1, y: 0.9 });
    }

    #[test]
    fn crop_zero_dimension_means_full_axis() {
        let opts = ProcessingOptions::parse(&["c:0:200"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.width, CropDimension::Full);
        assert_eq!(crop.height, CropDimension::Absolute(200));
    }

    #[test]
    fn crop_fractional_dimension_under_one_is_relative() {
        let opts = ProcessingOptions::parse(&["c:0.5:0.25"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.width, CropDimension::Relative(0.5));
        assert_eq!(crop.height, CropDimension::Relative(0.25));
    }

    #[test]
    fn crop_without_gravity_token_inherits_the_top_level_gravity_option() {
        // `gr:` appears *after* `c:` here - resolution happens once every
        // segment has been parsed, so the order shouldn't matter (see
        // `ProcessingOptions::parse`'s `crop_raw` comment).
        let opts = ProcessingOptions::parse(&["c:300:200", "gr:sowe"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(crop.gravity, Gravity::SouthWest);
        assert_eq!(opts.gravity, Gravity::SouthWest);
    }

    #[test]
    fn crop_with_its_own_gravity_overrides_the_top_level_gravity_option() {
        let opts = ProcessingOptions::parse(&["c:300:200:noea", "gr:sowe"]).unwrap();
        let crop = opts.crop.expect("crop should be set");
        assert_eq!(
            crop.gravity,
            Gravity::NorthEast,
            "c:'s own gravity token must win over the top-level gr: value"
        );
        assert_eq!(
            opts.gravity,
            Gravity::SouthWest,
            "the top-level gravity option itself is unaffected by c:'s own gravity"
        );
    }

    #[test]
    fn crop_negative_or_non_numeric_dimension_is_rejected() {
        assert!(ProcessingOptions::parse(&["c:-5:200"]).is_err());
        assert!(ProcessingOptions::parse(&["c:notanumber:200"]).is_err());
    }

    #[test]
    fn crop_missing_arguments_is_rejected() {
        assert!(ProcessingOptions::parse(&["c:300"]).is_err());
        assert!(ProcessingOptions::parse(&["c"]).is_err());
    }

    #[test]
    fn crop_defaults_to_none_when_absent() {
        let opts = ProcessingOptions::parse(&[]).unwrap();
        assert_eq!(opts.crop, None);
    }

    // --- #51 grammar tests start ---

    #[test]
    fn defaults_have_neutral_zoom_and_dpr() {
        let opts = ProcessingOptions::default();
        assert_eq!(opts.zoom_x, 1.0);
        assert_eq!(opts.zoom_y, 1.0);
        assert_eq!(opts.dpr, 1.0);
        assert_eq!(opts.rotate, 0);
        assert!(!opts.flip_horizontal);
        assert!(!opts.flip_vertical);
        assert_eq!(opts.trim, None);
        assert_eq!(opts.extend, None);
        assert_eq!(opts.padding, None);
        assert_eq!(opts.min_width, None);
        assert_eq!(opts.min_height, None);
    }

    #[test]
    fn parses_rotate() {
        for (token, expected) in [("0", 0), ("90", 90), ("180", 180), ("270", 270)] {
            let segment = format!("rot:{token}");
            assert_eq!(ProcessingOptions::parse(&[&segment]).unwrap().rotate, expected);
        }
    }

    #[test]
    fn negative_rotate_angle_normalises_into_0_360() {
        assert_eq!(ProcessingOptions::parse(&["rot:-90"]).unwrap().rotate, 270);
        assert_eq!(ProcessingOptions::parse(&["rot:-360"]).unwrap().rotate, 0);
    }

    #[test]
    fn rotate_angle_not_a_multiple_of_90_is_rejected() {
        assert!(ProcessingOptions::parse(&["rot:45"]).is_err());
        assert!(ProcessingOptions::parse(&["rot:1"]).is_err());
    }

    #[test]
    fn parses_flip_both_axes_independently() {
        let opts = ProcessingOptions::parse(&["fl:1:0"]).unwrap();
        assert!(opts.flip_horizontal);
        assert!(!opts.flip_vertical);

        let opts = ProcessingOptions::parse(&["fl:0:1"]).unwrap();
        assert!(!opts.flip_horizontal);
        assert!(opts.flip_vertical);

        let opts = ProcessingOptions::parse(&["fl:true:true"]).unwrap();
        assert!(opts.flip_horizontal);
        assert!(opts.flip_vertical);
    }

    #[test]
    fn flip_single_argument_only_sets_horizontal() {
        let opts = ProcessingOptions::parse(&["fl:1"]).unwrap();
        assert!(opts.flip_horizontal);
        assert!(!opts.flip_vertical);
    }

    #[test]
    fn flip_with_too_many_arguments_is_rejected() {
        assert!(ProcessingOptions::parse(&["fl:1:1:1"]).is_err());
    }

    #[test]
    fn parses_trim_threshold_only() {
        let opts = ProcessingOptions::parse(&["t:10"]).unwrap();
        let trim = opts.trim.expect("trim should be set");
        assert_eq!(trim.threshold, 10.0);
        assert_eq!(trim.color, None);
        assert!(!trim.equal_hor);
        assert!(!trim.equal_ver);
    }

    #[test]
    fn parses_trim_with_all_arguments() {
        let opts = ProcessingOptions::parse(&["t:10:ff0000:1:true"]).unwrap();
        let trim = opts.trim.expect("trim should be set");
        assert_eq!(trim.threshold, 10.0);
        assert_eq!(trim.color, Some([255, 0, 0]));
        assert!(trim.equal_hor);
        assert!(trim.equal_ver);
    }

    #[test]
    fn trim_missing_threshold_is_rejected() {
        assert!(ProcessingOptions::parse(&["t:"]).is_err());
        assert!(ProcessingOptions::parse(&["t:1:2:3:4:5"]).is_err());
    }

    #[test]
    fn trim_negative_threshold_is_rejected() {
        assert!(ProcessingOptions::parse(&["t:-1"]).is_err());
    }

    #[test]
    fn parses_extend() {
        assert_eq!(ProcessingOptions::parse(&["ex:1"]).unwrap().extend, Some(true));
        assert_eq!(ProcessingOptions::parse(&["ex:0"]).unwrap().extend, Some(false));
    }

    #[test]
    fn extend_with_gravity_argument_is_rejected_not_silently_ignored() {
        // imgproxy accepts an optional `:gravity` second argument; this
        // crate doesn't implement gravity for extend (see the field's doc
        // comment), so a second argument must be a 400, not a silently
        // ignored no-op - matching #51's "parses but means something
        // different is worse than missing" guidance.
        assert!(ProcessingOptions::parse(&["ex:1:ce"]).is_err());
    }

    #[test]
    fn parses_padding_single_value_applies_to_all_sides() {
        let padding = ProcessingOptions::parse(&["pd:10"]).unwrap().padding.unwrap();
        assert_eq!(padding, Padding { top: 10, right: 10, bottom: 10, left: 10 });
    }

    #[test]
    fn parses_padding_two_values_vertical_then_horizontal() {
        let padding = ProcessingOptions::parse(&["pd:10:20"]).unwrap().padding.unwrap();
        assert_eq!(padding, Padding { top: 10, right: 20, bottom: 10, left: 20 });
    }

    #[test]
    fn parses_padding_three_values() {
        let padding = ProcessingOptions::parse(&["pd:10:20:30"]).unwrap().padding.unwrap();
        assert_eq!(padding, Padding { top: 10, right: 20, bottom: 30, left: 20 });
    }

    #[test]
    fn parses_padding_four_values_all_distinct() {
        let padding = ProcessingOptions::parse(&["pd:10:20:30:40"]).unwrap().padding.unwrap();
        assert_eq!(padding, Padding { top: 10, right: 20, bottom: 30, left: 40 });
    }

    #[test]
    fn padding_empty_positional_slot_falls_back_like_an_omitted_one() {
        // `pd:10::30` - right omitted (empty) falls back to top (10);
        // bottom explicit 30; left omitted entirely falls back to right's
        // resolved value (10).
        let padding = ProcessingOptions::parse(&["pd:10::30"]).unwrap().padding.unwrap();
        assert_eq!(padding, Padding { top: 10, right: 10, bottom: 30, left: 10 });
    }

    #[test]
    fn padding_requires_at_least_the_top_argument() {
        assert!(ProcessingOptions::parse(&["pd:"]).is_err());
    }

    #[test]
    fn parses_zoom_single_value_applies_to_both_axes() {
        let opts = ProcessingOptions::parse(&["z:2"]).unwrap();
        assert_eq!(opts.zoom_x, 2.0);
        assert_eq!(opts.zoom_y, 2.0);
    }

    #[test]
    fn parses_zoom_two_distinct_values() {
        let opts = ProcessingOptions::parse(&["z:2:3"]).unwrap();
        assert_eq!(opts.zoom_x, 2.0);
        assert_eq!(opts.zoom_y, 3.0);
    }

    #[test]
    fn zoom_zero_or_negative_is_rejected() {
        assert!(ProcessingOptions::parse(&["z:0"]).is_err());
        assert!(ProcessingOptions::parse(&["z:-1"]).is_err());
    }

    #[test]
    fn parses_dpr() {
        assert_eq!(ProcessingOptions::parse(&["dpr:2"]).unwrap().dpr, 2.0);
        assert_eq!(ProcessingOptions::parse(&["dpr:1.5"]).unwrap().dpr, 1.5);
    }

    #[test]
    fn dpr_zero_or_negative_is_rejected() {
        assert!(ProcessingOptions::parse(&["dpr:0"]).is_err());
        assert!(ProcessingOptions::parse(&["dpr:-2"]).is_err());
    }

    #[test]
    fn parses_min_width_and_min_height() {
        let opts = ProcessingOptions::parse(&["mw:100", "mh:200"]).unwrap();
        assert_eq!(opts.min_width, Some(100));
        assert_eq!(opts.min_height, Some(200));
    }

    #[test]
    fn min_width_zero_means_unset() {
        assert_eq!(ProcessingOptions::parse(&["mw:0"]).unwrap().min_width, None);
    }

    #[test]
    fn combines_every_51_option_at_once() {
        let opts = ProcessingOptions::parse(&[
            "rs:fill:300:300",
            "rot:90",
            "fl:1:1",
            "t:5:ffffff:1:1",
            "ex:1",
            "pd:1:2:3:4",
            "z:2",
            "dpr:2",
            "mw:50",
            "mh:60",
        ])
        .unwrap();

        assert_eq!(opts.rotate, 90);
        assert!(opts.flip_horizontal);
        assert!(opts.flip_vertical);
        let trim = opts.trim.unwrap();
        assert_eq!(trim.threshold, 5.0);
        assert_eq!(trim.color, Some([255, 255, 255]));
        assert!(trim.equal_hor);
        assert!(trim.equal_ver);
        assert_eq!(opts.extend, Some(true));
        assert_eq!(
            opts.padding,
            Some(Padding { top: 1, right: 2, bottom: 3, left: 4 })
        );
        assert_eq!(opts.zoom_x, 2.0);
        assert_eq!(opts.zoom_y, 2.0);
        assert_eq!(opts.dpr, 2.0);
        assert_eq!(opts.min_width, Some(50));
        assert_eq!(opts.min_height, Some(60));
    }

    // --- #51 grammar tests end ---
}
