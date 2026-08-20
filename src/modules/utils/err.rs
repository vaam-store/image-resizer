use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use std::io;
use thiserror::Error;
use tracing::error;

/// Application-level error type carrying the correct HTTP semantics for every
/// failure the image pipeline can produce.
///
/// This used to exist but was never constructed anywhere (see #41): every
/// download/resize failure was swallowed into a bare `200 OK` with an empty
/// body, which is indistinguishable from a real (empty) image to both a CDN
/// and a client checking `response.ok`. Every variant here maps to a
/// non-cacheable response (`Cache-Control: no-store`) precisely because a
/// cached error is worse than an uncached one.
#[derive(Error, Debug)]
pub enum AppError {
    /// The requested resource does not exist (e.g. an unknown storage key).
    #[error("not found: {0}")]
    NotFound(String),

    /// The caller supplied invalid input (malformed URL, corrupt/unsupported
    /// image, a source image exceeding the configured size limit, a
    /// malformed signed-URL path, ...).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Signature verification failed, or an unsigned request was refused
    /// while signing is required (#27). `403`, not `401`: there's no
    /// credential-negotiation challenge (`WWW-Authenticate`) to offer here,
    /// matching imgproxy's own choice of `403` for exactly this case.
    #[error("forbidden: {0}")]
    Forbidden(String),

    /// A downstream/upstream dependency (origin server for `resize`, object
    /// storage for `download`) failed.
    #[error("bad gateway: {0}")]
    BadGateway(String),

    /// The service is shedding load and cannot handle the request right now.
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),

    /// I/O error: {0}
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),

    /// Any other failure that doesn't fit a more specific variant above.
    #[error("internal error: {0}")]
    AnyError(#[from] anyhow::Error),
}

