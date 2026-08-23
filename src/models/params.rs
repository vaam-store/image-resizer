//! Hand-written request models (#53: replaces the OpenAPI-codegen'd
//! `gen_server::models` this crate used to depend on).
//!
//! [`ResizeQuery`] is built directly by [`crate::modules::url`]'s signed-URL
//! grammar parser instead of being converted (via `o2o`) from a generated
//! `ResizeQueryParams` - there is no generated struct left to convert from.

/// Output image format - the imgproxy-style path grammar's trailing
/// `.{extension}` (`crate::modules::url::source`), not a query parameter.
///
/// #49 adds three variants on top of the original `{Jpg, Png, Webp}`:
/// - `Avif` - encode *and* decode, both via `libavif`
///   (`src/services/image/avif_codec.rs`, #67/#68): AOM for encode
///   (replacing the pure-Rust `ravif`/`rav1e` encoder this crate shipped
///   before #68) and dav1d for decode (previously entirely unsupported -
///   an AVIF *source* URL failed outright). `image`'s own `avif`/
///   `avif-native` features are not used for either direction any more -
///   see `Cargo.toml`'s `image`/`libavif-sys` dependency comments.
/// - `Gif` - decode and encode, including multi-frame animation
///   (`ImageService::process_image_blocking_with_limits`,
///   `src/services/image/handler.rs`).
/// - `Auto` - not a real output format at all: the URL grammar's `.auto`
///   extension, resolved to a concrete format by `Accept`-driven content
///   negotiation (`crate::modules::negotiation`) at the HTTP edge
///   (`crate::modules::api::resize::handle`) before a `ResizeQuery` is ever
///   built. Every other layer - `CacheService::generate_key`,
///   `ImageService` - only ever sees a concrete format; `Auto` reaching
///   either is a bug in the negotiation call site, not a real request
///   shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Jpg,
    Png,
    Webp,
    Avif,
    Gif,
    Auto,
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ImageFormat::Jpg => "jpg",
            ImageFormat::Png => "png",
            ImageFormat::Webp => "webp",
            ImageFormat::Avif => "avif",
            ImageFormat::Gif => "gif",
            ImageFormat::Auto => "auto",
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
            "avif" => Ok(ImageFormat::Avif),
            "gif" => Ok(ImageFormat::Gif),
            "auto" => Ok(ImageFormat::Auto),
            other => Err(format!(
                "unsupported image format {other:?} (expected jpg, jpeg, png, webp, avif, gif or auto)"
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

/// imgproxy's `g:{type}:{x_offset}:{y_offset}` gravity option (#50) -
/// <https://docs.imgproxy.net/usage/processing#gravity> - controlling which
/// part of the image survives a `fill`-type crop (the resize pipeline's
/// `ResizeType::Fill`/`Auto`-as-fill branch, `src/services/image/handler.rs`)
/// and, separately, anchoring an explicit [`Crop`] region.
///
/// Deliberately scoped down from imgproxy's own grammar in two ways, both
/// documented rather than silently approximated:
/// - No `x_offset`/`y_offset` nudge for the directional/corner/center
///   variants - imgproxy lets every gravity type take an extra offset pair
///   to nudge the anchor point; this crate only exposes that for
///   [`Gravity::FocusPoint`] (where the offset *is* the point), keeping the
///   directional variants at their plain anchor. A future URL-grammar change
///   can add the nudge without touching this enum's shape.
/// - No smart (`sm`) or object-detection (`obj`/`objw`, imgproxy Pro-only)
///   gravity. A real saliency implementation is out of scope for this change
///   (see the #50 issue body) - the URL parser
///   (`src/modules/url/options.rs::parse_gravity`) rejects `sm`/`obj`/`objw`
///   tokens with the same 400-shaped `InvalidOptionValue` error an unknown
///   token gets, rather than silently aliasing `sm` to `Center` and shipping
///   a fake "smart" gravity.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Gravity {
    #[default]
    Center,
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
    /// imgproxy's `fp:{x}:{y}` gravity - `x`/`y` are fractions in `[0, 1]` of
    /// the container's width/height respectively, marking the point the crop
    /// box should be centred on (clamped so the box stays fully inside the
    /// container - see `ImageService::gravity_anchor`,
    /// `src/services/image/handler.rs`).
    FocusPoint {
        x: f64,
        y: f64,
    },
}

impl std::fmt::Display for Gravity {
    /// Canonical short form, matching imgproxy's own `g:` token vocabulary -
    /// used both for round-tripping and as the string hashed into the cache
    /// key (`CacheService::generate_key`, `src/services/cache/handler.rs`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Gravity::Center => f.write_str("ce"),
            Gravity::North => f.write_str("no"),
            Gravity::South => f.write_str("so"),
            Gravity::East => f.write_str("ea"),
            Gravity::West => f.write_str("we"),
            Gravity::NorthEast => f.write_str("noea"),
            Gravity::NorthWest => f.write_str("nowe"),
            Gravity::SouthEast => f.write_str("soea"),
            Gravity::SouthWest => f.write_str("sowe"),
            Gravity::FocusPoint { x, y } => write!(f, "fp:{x}:{y}"),
        }
    }
}

