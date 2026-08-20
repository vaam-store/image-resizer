use crate::models::params::ResizeQuery;
use derive_builder::Builder;
use sha2::{Digest, Sha256};

/// Bumped whenever the byte layout fed into [`CacheService::generate_key`]
/// changes, so that stale entries hashed under an older, ambiguous scheme
/// are invalidated instead of silently colliding with (or being served for)
/// keys produced by the new scheme.
///
/// v1 was the original, un-prefixed, delimiter-free concatenation (see
/// GH-24). v2 introduces length-prefixed fields. v3 adds `enlarge` (#36) to
/// the hashed byte stream - without the bump, every key computed under v2
/// (which never hashed `enlarge` at all) would keep colliding across
/// `enlarge=true`/`enlarge=false` requests for the same other parameters
/// even after the field started being read from `ResizeQuery`, since bytes
/// already written to storage under those v2 keys wouldn't be reinterpreted
/// just because the code changed. v4 adds `resize_type` (#59): before #59,
/// every `width`+`height` request was resized identically (always `fill`)
/// regardless of the `rs:{type}:...` token in the URL, so `resize_type`
/// didn't need to be part of the key - two requests differing only in that
/// token produced byte-identical output. Now that `fit`/`fill`/`force`/
/// `auto` each produce different output for the same width/height, a v3 key
/// (which never hashed the type) would serve a `fit` request the `fill`
/// output cached under the same width/height/etc, or vice versa - exactly
/// the kind of stale, wrong-shape response #59 exists to eliminate. Every
/// key computed under v3 must therefore be invalidated by this bump, same
/// as the v2 -> v3 transition for `enlarge`. v5 adds `background` (#34/#60):
/// before this, the resize pipeline never flattened alpha or normalised
/// transparent pixels at all, so `background` had no effect on output
/// bytes. Now that it changes what gets encoded - both the flatten colour
/// for alpha->no-alpha conversions and the fill colour for fully-transparent
/// pixels when alpha is kept - two requests differing only in `background`
/// must not collide onto a v4 key that never hashed it. v6 adds `quality`,
/// `jpeg_quality`, `webp_quality` and `webp_lossless` (#35): `quality` was
/// already parsed into `ResizeQuery` (from the URL grammar's `q:{0-100}`)
/// before this change, but was never fed into `generate_key` at all - a
/// pure oversight, not a deliberate "doesn't affect output" omission like
/// `resize_type` was pre-#59, since quality has always affected encoded
/// bytes for whatever encoder actually used it. Now that the encoders in
/// `src/services/image/handler.rs` honour these fields, two requests
/// differing only in quality/format-quality/losslessness must not collide
/// onto a v5 key that never hashed any of them.
const CACHE_KEY_VERSION: u8 = 6;

#[derive(Clone, Builder)]
pub struct CacheService {
    minio_sub_path: String,
}

