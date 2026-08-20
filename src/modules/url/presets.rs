//! Preset definitions and the processing-option allowlist (#52,
//! imgproxy's `PRESETS`/`ALLOWED_PROCESSING_OPTIONS` config options -
//! <https://docs.imgproxy.net/configuration#presets>,
//! <https://docs.imgproxy.net/configuration#allowed-processing-options>).
//!
//! This module was reconstructed during wave-2 integration: the `#52`
//! patch declared `pub mod presets;` in `src/modules/url/mod.rs` and used
//! `PresetRegistry`/`AllowedOptions` throughout (`SignedRequest::parse_with_config`,
//! `ApiService`), but the module file itself was missing from the patch.
//! The implementation below matches every call site and test the patch
//! shipped - see `crate::modules::url::mod`'s `parse_with_config` for how
//! the two types are actually used, and its test module for the exact
//! `PRESETS`/allowlist shapes exercised.

use std::collections::{HashMap, HashSet};
use std::fmt;

/// Parsed `PRESETS` config value: a set of named, reusable option-segment
/// lists. `PRESETS` itself is a comma-separated list of `{name}={options}`
/// entries, `{options}` itself `/`-separated processing-option segments -
/// e.g. `thumbnail=rs:fill:300:300/q:80,default=el:1` - mirroring imgproxy's
/// own `PRESETS` format.
///
/// A preset named `default` is special: `SignedRequest::parse_with_config`
/// prepends it ahead of every request's own segments automatically, even
/// when the request never names a preset at all - see
/// [`Self::default_preset`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetRegistry {
    presets: HashMap<String, Vec<String>>,
}

/// Error returned by [`PresetRegistry::parse`] for a malformed `PRESETS`
/// value. Implements [`std::error::Error`]/[`fmt::Display`] so callers can
/// fold it into their own error type with `{e}`/`?` (see
/// `ApiService::create`, `src/modules/api/handler.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetParseError(String);

impl fmt::Display for PresetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PresetParseError {}

impl PresetRegistry {
    /// No presets configured - `PRESETS` unset/blank. Every `pr:{name}`
    /// segment is then a [`crate::modules::url::UrlParseError::UnknownPreset`],
    /// and there is no `default` preset to auto-apply.
    pub fn empty() -> Self {
        Self {
            presets: HashMap::new(),
        }
    }

    /// Parses a `PRESETS`-shaped config value: comma-separated
    /// `{name}={options}` entries, `{options}` itself `/`-separated
    /// processing-option segments. An empty/blank value parses to
    /// [`Self::empty`] rather than erroring - "unset" and "explicitly
    /// empty" behave the same way, mirroring
    /// `crate::config::performance`'s other optional config values.
    ///
    /// Rejects (rather than silently accepting) three malformed shapes:
    /// - an entry with no `=` (no name/options separator);
    /// - an entry naming an empty preset name, or one with no options;
    /// - a duplicate preset name;
    /// - a preset whose own definition contains a `pr:` segment - presets
    ///   don't recurse (see `SignedRequest::parse_with_config`'s doc
    ///   comment for why: a preset's expansion is spliced in verbatim,
    ///   never itself re-scanned for further `pr:` segments, so a `pr:`
    ///   segment inside one would silently pass through as a literal,
    ///   never-expanded option instead of doing anything - caught here at
    ///   config-load time instead of confusing a caller at request time).
    pub fn parse(raw: &str) -> Result<Self, PresetParseError> {
        let mut presets = HashMap::new();

        for entry in raw.split(',') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }

            let (name, options) = entry.split_once('=').ok_or_else(|| {
                PresetParseError(format!(
                    "preset entry {entry:?} is missing '=' (expected name=opt1:val/opt2:val)"
                ))
            })?;
            let name = name.trim();
            if name.is_empty() {
                return Err(PresetParseError(format!(
                    "preset entry {entry:?} has an empty name"
                )));
            }

            let segments: Vec<String> = options
                .split('/')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if segments.is_empty() {
                return Err(PresetParseError(format!(
                    "preset {name:?} has no options (expected at least one opt1:val segment \
                     after '=')"
                )));
            }

            if let Some(bad) = segments
                .iter()
                .find(|seg| seg.split(':').next().unwrap_or_default() == "pr")
            {
                return Err(PresetParseError(format!(
                    "preset {name:?} contains a pr: segment ({bad:?}) - presets cannot \
                     reference other presets"
                )));
            }

            if presets.insert(name.to_string(), segments).is_some() {
                return Err(PresetParseError(format!("duplicate preset name {name:?}")));
            }
        }

        Ok(Self { presets })
    }

    /// The option segments for `name`, if configured. `None` for an
    /// unknown preset - the caller (`SignedRequest::parse_with_config`)
    /// turns that into `UrlParseError::UnknownPreset`.
    pub fn get(&self, name: &str) -> Option<&Vec<String>> {
        self.presets.get(name)
    }

    /// The `default` preset's segments, if one is configured - applied
    /// automatically ahead of every request's own segments regardless of
    /// whether the request names a preset itself. `None` when no preset
    /// named exactly `default` exists.
    pub fn default_preset(&self) -> Option<&Vec<String>> {
        self.presets.get("default")
    }
}

