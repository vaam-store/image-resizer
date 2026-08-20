use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::config::MetricsAuthConfig;

const BEARER_PREFIX: &str = "Bearer ";

/// Gates `/metrics` behind a bearer token (#77). Mounted as a
/// `route_layer` on the `/metrics` route only
/// (`src/modules/router/router.rs`), so it never touches `/health` or any
/// other route - see that file's comment for why `/health` is left
/// unauthenticated on purpose.
///
/// Never logs the token, the `Authorization` header, or any part of the
/// request that could carry it - only the pass/fail outcome, via the
/// `401` response itself.
pub async fn require_metrics_auth(
    State(config): State<MetricsAuthConfig>,
    request: Request,
    next: Next,
) -> Response {
    // Explicit opt-out (#77): checked first and unconditionally - a
    // deployment that opted into unauthenticated metrics gets exactly
    // that, regardless of whether a token also happens to be configured.
    if config.allow_unauthenticated {
        return next.run(request).await;
    }

    let provided_token = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix(BEARER_PREFIX));

    let authorized = match provided_token {
        Some(token) => config.verify_token(token),
        None => false,
    };

    if authorized {
        return next.run(request).await;
    }

    unauthorized_response()
}

/// `401`, not `403`: unlike signed-URL verification (`AppError::Forbidden`,
/// `src/modules/api/resize.rs`) there *is* a real credential-negotiation
/// challenge to offer here - a bearer token the caller can supply on its
/// next request - so this follows RFC 7235 and sets `WWW-Authenticate`
/// rather than reusing the signing path's `403`.
fn unauthorized_response() -> Response {
    let mut response = (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"metrics\""),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::middleware as axum_middleware;
    use axum::routing::get;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn ok_handler() -> &'static str {
        "metrics body"
    }

    fn test_router(config: MetricsAuthConfig) -> Router {
        Router::new().route(
            "/metrics",
            get(ok_handler).route_layer(axum_middleware::from_fn_with_state(
                config,
                require_metrics_auth,
            )),
        )
    }

    async fn spawn(router: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service()).await;
        });
        addr
    }

    fn configured(token: &str, allow_unauthenticated: bool) -> MetricsAuthConfig {
        let env = {
            use envconfig::Envconfig;
            let mut env = crate::modules::env::env::EnvConfig::init_from_hashmap(
                &std::collections::HashMap::new(),
            )
            .expect("EnvConfig has defaults for every field envconfig knows about");
            env.metrics_auth_token = Some(token.to_string());
            env.allow_unauthenticated_metrics = Some(allow_unauthenticated);
            env
        };
        MetricsAuthConfig::from_env(&env).expect("valid metrics-auth config")
    }

    #[tokio::test]
    async fn valid_token_is_accepted() {
        let addr = spawn(test_router(configured("correct-token", false))).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/metrics"))
            .header("Authorization", "Bearer correct-token")
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "metrics body");
    }

    #[tokio::test]
    async fn invalid_token_is_rejected_with_401_and_www_authenticate() {
        let addr = spawn(test_router(configured("correct-token", false))).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/metrics"))
            .header("Authorization", "Bearer wrong-token")
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert!(
            response
                .headers()
                .get("www-authenticate")
                .expect("401 carries WWW-Authenticate")
                .to_str()
                .unwrap()
                .starts_with("Bearer")
        );
    }

    #[tokio::test]
    async fn missing_token_is_rejected_with_401() {
        let addr = spawn(test_router(configured("correct-token", false))).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn opt_out_allows_unauthenticated_access() {
        let addr = spawn(test_router(configured("correct-token", true))).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/metrics"))
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn malformed_authorization_header_is_rejected_with_401() {
        let addr = spawn(test_router(configured("correct-token", false))).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/metrics"))
            .header("Authorization", "correct-token")
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