/// One axis of an explicit `c:{width}:{height}:{gravity}` crop region (#50) -
/// <https://docs.imgproxy.net/usage/processing#crop>. Mirrors imgproxy's own
/// three-way convention for each of `width`/`height`: `0` means "use the
/// full source dimension on this axis" (no crop on that axis alone), a value
/// `>= 1` is an absolute pixel size, and a value in `(0, 1)` is a fraction of
/// the source dimension on that axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropDimension {
    Full,
    Absolute(u32),
    Relative(f64),
}

impl std::fmt::Display for CropDimension {
    /// Canonical string form hashed into the cache key
    /// (`CacheService::generate_key`) - `Relative` renders its exact `f64`
    /// bit pattern via `{}` (not lossy-rounded), so two distinct relative
    /// fractions can never collide onto the same cache key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CropDimension::Full => f.write_str("full"),
            CropDimension::Absolute(v) => write!(f, "abs:{v}"),
            CropDimension::Relative(v) => write!(f, "rel:{v}"),
        }
    }
}

/// imgproxy's explicit `crop`/`c:{width}:{height}:{gravity}` processing
/// option (#50) - "Defines an area of the image to be processed (crop
/// before resize)." Applied to the decoded source image, before any
/// fit/fill/force/auto resize math runs, by
/// `ImageService::process_image_blocking_with_limits`
/// (`src/services/image/handler.rs`) - every downstream dimension
/// (upscale guard, `resize_dimensions`, fill/auto orientation comparison)
/// is computed against the *cropped* image, matching imgproxy's own
/// "crop before resize" ordering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub width: CropDimension,
    pub height: CropDimension,
    /// Anchors the crop region within the source image. Defaults to
    /// `Gravity::default()` (`Center`) when the URL's `c:` segment omits a
    /// gravity token.
    pub gravity: Gravity,
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

    // --- #76 additions start: progressive JPEG, chroma subsampling, and
    // max_bytes. Kept as one contiguous block, mirroring the matching block
    // in `src/services/cache/handler.rs`'s `generate_key` and
    // `src/modules/url/options.rs`'s `ProcessingOptions`, so the three stay
    // easy to cross-reference.
    /// Encode JPEG output progressively instead of baseline sequential
    /// (imgproxy's `jpeg_options`/`jpgo:{progressive}:...` option's first
    /// slot, <https://docs.imgproxy.net/usage/processing#jpeg-options> -
    /// only the `progressive`/`no_subsample` slots are implemented here,
    /// see [`crate::modules::url::options::ProcessingOptions::jpeg_progressive`]
    /// for why). `None` means "use this deployment's configured default"
    /// (`PerformanceConfig::jpeg_progressive_default`, `JPEG_PROGRESSIVE`
    /// env var - imgproxy's `IMGPROXY_JPEG_PROGRESSIVE`), resolved in
    /// `ImageService::encode_single_image`. Meaningless for non-JPEG
    /// output, same as every other format-specific option on this struct.
    pub jpeg_progressive: Option<bool>,

    /// Encode JPEG chroma at full resolution (4:4:4) instead of this
    /// crate's default 4:2:2 (`jpgo:{progressive}:{no_subsample}`'s second
    /// slot; imgproxy's `IMGPROXY_JPEG_NO_SUBSAMPLING`). `None` means "use
    /// this deployment's configured default"
    /// (`PerformanceConfig::jpeg_no_subsampling_default`,
    /// `JPEG_NO_SUBSAMPLING` env var), same resolution point as
    /// `jpeg_progressive` above. Meaningless for non-JPEG output.
    pub jpeg_no_subsampling: Option<bool>,

    /// Maximum encoded output size in bytes (imgproxy's `max_bytes`/
    /// `mb:{bytes}` option). `None`/`Some(0)`-normalised-to-`None` (`mb`'s
    /// URL parser follows the same "0 means unset" convention as
    /// `width`/`height`) means no budget - the encoder just uses whatever
    /// quality was otherwise resolved. When set, `ImageService` iteratively
    /// lowers quality (bounded search - see
    /// `ImageService::encode_with_max_bytes`) until the output fits, or the
    /// search budget is exhausted, matching imgproxy's own documented
    /// best-effort behaviour ("automatically degrades the quality... until
    /// the image size is under the specified amount of bytes"). Only
    /// applied to JPEG output - see `encode_single_image`'s JPEG branch for
    /// the cost reasoning (measured against `benches/encode.rs`) behind
    /// excluding every other format.
    pub max_bytes: Option<u64>,
    // --- #76 additions end.

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

    /// #5 - imgproxy's `strip_metadata`/`sm:{0|1|true|false}` processing
    /// option (<https://docs.imgproxy.net/usage/processing#strip-metadata>):
    /// whether to drop the source's EXIF metadata (GPS coordinates, camera
    /// make/model, timestamps, ...) instead of forwarding it to the output.
    ///
    /// Defaults to `true` (strip) - matching imgproxy's own documented
    /// default (`IMGPROXY_STRIP_METADATA: true`) and, unlike every other
    /// boolean option on this struct besides `autorotate`, chosen
    /// deliberately rather than inherited from `bool`'s zero value: this
    /// service accepts arbitrary user-uploaded sources, and forwarding GPS
    /// coordinates or other EXIF fields by default is a real privacy
    /// footgun, not a neutral one. A caller who wants metadata preserved
    /// must opt in with `sm:0`/`sm:false`.
    ///
    /// This governs EXIF specifically - the fields a phone/camera embeds
    /// about *when, where and with what* a photo was taken. It deliberately
    /// does **not** cover:
    /// - The embedded ICC colour profile (`icc_profile` threaded through
    ///   `ImageService::encode_single_image`, `src/services/image/handler.rs`,
    ///   #33) - that's colour-correctness data, not privacy-sensitive
    ///   metadata, and stays unconditionally forwarded exactly as it did
    ///   before this option existed. imgproxy draws the same line, just with
    ///   its own separate `strip_color_profile`/`scp` option (default
    ///   `true`, and unlike a bare "stop forwarding" toggle, imgproxy's
    ///   version *converts* the profile to sRGB rather than merely dropping
    ///   it - color-managing every image on the way in would need a real
    ///   colour-management dependency this crate doesn't have and can't add
    ///   here, so that option is not implemented at all rather than shipped
    ///   as a lesser, non-equivalent stand-in).
    /// - imgproxy's `keep_copyright`/`kcr` (default `true`: even when
    ///   `strip_metadata` is on, imgproxy keeps just the copyright field) is
    ///   also not implemented - doing that faithfully means parsing the raw
    ///   EXIF/IPTC/XMP blob well enough to extract one field, which needs an
    ///   actual metadata-parsing dependency this crate doesn't have either.
    ///   `strip_metadata` here is all-or-nothing.
    ///
    /// EXIF `Orientation` is a special case regardless of this flag's value:
    /// `autorotate` (above) applies it to the *pixels* and the corrected
    /// image never carries a stale rotation instruction forward - see
    /// `ImageService::neutralize_exif_orientation`'s doc comment
    /// (`src/services/image/handler.rs`) for how "keep metadata" avoids
    /// telling an EXIF-aware viewer to rotate an already-rotated image a
    /// second time.
    ///
    /// Per-format support for actually *writing* kept EXIF back out is not
    /// uniform - see `ImageService::encode_single_image`'s own doc comment
    /// for the real matrix (JPEG via a raw `mozjpeg` APP1 marker, PNG and
    /// AVIF via `image`'s `ImageEncoder::set_exif_metadata`, WebP and GIF
    /// unsupported by their respective encoders regardless of this flag).
    pub strip_metadata: bool,

    /// Explicit `crop`/`c:` processing option (#50) - `None` means no
    /// explicit crop, the pre-#50 behaviour. See [`Crop`].
    pub crop: Option<Crop>,

    /// `gravity`/`gr:` processing option (#50) - see [`Gravity`]. Always
    /// present (not `Option<Gravity>`, mirroring `resize_type`/`enlarge`
    /// above): it has a meaningful default (`Center`, imgproxy's own
    /// default) even when the URL never names a `gr:` segment, and every
    /// `Fill`-type crop needs *some* gravity to anchor on regardless.
    /// Consumed two ways in `ImageService`
    /// (`src/services/image/handler.rs`): as the anchor for the
    /// `ResizeType::Fill`/`Auto`-as-fill cover-crop, and as the default
    /// anchor for an explicit [`Crop`] whose own `gravity` wasn't set in
    /// the URL (`ParsedRequest::into_resize_query`,
    /// `src/modules/url/mod.rs`, wires the same top-level `gr:` value in
    /// unless the `c:` segment named its own).
    pub gravity: Gravity,

    // --- #51: geometry operations (rotate, flip, trim, extend, padding,
    // zoom, dpr, min-width/min-height). Kept as one contiguous, clearly
    // commented block so the integration diff against the sibling #49/#50/
    // #52 issues stays legible - see `src/services/cache/handler.rs` for
    // the matching contiguous block added to `generate_key`'s hashed
    // stream (CACHE_KEY_VERSION bumped to v8 once, covering all four
    // issues - see that constant's own doc comment).
    //
    /// imgproxy's `rotate`/`rot:{angle}` processing option. Always
    /// normalised to one of `0`/`90`/`180`/`270` by the URL parser
    /// (`crate::modules::url::options`) before it ever reaches here -
    /// `ImageService` can therefore match on exactly those four values.
    /// Applied *after* resize, matching imgproxy's own pipeline order
    /// (`processing/processing.go`'s `mainPipeline`: `scale` then
    /// `rotateAndFlip`) - see `ImageService::effective_resize_box` for how
    /// a 90/270 rotation swaps the width/height box fed into the resize
    /// step itself, so the *final* (post-rotation) image still matches the
    /// requested `width`/`height`.
    pub rotate: i32,

    /// imgproxy's `flip`/`fl:{horizontal}:{vertical}` processing option -
    /// mirrors the image along the given axis/axes. Applied together with
    /// `rotate`, immediately after it (imgproxy's `rotateAndFlip` is a
    /// single pipeline step covering both).
    pub flip_horizontal: bool,
    pub flip_vertical: bool,

    /// imgproxy's `trim`/`t:{threshold}:{color}:{equal_hor}:{equal_ver}`
    /// processing option - removes uniform-colour borders. `None` means no
    /// trim requested. Always the *first* geometry operation applied (right
    /// after decode, before dimensions are read for the resize/enlarge-guard
    /// math) - matches imgproxy's own pipeline, where `trim` is the very
    /// first step, ahead of `scaleOnLoad`/`crop`/`scale`.
    pub trim: Option<TrimOptions>,

    /// imgproxy's `extend`/`ex:{enabled}` processing option - pads the
    /// image up to the requested `width`x`height` (centred) if it would
    /// otherwise come out smaller, instead of never doing so (`enlarge`
    /// stays about upscaling actual pixels; `extend` is the
    /// background-fill alternative). This crate only accepts the boolean
    /// `enabled` argument - imgproxy's optional second `:gravity` argument
    /// is rejected at parse time (400) rather than silently ignored, since
    /// only centre-gravity extend is implemented here (gravity/crop as a
    /// whole is out of #51's scope). A no-op unless both `width` and
    /// `height` are set (there is no other well-defined target canvas to
    /// extend into).
    pub extend: bool,

    /// imgproxy's `padding`/`pd:{top}:{right}:{bottom}:{left}` processing
    /// option (CSS-style shorthand - see [`Padding`]). Applied after
    /// `extend`, matching imgproxy's own pipeline order, and always
    /// enlarges the final canvas via `background` fill.
    pub padding: Option<Padding>,

    /// imgproxy's `zoom`/`z:{zoom_x}:{zoom_y}` processing option - a
    /// multiplier (default `1.0`) applied to an *explicitly requested*
    /// `width`/`height` before resize. Unlike imgproxy, this crate only
    /// scales an axis that already has an explicit `width`/`height` set -
    /// see `ImageService::effective_resize_box`'s doc comment for why this
    /// is a deliberate narrowing rather than full parity.
    pub zoom_x: f32,
    pub zoom_y: f32,

    /// imgproxy's `dpr:{value}` processing option - a multiplier (default
    /// `1.0`) applied to an explicitly requested `width`/`height`, same as
    /// `zoom` (and combined with it: both multiply the same axis). This is
    /// how responsive images are served in practice (`w:300/dpr:2` for a
    /// 300 CSS-px slot on a 2x-density screen) - see
    /// `ImageService::effective_resize_box` for exactly how it interacts
    /// with the #36 enlarge guard and the #26 output-dimension cap.
    pub dpr: f32,

    /// imgproxy's `min-width`/`mw:{width}` and `min-height`/`mh:{height}`
    /// processing options - a floor on the *resulting* image size that,
    /// matching imgproxy's own behaviour, is **not** gated by `enlarge`: it
    /// can force upscaling past the source even with `enlarge=false`. `0`
    /// (or absent) means "no floor", mirroring the `width`/`height`
    /// `0`-means-unset convention already used elsewhere in this grammar.
    pub min_width: Option<u32>,
    pub min_height: Option<u32>,
    // --- #51 additions end.

    /// Watermark request (imgproxy's `watermark`/`wm:` option and its
    /// modifiers, #52). `None` means no watermark is composited. See
    /// [`WatermarkQuery`] for the individual fields and
    /// `ImageService::process_image`/`ImageService::apply_watermark`
    /// (`src/services/image/handler.rs`) for how it's fetched and
    /// composited - *before* the #34/#60 alpha-flatten/normalise stage, so
    /// a watermark's own alpha contributes to what gets flattened/
    /// normalised rather than slipping past it.
    pub watermark: Option<WatermarkQuery>,
}

