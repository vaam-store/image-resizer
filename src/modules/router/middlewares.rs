use axum::Router;
use axum::http::Method;
use tower_http::compression::predicate::{NotForContentType, SizeAbove};
use tower_http::compression::{CompressionLayer, DefaultPredicate, Predicate};
use tower_http::cors::{Any, CorsLayer};

#[inline]
pub fn apply_common_middlewares(router: Router) -> Router {
    let cors = CorsLayer::new()
        // allow `GET` and `POST` when accessing the resource
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::OPTIONS,
            Method::PUT,
            Method::DELETE,
            Method::PATCH,
        ])
        // allow requests from any origin
        .allow_origin(Any);

    let compression_predicate = DefaultPredicate::new()
        .and(NotForContentType::new("application/octet-stream"))
        .and(SizeAbove::new(0));

    let compression_layer = CompressionLayer::new()
        .br(true)
        .deflate(true)
        .gzip(true)
        .zstd(true)
        .compress_when(compression_predicate);

    router.layer(compression_layer).layer(cors)
}