/// Parsed `ALLOWED_PROCESSING_OPTIONS` config value: an allowlist of
/// processing-option short codes (`rs`, `q`, `pr`, ...) permitted directly
/// in a request URL. Deliberately does **not** apply to options used
/// *inside* a preset's own definition (`PresetRegistry::parse` already
/// validated those at config-load time, and imgproxy's own documented
/// behaviour is the same split: the allowlist governs what a caller can
/// name directly, not what an operator-authored preset expands to) - this
/// is what lets an operator hand out a restricted set of presets while
/// forbidding the raw options they're built from directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedOptions {
    /// `None` means unrestricted (every code is allowed) - distinct from
    /// `Some(empty set)`, which would allow nothing at all.
    allowed: Option<HashSet<String>>,
}

impl AllowedOptions {
    /// No restriction - every processing-option code is allowed. The
    /// default when `ALLOWED_PROCESSING_OPTIONS` is unset/blank.
    pub fn unrestricted() -> Self {
        Self { allowed: None }
    }

    /// Parses a comma-separated allowlist (e.g. `rs,q,pr`). A blank value
    /// (or one that trims down to no codes at all) is equivalent to
    /// [`Self::unrestricted`], matching `PresetRegistry::parse`'s "unset
    /// and explicitly empty behave the same" convention.
    pub fn parse(raw: &str) -> Self {
        let codes: HashSet<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        if codes.is_empty() {
            Self::unrestricted()
        } else {
            Self {
                allowed: Some(codes),
            }
        }
    }

    /// Whether `code` (a processing-option short code, e.g. `"rs"`) may be
    /// used directly in a request URL.
    pub fn is_allowed(&self, code: &str) -> bool {
        match &self.allowed {
            None => true,
            Some(set) => set.contains(code),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_has_no_presets_and_no_default() {
        let registry = PresetRegistry::empty();
        assert_eq!(registry.get("thumb"), None);
        assert_eq!(registry.default_preset(), None);
    }

    #[test]
    fn parse_empty_string_is_an_empty_registry() {
        assert_eq!(PresetRegistry::parse("").unwrap(), PresetRegistry::empty());
        assert_eq!(
            PresetRegistry::parse("   ").unwrap(),
            PresetRegistry::empty()
        );
    }

    #[test]
    fn parses_a_single_preset() {
        let registry = PresetRegistry::parse("thumb=rs:fill:300:300/q:80").unwrap();
        assert_eq!(
            registry.get("thumb"),
            Some(&vec!["rs:fill:300:300".to_string(), "q:80".to_string()])
        );
    }

    #[test]
    fn parses_multiple_comma_separated_presets() {
        let registry = PresetRegistry::parse("thumb=rs:fill:300:300/q:80,default=el:1").unwrap();
        assert_eq!(
            registry.get("thumb"),
            Some(&vec!["rs:fill:300:300".to_string(), "q:80".to_string()])
        );
        assert_eq!(
            registry.default_preset(),
            Some(&vec!["el:1".to_string()])
        );
    }

    #[test]
    fn entry_without_equals_sign_is_rejected() {
        assert!(PresetRegistry::parse("thumb").is_err());
    }

    #[test]
    fn empty_preset_name_is_rejected() {
        assert!(PresetRegistry::parse("=q:80").is_err());
    }

    #[test]
    fn preset_with_no_options_is_rejected() {
        assert!(PresetRegistry::parse("thumb=").is_err());
    }

    #[test]
    fn duplicate_preset_name_is_rejected() {
        assert!(PresetRegistry::parse("thumb=q:80,thumb=q:90").is_err());
    }

    /// Presets cannot reference other presets - see `PresetRegistry::parse`'s
    /// doc comment for why this is rejected at config-load time.
    #[test]
    fn preset_referencing_another_preset_is_rejected() {
        assert!(PresetRegistry::parse("a=q:80,b=pr:a").is_err());
    }

    #[test]
    fn unrestricted_allows_any_code() {
        let allowed = AllowedOptions::unrestricted();
        assert!(allowed.is_allowed("rs"));
        assert!(allowed.is_allowed("anything"));
    }

    #[test]
    fn parse_empty_string_is_unrestricted() {
        assert_eq!(AllowedOptions::parse(""), AllowedOptions::unrestricted());
    }

    #[test]
    fn parsed_allowlist_only_allows_listed_codes() {
        let allowed = AllowedOptions::parse("rs,q,pr");
        assert!(allowed.is_allowed("rs"));
        assert!(allowed.is_allowed("q"));
        assert!(allowed.is_allowed("pr"));
        assert!(!allowed.is_allowed("bl"));
    }
}
