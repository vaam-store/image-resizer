use crate::modules::env::env::EnvConfig;
use anyhow::{Result, bail};
use subtle::ConstantTimeEq;

/// Runtime configuration for `/metrics` authentication (#77).
///
/// Read from `METRICS_AUTH_TOKEN` / `ALLOW_UNAUTHENTICATED_METRICS`
/// (`EnvConfig`, `src/modules/env/env.rs`). Deliberately mirrors
/// `SigningConfig` (`src/modules/signing/config.rs`) exactly: `/metrics`
/// exposes request rates, cache hit ratios, error counts and latency
/// histograms - precise reconnaissance for an attacker probing for
/// expensive requests - so this fails closed at startup rather than
/// per-request. A deployment with neither a real token nor an explicit
/// `ALLOW_UNAUTHENTICATED_METRICS=true` opt-out can never be told apart
/// from "operator forgot to configure this", so refusing to start is
/// preferable to silently serving traffic telemetry to anyone who asks.
#[derive(Clone, Default)]
pub struct MetricsAuthConfig {
    token: Vec<u8>,
    /// Opt-in escape hatch: when `true`, `/metrics` is served without
    /// requiring a bearer token at all. Never weakens verification of a
    /// real token - it only ever widens the unauthenticated-access escape
    /// path, exactly like `SigningConfig::allow_unsigned`.
    pub allow_unauthenticated: bool,
}

/// Hand-written, not derived: `token` is secret material and must never
/// show up verbatim in a `{:?}` log line, test failure message, or panic
/// payload - only its length and `allow_unauthenticated` are printed.
impl std::fmt::Debug for MetricsAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsAuthConfig")
            .field("token", &format_args!("<{} bytes redacted>", self.token.len()))
            .field("allow_unauthenticated", &self.allow_unauthenticated)
            .finish()
    }
}

impl MetricsAuthConfig {
    /// A real token is configured, so a request can actually be verified.
    pub fn enabled(&self) -> bool {
        !self.token.is_empty()
    }

    /// Builds the `/metrics` auth configuration, failing closed at startup
    /// (#77) rather than per-request - see the module-level docs on why
    /// that asymmetry (fail closed, explicit opt-out) is the point.
    pub fn from_env(config: &EnvConfig) -> Result<Self> {
        let allow_unauthenticated = config.allow_unauthenticated_metrics.unwrap_or(false);

        // A token configured as empty or all-whitespace (`METRICS_AUTH_TOKEN=`
        // or `METRICS_AUTH_TOKEN="   "`) is malformed, not a real secret -
        // treat it as unset rather than let `enabled()` report `true` for a
        // token nothing could ever match, which would make every request
        // fail with a confusing 401 instead of failing loudly at startup.
        let token = config
            .metrics_auth_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.as_bytes().to_vec())
            .unwrap_or_default();

        let metrics_auth = Self {
            token,
            allow_unauthenticated,
        };

        if !metrics_auth.enabled() && !metrics_auth.allow_unauthenticated {
            bail!(
                "METRICS_AUTH_TOKEN must be set (#77) - /metrics exposes request rates, cache \
                 hit ratios, error counts and latency histograms, which is reconnaissance-grade \
                 information about this service's traffic. Set METRICS_AUTH_TOKEN to a secret \
                 bearer token that the Prometheus scraper sends via its `authorization` scrape \
                 config, or set ALLOW_UNAUTHENTICATED_METRICS=true to explicitly opt into \
                 serving /metrics without authentication."
            );
        }