/// imgproxy's `padding` processing option, parsed CSS-shorthand style: a
/// missing `right` copies `top`, a missing `bottom` copies `top`, and a
/// missing `left` copies `right` (in that order) - reproducing imgproxy's
/// exact cascading-fallback parse (`options/parser/apply.go`'s
/// `applyPaddingOption`), which is what makes `pd:10` mean "10 on every
/// side", `pd:10:20` mean "10 top/bottom, 20 left/right", etc., exactly
/// like CSS's own shorthand - even though the underlying parse is really
/// positional-with-fallback, not a value-count switch.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Padding {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

/// imgproxy's `trim` processing option's parsed arguments. See
/// [`ResizeQuery::trim`] and `ImageService::apply_trim`
/// (`src/services/image/handler.rs`) for how these are consumed.
#[derive(Clone, PartialEq, Debug)]
pub struct TrimOptions {
    /// Colour-similarity tolerance. Compared as the maximum per-channel
    /// (Chebyshev) distance from the target colour - a simpler, more
    /// predictable metric than imgproxy's own perceptual "smart" trim, and
    /// one whose result is easy to reason about/test pixel-exactly.
    pub threshold: f32,
    /// Explicit background colour to trim. `None` means "auto-detect from
    /// the image's top-left corner pixel" - a deliberately simpler stand-in
    /// for imgproxy's own multi-corner "smart" detection.
    pub color: Option<[u8; 3]>,
    /// When set, the left and right trim amounts are equalised (both take
    /// the smaller of the two), so only a symmetric amount is cut.
    pub equal_hor: bool,
    /// Same as `equal_hor`, for the top/bottom trim amounts.
    pub equal_ver: bool,
}

