/// Deliberately unauthenticated (#77): see the doc comment on
/// `build_app` (`src/modules/router/router.rs`) for the full reasoning.
/// Kubernetes probes can't easily carry a secret, this handler only ever
/// leaks the literal string below (no traffic/cache/latency data, unlike
/// `/metrics`), and restricting it further belongs at the network layer,
/// not here.
pub async fn health() -> &'static str {
    "OK"
}
