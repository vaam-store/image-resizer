use crate::modules::env::env::EnvConfig;
use anyhow::{Result, bail};

/// Runtime configuration for signed-URL verification (#27).
///
/// Read from `SIGNING_KEY` / `SIGNING_SALT` / `ALLOW_UNSIGNED_REQUESTS`
/// (`EnvConfig`, `src/modules/env/env.rs`) - hex-encoded key/salt, mirroring
/// imgproxy's own `IMGPROXY_KEY`/`IMGPROXY_SALT` shape closely enough that
/// operators migrating from imgproxy can reuse the same values.
#[derive(Clone, Default)]
pub struct SigningConfig {
    pub key: Vec<u8>,
    pub salt: Vec<u8>,
    /// Opt-in escape hatch for local development (#27's "unsigned mode"):
    /// when `true`, a request whose signature segment is the literal
    /// `unsigned` bypasses verification entirely. Signing itself stays the
    /// default regardless of this flag - it only ever widens the *unsigned*
    /// escape path, never weakens verification of a real signature.
    pub allow_unsigned: bool,
}

/// Hand-written, not derived: `key`/`salt` are secret material and must
/// never show up verbatim in a `{:?}` log line, test failure message, or
/// panic payload - only their lengths and `allow_unsigned` are printed.
impl std::fmt::Debug for SigningConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningConfig")
            .field("key", &format_args!("<{} bytes redacted>", self.key.len()))
            .field("salt", &format_args!("<{} bytes redacted>", self.salt.len()))
            .field("allow_unsigned", &self.allow_unsigned)
            .finish()
    }
}

impl SigningConfig {
    /// A real key and salt are both configured, so a non-`unsigned` request
    /// can actually be verified.
    pub fn enabled(&self) -> bool {
        !self.key.is_empty() && !self.salt.is_empty()
    }

    /// Builds the signing configuration, failing closed at startup rather
    /// than per-request (#27: "signing is the default, not opt-in") - a
    /// deployment with neither a real key/salt nor an explicit
    /// `ALLOW_UNSIGNED_REQUESTS=true` opt-out can never verify anything, so
    /// refusing to start is preferable to silently serving 403s to every
    /// caller.
    pub fn from_env(config: &EnvConfig) -> Result<Self> {
        let allow_unsigned = config.allow_unsigned_requests.unwrap_or(false);
        let key = decode_hex_env("SIGNING_KEY", config.signing_key.as_deref())?;
        let salt = decode_hex_env("SIGNING_SALT", config.signing_salt.as_deref())?;

        let signing = Self {
            key,
            salt,
            allow_unsigned,
        };

        if !signing.enabled() && !signing.allow_unsigned {
            bail!(
                "SIGNING_KEY and SIGNING_SALT must both be set (hex-encoded) - signed URLs are \
                 the default (#27), not opt-in. Set ALLOW_UNSIGNED_REQUESTS=true explicitly for \
                 local development instead if you don't want to configure a key yet."
            );
        }

        Ok(signing)
    }
}

/// Decodes a hex-encoded environment variable into raw bytes. No external
/// hex crate is pulled in for this - the format is fixed (even-length,
/// `[0-9a-fA-F]*`) and small enough to hand-roll.
fn decode_hex_env(name: &str, raw: Option<&str>) -> Result<Vec<u8>> {
    let Some(raw) = raw.filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };

    if !raw.len().is_multiple_of(2) {
        bail!("{name} must be an even-length hex string, got {} chars", raw.len());
    }

    let mut bytes = Vec::with_capacity(raw.len() / 2);
    let raw_bytes = raw.as_bytes();
    for chunk in raw_bytes.chunks_exact(2) {
        let hi = hex_nibble(chunk[0]).ok_or_else(|| {
            anyhow::anyhow!("{name} contains a non-hex character: {:?}", chunk[0] as char)
        })?;
        let lo = hex_nibble(chunk[1]).ok_or_else(|| {
            anyhow::anyhow!("{name} contains a non-hex character: {:?}", chunk[1] as char)
        })?;
        bytes.push((hi << 4) | lo);
    }

    Ok(bytes)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envconfig::Envconfig;

    fn env(
        signing_key: Option<&str>,
        signing_salt: Option<&str>,
        allow_unsigned: Option<bool>,
    ) -> EnvConfig {
        let mut config = EnvConfig::init_from_hashmap(&std::collections::HashMap::new())
            .expect("EnvConfig has defaults for every field envconfig knows about");
        config.signing_key = signing_key.map(str::to_string);
        config.signing_salt = signing_salt.map(str::to_string);
        config.allow_unsigned_requests = allow_unsigned;
        config
    }

    #[test]
    fn decodes_valid_hex_key_and_salt() {
        let config = env(Some("00ff"), Some("a1b2"), None);
        let signing = SigningConfig::from_env(&config).expect("valid hex should decode");
        assert_eq!(signing.key, vec![0x00, 0xff]);
        assert_eq!(signing.salt, vec![0xa1, 0xb2]);
        assert!(signing.enabled());
        assert!(!signing.allow_unsigned);
    }

    #[test]
    fn hex_decoding_is_case_insensitive() {
        let config = env(Some("A1B2"), Some("a1b2"), None);
        let signing = SigningConfig::from_env(&config).expect("mixed-case hex should decode");
        assert_eq!(signing.key, signing.salt);
    }

    #[test]
    fn odd_length_hex_is_rejected() {
        let config = env(Some("abc"), Some("a1b2"), None);
        assert!(SigningConfig::from_env(&config).is_err());
    }

    #[test]
    fn non_hex_characters_are_rejected() {
        let config = env(Some("zz"), Some("a1b2"), None);
        assert!(SigningConfig::from_env(&config).is_err());
    }

    #[test]
    fn missing_key_and_salt_without_allow_unsigned_fails_closed() {
        let config = env(None, None, None);
        let err = SigningConfig::from_env(&config).expect_err(
            "refusing to start unverifiable is the point - signing defaults to required (#27)",
        );
        assert!(err.to_string().contains("ALLOW_UNSIGNED_REQUESTS"));
    }

    #[test]
    fn missing_key_and_salt_with_allow_unsigned_true_is_accepted() {
        let config = env(None, None, Some(true));
        let signing = SigningConfig::from_env(&config).expect("explicit opt-out is allowed");
        assert!(!signing.enabled());
        assert!(signing.allow_unsigned);
    }

    #[test]
    fn key_configured_but_allow_unsigned_still_works_independently() {
        let config = env(Some("00ff"), Some("a1b2"), Some(true));
        let signing = SigningConfig::from_env(&config).expect("valid config");
        assert!(signing.enabled());
        assert!(signing.allow_unsigned);
    }
}