impl Default for ResizeQuery {
    /// Every #51 field defaults to "no effect": `rotate = 0`,
    /// `flip_horizontal`/`flip_vertical` = `false`, `trim`/`padding` =
    /// `None`, `extend = false`, `zoom_x`/`zoom_y`/`dpr = 1.0` (a `1.0`
    /// multiplier is the neutral element, unlike the other fields' `0`/
    /// `None`/`false`), `min_width`/`min_height = None`. Exists mainly so
    /// the many `ResizeQuery { .. , ..Default::default() }` test/bench
    /// call sites across the crate don't all need to be taught about every
    /// new field this issue adds. `autorotate` (#33) defaults to `true` -
    /// not `bool`'s own zero value - matching its own field doc comment's
    /// "autorotate stays on unless a caller opts out" default; `crop`/
    /// `gravity` (#50) default to "no explicit crop" / `Gravity::default()`
    /// (`Center`), same as `ProcessingOptions`'s own hand-written `Default`
    /// (`src/modules/url/options.rs`). `strip_metadata` (#5) also defaults
    /// to `true` - not `bool`'s zero value - same reasoning as `autorotate`:
    /// see its own field doc comment.
    fn default() -> Self {
        Self {
            url: String::new(),
            width: None,
            height: None,
            resize_type: ResizeType::default(),
            format: ImageFormat::default(),
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            jpeg_progressive: None,
            jpeg_no_subsampling: None,
            max_bytes: None,
            background: None,
            autorotate: true,
            strip_metadata: true,
            crop: None,
            gravity: Gravity::default(),
            rotate: 0,
            flip_horizontal: false,
            flip_vertical: false,
            trim: None,
            extend: false,
            padding: None,
            zoom_x: 1.0,
            zoom_y: 1.0,
            dpr: 1.0,
            min_width: None,
            min_height: None,
            watermark: None,
        }
    }
}