        Ok(metrics_auth)
    }

    /// Constant-time bearer-token check, exactly as
    /// `signing::verify::verify_signature` does via `subtle::ConstantTimeEq`
    /// (#77) - a naive `==` here would be a timing oracle over the
    /// configured secret.
    ///
    /// The length comparison ahead of `ct_eq` is not itself a timing leak:
    /// like `verify_signature`, only the token's *content* is secret, not
    /// its length, and no established HMAC/token verifier tries to hide
    /// length either.
    pub fn verify_token(&self, provided: &str) -> bool {
        if !self.enabled() {
            return false;
        }

        let provided = provided.as_bytes();
        if provided.len() != self.token.len() {
            return false;
        }

        provided.ct_eq(&self.token).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use envconfig::Envconfig;

    fn env(token: Option<&str>, allow_unauthenticated: Option<bool>) -> EnvConfig {
        let mut config = EnvConfig::init_from_hashmap(&std::collections::HashMap::new())
            .expect("EnvConfig has defaults for every field envconfig knows about");
        config.metrics_auth_token = token.map(str::to_string);
        config.allow_unauthenticated_metrics = allow_unauthenticated;
        config
    }

    #[test]
    fn valid_token_is_accepted_by_verify_token() {
        let config = env(Some("s3cr3t-token"), None);
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid token configured");
        assert!(metrics_auth.enabled());
        assert!(metrics_auth.verify_token("s3cr3t-token"));
    }

    #[test]
    fn wrong_token_is_rejected_by_verify_token() {
        let config = env(Some("s3cr3t-token"), None);
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid token configured");
        assert!(!metrics_auth.verify_token("wrong-token"));
    }

    #[test]
    fn wrong_length_token_is_rejected_by_verify_token() {
        let config = env(Some("s3cr3t-token"), None);
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid token configured");
        assert!(!metrics_auth.verify_token("short"));
    }

    #[test]
    fn empty_provided_token_is_rejected_by_verify_token() {
        let config = env(Some("s3cr3t-token"), None);
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid token configured");
        assert!(!metrics_auth.verify_token(""));
    }

    #[test]
    fn missing_token_without_opt_out_fails_closed_at_startup() {
        let config = env(None, None);
        let err = MetricsAuthConfig::from_env(&config).expect_err(
            "refusing to start unprotected is the point - metrics auth defaults to required (#77)",
        );
        assert!(err.to_string().contains("ALLOW_UNAUTHENTICATED_METRICS"));
        assert!(err.to_string().contains("METRICS_AUTH_TOKEN"));
    }

    #[test]
    fn empty_token_without_opt_out_fails_closed_at_startup() {
        // A token explicitly set to an empty string is malformed, not a
        // real secret - must fail closed exactly like an unset token,
        // never silently produce a `MetricsAuthConfig` that lets every
        // request through unauthenticated.
        let config = env(Some(""), None);
        assert!(MetricsAuthConfig::from_env(&config).is_err());
    }

    #[test]
    fn whitespace_only_token_without_opt_out_fails_closed_at_startup() {
        let config = env(Some("   "), None);
        assert!(MetricsAuthConfig::from_env(&config).is_err());
    }

    #[test]
    fn missing_token_with_allow_unauthenticated_is_accepted() {
        let config = env(None, Some(true));
        let metrics_auth =
            MetricsAuthConfig::from_env(&config).expect("explicit opt-out is allowed");
        assert!(!metrics_auth.enabled());
        assert!(metrics_auth.allow_unauthenticated);
        // With no token configured at all, nothing can ever verify - the
        // opt-out is what the middleware checks first, not this.
        assert!(!metrics_auth.verify_token("anything"));
    }

    #[test]
    fn token_configured_and_allow_unauthenticated_still_work_independently() {
        let config = env(Some("s3cr3t-token"), Some(true));
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid config");
        assert!(metrics_auth.enabled());
        assert!(metrics_auth.allow_unauthenticated);
        assert!(metrics_auth.verify_token("s3cr3t-token"));
    }

    #[test]
    fn debug_output_never_contains_the_token() {
        let config = env(Some("super-secret-value-that-must-not-leak"), None);
        let metrics_auth = MetricsAuthConfig::from_env(&config).expect("valid token configured");
        let debug_output = format!("{metrics_auth:?}");
        assert!(!debug_output.contains("super-secret-value-that-must-not-leak"));
        assert!(debug_output.contains("redacted"));
    }
}