impl AppError {
    fn status_code(&self) -> StatusCode {
        match self {
            AppError::NotFound(_) => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Forbidden(_) => StatusCode::FORBIDDEN,
            AppError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            AppError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            AppError::IoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::AnyError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Classify a `download`-path failure coming out of `ResizeService::download`.
    ///
    /// Tries a typed downcast first: `key_validation::validate_cache_key`
    /// (`src/services/storage/key_validation.rs`, owned separately) raises
    /// `InvalidKeyError` specifically so this can be an exact match instead
    /// of a heuristic. Everything else on this path still surfaces as a
    /// stringly-typed `anyhow::Error`, so the message-content fallback below
    /// stays in place as a stopgap until those failures grow typed errors
    /// too. An unrecognized message falls back to `502 Bad Gateway`, which is
    /// safer than a bare `200`.
    pub fn classify_download_error(err: anyhow::Error) -> Self {
        if err
            .downcast_ref::<crate::services::storage::key_validation::InvalidKeyError>()
            .is_some()
        {
            return AppError::NotFound(err.to_string());
        }

        let msg = err.to_string();
        if msg.to_lowercase().contains("not found") {
            AppError::NotFound(msg)
        } else {
            AppError::BadGateway(msg)
        }
    }

    /// Classify a `resize`-path failure coming out of `ResizeService::resize`.
    ///
    /// Tries a typed downcast first: `source_guard::SourceRejected`
    /// (`src/services/image/source_guard.rs`, owned separately) covers every
    /// SSRF-guard rejection - bad scheme, blocked IP literal, not
    /// allowlisted, DNS resolved to nothing/something blocked - and maps
    /// unconditionally to `400 Bad Request`, since in every one of those
    /// cases we refused the caller's request outright rather than failing to
    /// serve it; treating that as `502 Bad Gateway` (the string-heuristic
    /// fallback's default) would wrongly blame an upstream and invite
    /// retries of a request that can never succeed. Everything else on this
    /// path is still a message-based heuristic over an opaque
    /// `anyhow::Error`, not a match on a typed error - a stopgap until those
    /// failures grow typed errors too. See the message sources in
    /// `src/services/image/handler.rs` (`download_image`, `process_image`)
    /// and `src/services/resize/handler.rs` (`resize`).
    pub fn classify_resize_error(err: anyhow::Error) -> Self {
        if err
            .downcast_ref::<crate::services::image::source_guard::SourceRejected>()
            .is_some()
        {
            return AppError::BadRequest(err.to_string());
        }

        let msg = err.to_string();
        let lower = msg.to_lowercase();

        if lower.contains("too large") || lower.contains("decode") {
            // Caller-controlled: the URL they asked us to fetch either points
            // at an oversized payload or isn't a decodable image.
            AppError::BadRequest(msg)
        } else if lower.contains("permit") || lower.contains("cancelled") {
            // The download semaphore / CPU thread pool is shedding load.
            AppError::ServiceUnavailable(msg)
        } else if lower.contains("encode") {
            // We failed to encode our own output - not the caller's fault
            // and not an upstream failure either.
            AppError::AnyError(err)
        } else {
            // Everything else observed on this path (network failures,
            // non-2xx origin responses, storage upload failures, ...) is an
            // upstream dependency failing on us.
            AppError::BadGateway(msg)
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        error!(%status, "request failed: {}", self);

        let mut response = (status, self.to_string()).into_response();
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_store(response: &Response) -> bool {
        response
            .headers()
            .get(CACHE_CONTROL)
            .map(|v| v == "no-store")
            .unwrap_or(false)
    }

    #[test]
    fn classify_download_missing_key_as_not_found() {
        let err = anyhow::anyhow!("Image not found in storage: some/key.png");
        assert!(matches!(
            AppError::classify_download_error(err),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn classify_download_other_failure_as_bad_gateway() {
        let err = anyhow::anyhow!("S3 error: connection reset");
        assert!(matches!(
            AppError::classify_download_error(err),
            AppError::BadGateway(_)
        ));
    }

    /// Gap 2: `InvalidKeyError` (`src/services/storage/key_validation.rs`)
    /// must be downcast to `404 Not Found` directly, not only recognized via
    /// the `"not found"` string-match fallback - this test constructs the
    /// typed error itself (rather than a message that happens to contain
    /// "not found") so it would fail if the downcast were ever removed.
    #[test]
    fn classify_download_invalid_key_error_downcasts_to_not_found() {
        let key_err = crate::services::storage::key_validation::InvalidKeyError {
            key: "../../etc/passwd".to_string(),
        };
        let err: anyhow::Error = key_err.into();
        let classified = AppError::classify_download_error(err);
        assert!(matches!(classified, AppError::NotFound(_)));
        let response = classified.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Gap 1: every `SourceRejected` variant (`src/services/image/source_guard.rs`)
    /// must downcast to `400 Bad Request`, not fall through the string
    /// heuristic to the `502 Bad Gateway` default - a blocked-IP or
    /// unsupported-scheme rejection is the caller's fault, not an upstream
    /// failure, and 502 would wrongly invite the caller to retry a request
    /// that can never succeed.
    #[test]
    fn classify_resize_blocked_ip_source_rejection_as_bad_request_not_bad_gateway() {
        let rejected = crate::services::image::source_guard::SourceRejected::BlockedIpLiteral {
            host: "169.254.169.254".to_string(),
            addr: "169.254.169.254".parse().unwrap(),
        };
        let err: anyhow::Error = rejected.into();
        let classified = AppError::classify_resize_error(err);
        assert!(
            matches!(classified, AppError::BadRequest(_)),
            "expected BadRequest, got {classified:?}"
        );
        let response = classified.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn classify_resize_unsupported_scheme_rejection_as_bad_request_not_bad_gateway() {
        let rejected = crate::services::image::source_guard::SourceRejected::UnsupportedScheme {
            scheme: "gopher".to_string(),
        };
        let err: anyhow::Error = rejected.into();
        let classified = AppError::classify_resize_error(err);
        assert!(
            matches!(classified, AppError::BadRequest(_)),
            "expected BadRequest, got {classified:?}"
        );
        let response = classified.into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn classify_resize_decode_failure_as_bad_request() {
        let err = anyhow::anyhow!("Failed to decode image");
        assert!(matches!(
            AppError::classify_resize_error(err),
            AppError::BadRequest(_)
        ));
    }

    #[test]
    fn classify_resize_upstream_status_as_bad_gateway() {
        let err = anyhow::anyhow!("Failed to download image from http://x: status 500");
        assert!(matches!(
            AppError::classify_resize_error(err),
            AppError::BadGateway(_)
        ));
    }

    #[test]
    fn classify_resize_permit_failure_as_service_unavailable() {
        let err = anyhow::anyhow!("Failed to acquire download permit");
        assert!(matches!(
            AppError::classify_resize_error(err),
            AppError::ServiceUnavailable(_)
        ));
    }

    #[test]
    fn every_variant_maps_to_correct_status_and_is_non_cacheable() {
        let cases: Vec<(AppError, StatusCode)> = vec![
            (AppError::NotFound("x".into()), StatusCode::NOT_FOUND),
            (AppError::BadRequest("x".into()), StatusCode::BAD_REQUEST),
            (AppError::Forbidden("x".into()), StatusCode::FORBIDDEN),
            (AppError::BadGateway("x".into()), StatusCode::BAD_GATEWAY),
            (
                AppError::ServiceUnavailable("x".into()),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                AppError::AnyError(anyhow::anyhow!("boom")),
                StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (err, expected_status) in cases {
            let response = err.into_response();
            assert_eq!(response.status(), expected_status);
            assert!(no_store(&response), "expected Cache-Control: no-store");
        }
    }
}
