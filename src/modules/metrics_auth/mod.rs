//! `/metrics` bearer-token authentication (#77, a leftover from #27's
//! stated scope).
//!
//! `/metrics` is only ever mounted, and only ever has anything to serve,
//! when the binary is built with `--features otel`
//! (`src/modules/router/router.rs`, `src/services/metrics`) - so this whole
//! module is gated the same way `src/modules/tracer` already is, rather
//! than existing (and demanding a startup decision) in builds that can
//! never expose the endpoint it protects.

mod config;
mod middleware;

pub use config::MetricsAuthConfig;
pub use middleware::require_metrics_auth;