/// Placement anchor for a composited watermark (#52), mirroring imgproxy's
/// `watermark`/`wm:` position codes
/// (<https://docs.imgproxy.net/usage/processing#watermark>). Tiling modes
/// (`re` repeat, `ch` chessboard) are not implemented - every other
/// documented position is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WatermarkPosition {
    #[default]
    Center,
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

/// A resolved watermark request (#52), built by
/// [`crate::modules::url::ParsedRequest::into_resize_query`] once the
/// `wm:`/`watermark` option is present - see
/// [`crate::modules::url::options::ProcessingOptions`] for the individual
/// `wm`/`wmu`/`wms`/`wmr`/`wmsh` fields this is assembled from.
///
/// `url` (from `wmu:{base64url}`) is `None` when the request relies on this
/// deployment's configured default watermark (`WATERMARK_URL`,
/// `PerformanceConfig::watermark_url`) instead of naming its own - see
/// `ImageService::process_image` (`src/services/image/handler.rs`) for how
/// the two are resolved into a single URL to fetch. Whichever URL is used,
/// it goes through the exact same SSRF guard
/// (`services::image::source_guard`, #21/#57) as the main source image - a
/// watermark URL is just as much an attacker-reachable fetch target as the
/// source URL when it comes from the request.
#[derive(Debug, Clone, PartialEq)]
pub struct WatermarkQuery {
    /// Final opacity applied to the watermark (imgproxy: `base_opacity *
    /// opacity`; this crate has no separate configured base opacity, so
    /// this is used directly). Clamped to `[0.0, 1.0]` at composite time.
    pub opacity: f32,
    pub position: WatermarkPosition,
    /// Horizontal offset from `position`'s anchor. A magnitude `>= 1.0` is
    /// absolute pixels; anything smaller is a fraction of the base image's
    /// width - imgproxy's own `x_offset` convention.
    pub x_offset: f32,
    /// Vertical offset from `position`'s anchor, same absolute-vs-relative
    /// convention as `x_offset` but against the base image's height.
    pub y_offset: f32,
    /// Watermark size relative to the base image size (both dimensions
    /// scaled by this factor, then fit preserving the watermark's own
    /// aspect ratio). `0.0` (the default, `wm:`'s scale slot left blank)
    /// means "no scaling" - use `size` if set, otherwise the watermark's
    /// natural resolution.
    pub scale: f32,
    /// Per-request watermark source (`wmu:{base64url}`, imgproxy Pro's
    /// arbitrary-URL watermark). `None` falls back to this deployment's
    /// configured default.
    pub url: Option<String>,
    /// Explicit watermark dimensions (`wms:{width}:{height}`, imgproxy
    /// Pro). Either dimension may be `0`, meaning "derive from the other
    /// via the watermark's own aspect ratio" - imgproxy's own convention
    /// for this option. Always resized with `fit` semantics (never
    /// stretched), matching imgproxy's documented behaviour.
    pub size: Option<(u32, u32)>,
    /// Clockwise rotation in degrees (`wmr:{angle}`, imgproxy Pro). `0.0`
    /// (the default) applies no rotation.
    pub rotate: f32,
    /// Gaussian blur sigma for a drop-shadow silhouette drawn behind the
    /// watermark (`wmsh:{sigma}`, imgproxy Pro). `None`/`Some(0.0)` draws
    /// no shadow.
    pub shadow: Option<f32>,
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
            (ImageFormat::Avif, "avif"),
            (ImageFormat::Gif, "gif"),
            (ImageFormat::Auto, "auto"),
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
        assert!("bmp".parse::<ImageFormat>().is_err());
        assert!("heic".parse::<ImageFormat>().is_err());
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