impl CacheService {
    /// Generate a deterministic cache key based on resize parameters.
    ///
    /// # Collision resistance
    ///
    /// Fields are **length-prefixed** rather than separated by a delimiter
    /// byte: `url` is fully attacker-controlled, so any fixed delimiter
    /// (e.g. a null byte or `|`) could itself appear inside the URL and be
    /// used to forge a byte stream that collides with a different,
    /// legitimate set of parameters. A 4-byte big-endian length prefix
    /// before each field's bytes makes the boundary between fields
    /// unambiguous regardless of what bytes the field itself contains, so
    /// `hasher.update(len_be_bytes); hasher.update(field_bytes)` for every
    /// field yields an injective mapping from `(field_1, .., field_n)` to
    /// the hashed byte stream.
    ///
    /// A single version byte is written first so that entries cached under
    /// any earlier, ambiguous scheme are naturally invalidated (they hash
    /// to different keys) rather than being reused under the new scheme.
    pub fn generate_key(&self, params: &ResizeQuery) -> String {
        let mut hasher = Sha256::new();
        hasher.update([CACHE_KEY_VERSION]);

        Self::update_field(&mut hasher, params.url.as_bytes());

        match params.width {
            Some(width) => Self::update_field(&mut hasher, width.to_string().as_bytes()),
            None => Self::update_field(&mut hasher, b"None"),
        }

        match params.height {
            Some(height) => Self::update_field(&mut hasher, height.to_string().as_bytes()),
            None => Self::update_field(&mut hasher, b"None"),
        }

        // `resize_type` (#59) changes the resize pipeline's output whenever
        // both `width` and `height` are present - `fit`/`fill`/`force`/
        // `auto` each produce visibly different bytes for the same box - so
        // it must be part of the key like every other output-affecting
        // field (see the v4 `CACHE_KEY_VERSION` note above). Always-present
        // (not `Option<ResizeType>`), so no "None" bucket is needed here.
        Self::update_field(&mut hasher, params.resize_type.to_string().as_bytes());

        Self::update_field(
            &mut hasher,
            params.format.to_string().to_lowercase().as_bytes(),
        );

        Self::update_field(
            &mut hasher,
            Self::canonical_blur_sigma(params.blur_sigma).as_bytes(),
        );

        match params.grayscale {
            Some(grayscale) => Self::update_field(&mut hasher, grayscale.to_string().as_bytes()),
            None => Self::update_field(&mut hasher, b"None"),
        }

        // `enlarge` (#36) changes the resize pipeline's output (whether
        // upscaling past the source resolution is permitted), so it must be
        // part of the key like every other field that affects output bytes
        // - otherwise a request with `enlarge=true` could be served a
        // cached response produced with `enlarge=false` (or vice versa).
        // Always-present (not `Option<bool>`, so no "None" bucket needed
        // here unlike `grayscale`/`blur_sigma`).
        Self::update_field(&mut hasher, params.enlarge.to_string().as_bytes());

        // `quality`/`jpeg_quality`/`webp_quality`/`webp_lossless` (#35, v6)
        // change the encoder's output bytes directly - see the v6
        // `CACHE_KEY_VERSION` note above for why these were missing before.
        match params.quality {
            Some(quality) => Self::update_field(&mut hasher, &[quality]),
            None => Self::update_field(&mut hasher, b"None"),
        }

        match params.jpeg_quality {
            Some(quality) => Self::update_field(&mut hasher, &[quality]),
            None => Self::update_field(&mut hasher, b"None"),
        }

        match params.webp_quality {
            Some(quality) => Self::update_field(&mut hasher, &[quality]),
            None => Self::update_field(&mut hasher, b"None"),
        }

        match params.webp_lossless {
            Some(lossless) => Self::update_field(&mut hasher, lossless.to_string().as_bytes()),
            None => Self::update_field(&mut hasher, b"None"),
        }

        // `background` (#34/#60, v5) changes the resize pipeline's output
        // bytes wherever alpha is flattened or transparent pixels are
        // normalised, so it must be part of the key like every other
        // output-affecting field - see the v5 `CACHE_KEY_VERSION` note
        // above.
        match params.background {
            Some([r, g, b]) => Self::update_field(&mut hasher, &[r, g, b]),
            None => Self::update_field(&mut hasher, b"None"),
        }

        let result = hasher.finalize();
        format!("{:}{:x}.{}", self.minio_sub_path, result, params.format)
    }

    /// Feed one field into the hasher, prefixed with its length as a fixed-width
    /// 4-byte big-endian integer. See [`generate_key`](Self::generate_key) for why
    /// a length prefix is used instead of a delimiter.
    fn update_field(hasher: &mut Sha256, bytes: &[u8]) {
        hasher.update((bytes.len() as u32).to_be_bytes());
        hasher.update(bytes);
    }

