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
/// just because the code changed.
const CACHE_KEY_VERSION: u8 = 3;

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
    use gen_server::models::ImageFormat;
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
            format,
            blur_sigma,
            grayscale,
            enlarge,
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