    /// #50: `Gravity`'s `Display` impl is hashed into the cache key
    /// (`CacheService::generate_key`, `src/services/cache/handler.rs`) -
    /// pinning its exact short-form output here means a future refactor of
    /// this `fmt` impl that accidentally changes the string (without also
    /// bumping `CACHE_KEY_VERSION`) is caught here instead of silently
    /// invalidating/colliding cache entries.
    #[test]
    fn gravity_display_matches_imgproxy_short_form_tokens() {
        for (gravity, text) in [
            (Gravity::Center, "ce"),
            (Gravity::North, "no"),
            (Gravity::South, "so"),
            (Gravity::East, "ea"),
            (Gravity::West, "we"),
            (Gravity::NorthEast, "noea"),
            (Gravity::NorthWest, "nowe"),
            (Gravity::SouthEast, "soea"),
            (Gravity::SouthWest, "sowe"),
        ] {
            assert_eq!(gravity.to_string(), text);
        }
        assert_eq!(
            Gravity::FocusPoint { x: 0.25, y: 0.75 }.to_string(),
            "fp:0.25:0.75"
        );
    }

    #[test]
    fn gravity_default_is_center() {
        assert_eq!(Gravity::default(), Gravity::Center);
    }

    #[test]
    fn crop_dimension_display_distinguishes_every_variant() {
        assert_eq!(CropDimension::Full.to_string(), "full");
        assert_eq!(CropDimension::Absolute(300).to_string(), "abs:300");
        assert_eq!(CropDimension::Relative(0.5).to_string(), "rel:0.5");
        // Absolute and Relative must never render to the same string for
        // any pair of values a caller could plausibly send, since that
        // string is what the cache key hashes.
        assert_ne!(
            CropDimension::Absolute(1).to_string(),
            CropDimension::Relative(1.0).to_string()
        );
    }
}