    /// Canonicalise `blur_sigma` so that every value the resize pipeline
    /// treats as "no blur" hashes identically, instead of each producing its
    /// own cache entry.
    ///
    /// `image/handler.rs` only applies blur `if sigma > 0.0`. Mirroring that
    /// exact predicate here (rather than re-deriving an equivalent one) keeps
    /// the two in lockstep and, as a side effect, handles NaN for free: IEEE
    /// 754 defines every comparison against NaN (including `>`) as `false`,
    /// so `Some(NaN)` falls into the same "None" bucket as `None`, `0.0`,
    /// `-0.0`, and any negative value without a separate `is_nan` check.
    fn canonical_blur_sigma(sigma: Option<f32>) -> String {
        match sigma {
            Some(sigma) if sigma > 0.0 => sigma.to_string(),
            _ => "None".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // #53: `gen_server` (OpenAPI codegen) was deleted; `ImageFormat` is now
    // hand-written in `src/models/params.rs`. Mechanical import change
    // only - no logic here changed.
    use crate::models::params::{ImageFormat, ResizeType};
    use std::collections::HashSet;

    fn cache_service() -> CacheService {
        CacheServiceBuilder::default()
            .minio_sub_path("sub/".to_string())
            .build()
            .expect("build CacheService")
    }

    fn params(
        url: &str,
        width: Option<u32>,
        height: Option<u32>,
        format: ImageFormat,
        blur_sigma: Option<f32>,
        grayscale: Option<bool>,
    ) -> ResizeQuery {
        params_with_enlarge(url, width, height, format, blur_sigma, grayscale, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn params_with_enlarge(
        url: &str,
        width: Option<u32>,
        height: Option<u32>,
        format: ImageFormat,
        blur_sigma: Option<f32>,
        grayscale: Option<bool>,
        enlarge: bool,
    ) -> ResizeQuery {
        ResizeQuery {
            url: url.to_string(),
            width,
            height,
            resize_type: ResizeType::Fit,
            format,
            blur_sigma,
            grayscale,
            enlarge,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background: None,
        }
    }

    /// GH-24, collision 1: with no delimiters, `w=1, h=23` and `w=12, h=3`
    /// both flattened to the digit stream "123" under the old scheme.
    #[test]
    fn regression_width_height_boundary_collision_now_differs() {
        let cache = cache_service();

        let a = params(
            "https://ex.com/a.jpg",
            Some(1),
            Some(23),
            ImageFormat::Jpg,
            None,
            None,
        );
        let b = params(
            "https://ex.com/a.jpg",
            Some(12),
            Some(3),
            ImageFormat::Jpg,
            None,
            None,
        );

        assert_ne!(cache.generate_key(&a), cache.generate_key(&b));
    }

    /// GH-24, collision 2: the attacker-controlled URL "absorbs" a digit
    /// from the width field, so `url=".../a.jpg1", w=2` and
    /// `url=".../a.jpg", w=12` both flattened identically under the old
    /// scheme. This is the security-relevant case: an attacker who controls
    /// the URL could pick one that lands on another request's cache key.
    #[test]
    fn regression_url_absorbs_width_digit_now_differs() {
        let cache = cache_service();

        let attacker = params(
            "https://ex.com/a.jpg1",
            Some(2),
            Some(3),
            ImageFormat::Jpg,
            None,
            None,
        );
        let victim = params(
            "https://ex.com/a.jpg",
            Some(12),
            Some(3),
            ImageFormat::Jpg,
            None,
            None,
        );

        assert_ne!(cache.generate_key(&attacker), cache.generate_key(&victim));
    }

    /// `blur_sigma` values that the resize pipeline (`image/handler.rs`,
    /// `if sigma > 0.0`) all treat as "no blur" must canonicalise to the
    /// same cache key, instead of each wastefully producing its own entry.
    #[test]
    fn blur_sigma_inactive_values_collapse_to_one_key() {
        let cache = cache_service();

        let base = |sigma: Option<f32>| {
            params(
                "https://ex.com/a.jpg",
                Some(100),
                Some(100),
                ImageFormat::Png,
                sigma,
                Some(false),
            )
        };

        let none_key = cache.generate_key(&base(None));
        let inactive_values = [
            Some(0.0_f32),
            Some(-0.0_f32),
            Some(-1.0_f32),
            Some(-100.5_f32),
            Some(f32::NAN),
            Some(-f32::NAN),
            Some(f32::NEG_INFINITY),
        ];

        for value in inactive_values {
            assert_eq!(
                cache.generate_key(&base(value)),
                none_key,
                "expected {value:?} to canonicalise to the same key as None"
            );
        }
    }

    /// A positive `blur_sigma`, which the resize pipeline *does* apply, must
    /// remain distinct from the "no blur" bucket and from other active
    /// values.
    #[test]
    fn blur_sigma_active_values_stay_distinct() {
        let cache = cache_service();

        let base = |sigma: Option<f32>| {
            params(
                "https://ex.com/a.jpg",
                Some(100),
                Some(100),
                ImageFormat::Png,
                sigma,
                Some(false),
            )
        };

        let none_key = cache.generate_key(&base(None));
        let key_1_5 = cache.generate_key(&base(Some(1.5)));
        let key_2_0 = cache.generate_key(&base(Some(2.0)));

        assert_ne!(key_1_5, none_key);
        assert_ne!(key_2_0, none_key);
        assert_ne!(key_1_5, key_2_0);
    }

    /// `enlarge` (#36) changes the resize pipeline's output, so two
    /// requests differing only in `enlarge` must not collide onto the same
    /// cache key.
    #[test]
    fn enlarge_true_and_false_produce_distinct_keys() {
        let cache = cache_service();

        let base = |enlarge: bool| {
            params_with_enlarge(
                "https://ex.com/a.jpg",
                Some(100),
                Some(100),
                ImageFormat::Png,
                None,
                Some(false),
                enlarge,
            )
        };

        assert_ne!(
            cache.generate_key(&base(true)),
            cache.generate_key(&base(false))
        );
    }

    /// #34/#60 (v5 bump): `background` changes the resize pipeline's output
    /// bytes (flatten/normalise colour), so distinct backgrounds - and the
    /// unset ("use the default") case - must not collide onto the same
    /// cache key.
    #[test]
    fn background_produces_distinct_keys() {
        let cache = cache_service();

        let base = |background: Option<[u8; 3]>| ResizeQuery {
            url: "https://ex.com/a.jpg".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type: ResizeType::Fit,
            format: ImageFormat::Jpg,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background,
        };

        let keys: HashSet<String> = [None, Some([255, 255, 255]), Some([0, 0, 0]), Some([1, 2, 3])]
            .into_iter()
            .map(|bg| cache.generate_key(&base(bg)))
            .collect();

        assert_eq!(
            keys.len(),
            4,
            "each distinct background (including unset) must produce a distinct cache key"
        );
    }

    /// #35 (v6 bump): `quality` changes the encoded output bytes, so
    /// distinct qualities - and the unset ("use the encoder's default")
    /// case - must not collide onto the same cache key.
    #[test]
    fn quality_produces_distinct_keys() {
        let cache = cache_service();

        let base = |quality: Option<u8>| ResizeQuery {
            url: "https://ex.com/a.jpg".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type: ResizeType::Fit,
            format: ImageFormat::Jpg,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background: None,
        };

        let keys: HashSet<String> = [None, Some(30), Some(75), Some(90)]
            .into_iter()
            .map(|q| cache.generate_key(&base(q)))
            .collect();

        assert_eq!(
            keys.len(),
            4,
            "each distinct quality (including unset) must produce a distinct cache key"
        );
    }

    /// #35 (v6 bump): a per-format quality override changes the encoded
    /// output bytes independently of the global `quality`, so it must be
    /// its own dimension in the cache key rather than being folded into (or
    /// ignored relative to) `quality`.
    #[test]
    fn format_quality_produces_distinct_keys_from_global_quality() {
        let cache = cache_service();

        let base = |quality: Option<u8>, jpeg_quality: Option<u8>| ResizeQuery {
            url: "https://ex.com/a.jpg".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type: ResizeType::Fit,
            format: ImageFormat::Jpg,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality,
            jpeg_quality,
            webp_quality: None,
            webp_lossless: None,
            background: None,
        };

        let global_only = cache.generate_key(&base(Some(80), None));
        let override_only = cache.generate_key(&base(None, Some(80)));
        let both = cache.generate_key(&base(Some(80), Some(80)));
        let neither = cache.generate_key(&base(None, None));

        let keys: HashSet<String> = [
            global_only.clone(),
            override_only.clone(),
            both.clone(),
            neither.clone(),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            keys.len(),
            4,
            "quality and jpeg_quality must each be an independent cache-key dimension"
        );
    }

    /// #35 (v6 bump): `webp_lossless` changes the encoded output bytes
    /// (an entirely different codec path - `encode_lossless` vs `encode`),
    /// so it must not collide with the lossy default.
    #[test]
    fn webp_lossless_produces_distinct_keys() {
        let cache = cache_service();

        let base = |webp_lossless: Option<bool>| ResizeQuery {
            url: "https://ex.com/a.jpg".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type: ResizeType::Fit,
            format: ImageFormat::Webp,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless,
            background: None,
        };

        let keys: HashSet<String> = [None, Some(false), Some(true)]
            .into_iter()
            .map(|l| cache.generate_key(&base(l)))
            .collect();

        assert_eq!(
            keys.len(),
            3,
            "each distinct webp_lossless value (including unset) must produce a distinct cache key"
        );
    }

    /// #59 (v4 bump): before #59, `fit`/`fill`/`force`/`auto` were parsed
    /// but ignored, so every width+height request resized identically and
    /// `resize_type` didn't need to be part of the key. Now that each type
    /// produces different output bytes for the same width/height, every
    /// pairwise-distinct type must also produce a distinct cache key -
    /// otherwise a `fit` request could be served a `fill`-shaped cached
    /// response (or vice versa), silently resurrecting the #59 bug via a
    /// stale cache instead of an unversioned key.
    #[test]
    fn resize_type_produces_distinct_keys() {
        let cache = cache_service();

        let base = |resize_type: ResizeType| ResizeQuery {
            url: "https://ex.com/a.jpg".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type,
            format: ImageFormat::Png,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            jpeg_quality: None,
            webp_quality: None,
            webp_lossless: None,
            background: None,
        };

        let keys: HashSet<String> = [
            ResizeType::Fit,
            ResizeType::Fill,
            ResizeType::Force,
            ResizeType::Auto,
        ]
        .into_iter()
        .map(|kind| cache.generate_key(&base(kind)))
        .collect();

        assert_eq!(
            keys.len(),
            4,
            "each resize type must produce a distinct cache key"
        );
    }

    /// Property-style check: a set of distinct parameter tuples must all
    /// produce distinct keys. Written as a plain loop over a `HashSet`
    /// rather than pulling in a proptest-style dependency.
    #[test]
    fn distinct_parameter_tuples_produce_distinct_keys() {
        let cache = cache_service();

        let urls = [
            "https://ex.com/a.jpg",
            "https://ex.com/a.jpg1",
            "https://ex.com/b.jpg",
            "https://ex.com/a.jpg?w=1",
        ];
        let widths: [Option<u32>; 3] = [None, Some(1), Some(12)];
        let heights: [Option<u32>; 3] = [None, Some(3), Some(23)];
        let formats = [ImageFormat::Jpg, ImageFormat::Png, ImageFormat::Webp];
        let blur_sigmas: [Option<f32>; 4] = [None, Some(0.0), Some(1.5), Some(2.0)];
        let grayscales: [Option<bool>; 3] = [None, Some(true), Some(false)];
        let enlarges = [false, true];

        let mut tuples = Vec::new();
        for url in urls {
            for width in widths {
                for height in heights {
                    for format in formats {
                        for blur_sigma in blur_sigmas {
                            for grayscale in grayscales {
                                for enlarge in enlarges {
                                    tuples.push(params_with_enlarge(
                                        url, width, height, format, blur_sigma, grayscale, enlarge,
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        // `Some(0.0)` and `None` for blur_sigma are intentionally
        // canonicalised to the same key (see
        // `blur_sigma_inactive_values_collapse_to_one_key`), so collapse
        // each tuple onto its expected-distinct equivalence class before
        // asserting injectivity: keep only one blur_sigma representative
        // ("no blur") among {None, Some(0.0)} per remaining-field
        // combination.
        let mut seen_keys = HashSet::new();
        let mut seen_classes = HashSet::new();
        for t in &tuples {
            let class = (
                t.url.clone(),
                t.width,
                t.height,
                t.format.to_string(),
                if matches!(t.blur_sigma, Some(s) if s > 0.0) {
                    Some(t.blur_sigma.unwrap().to_string())
                } else {
                    None
                },
                t.grayscale,
                t.enlarge,
            );
            if !seen_classes.insert(class) {
                continue;
            }

            let key = cache.generate_key(t);
            assert!(
                seen_keys.insert(key),
                "duplicate cache key produced for a distinct parameter tuple"
            );
        }
    }

    /// The output shape `{sub_path}{64 hex chars}.{format}` must be
    /// unchanged, since other code (and a concurrent validator matching
    /// `^[0-9a-f]{{64}}\.(jpg|png|webp)$` after the sub_path prefix) depends
    /// on it.
    #[test]
    fn output_matches_expected_shape() {
        let cache = CacheServiceBuilder::default()
            .minio_sub_path("images/".to_string())
            .build()
            .expect("build CacheService");

        let key = cache.generate_key(&params(
            "https://ex.com/a.jpg",
            Some(800),
            Some(600),
            ImageFormat::Webp,
            Some(1.5),
            Some(false),
        ));

        assert!(key.starts_with("images/"));
        let rest = key.strip_prefix("images/").unwrap();
        let (hex, ext) = rest.split_once('.').expect("expected a '.' separator");
        assert_eq!(hex.len(), 64, "hex digest should be 64 chars: {hex}");
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "hex digest should be lowercase hex: {hex}"
        );
        assert_eq!(ext, "webp");
    }
}
