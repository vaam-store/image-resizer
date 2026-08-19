use crate::modules::api::handler::ApiService;
use crate::modules::env::env::EnvConfig;
use crate::modules::router::router::router;
use crate::modules::utils::cgroup::effective_cpu_count;

use envconfig::Envconfig;
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

mod config;
mod models;
mod modules;
mod services;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// Default graceful-shutdown drain deadline (#42): must be comfortably
/// shorter than a typical orchestrator termination grace period - 30s is
/// Kubernetes' own default `terminationGracePeriodSeconds` - so the process
/// always finishes draining and flushing telemetry, and exits on its own,
/// before the orchestrator escalates to `SIGKILL`.
const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 20;

/// The Tokio runtime's `worker_threads` used to be a compile-time constant
/// (`#[tokio::main(worker_threads = 4)]`) while every other performance
/// knob in this service is configurable (#44). Attribute macros only
/// accept literals, so making this runtime-configurable means building the
/// runtime by hand instead of via `#[tokio::main]`.
fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let worker_threads = std::env::var("TOKIO_WORKER_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        // Cgroup-aware, not `num_cpus::get()` directly (#44): a container
        // limited to e.g. `400m` CPU on a much larger host would otherwise
        // size the runtime for the host's full core count. See
        // `modules::utils::cgroup` for why `num_cpus::get()` alone can't
        // see this.
        .unwrap_or_else(effective_cpu_count);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()?;

    debug!(worker_threads, "Configured Tokio runtime worker threads");

    runtime.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = EnvConfig::init_from_env()?;

    // Initialize tracing and OpenTelemetry
    #[cfg(feature = "otel")]
    let (metrics, trace_provider, meter_provider) =
        modules::tracer::init_tracing(config.clone()).await?;

    // Get address to listen on
    let addr = format!("{}:{:?}", config.http_host, config.http_port).parse::<SocketAddr>()?;
    let listener = TcpListener::bind(addr).await?;
    debug!(config.http_port, config.http_host, "Will start");
    debug!(
        config.max_concurrent_downloads,
        config.max_concurrent_processing,
        config.http_timeout_secs,
        config.max_image_size_mb,
        config.cpu_thread_pool_size,
        config.enable_http2,
        config.connection_pool_size,
        config.keep_alive_timeout_secs,
        config.performance_profile,
        "Performance configuration"
    );

    let api_service = Arc::new(ApiService::create(config)?);

    #[cfg(feature = "otel")]
    let app = router(metrics, api_service).await?;

    #[cfg(not(feature = "otel"))]
    let app = router(api_service).await?;

    let shutdown_timeout = shutdown_timeout_from_env();

    // Start the server, draining in-flight requests on SIGTERM/SIGINT
    // rather than hard-dropping them (#42).
    info!("Server running on http://{:?}", listener.local_addr()?);
    let server_result =
        serve_with_graceful_shutdown(listener, app, shutdown_timeout, wait_for_shutdown_signal())
            .await;

    if let Err(e) = &server_result {
        error!(error = %e, "server exited with an error");
    }

    #[cfg(feature = "otel")]
    {
        // Shut down the tracer/meter providers, flushing any buffered
        // telemetry. This block is reachable now (#42): before,
        // `axum::serve(..).await` never returned without a shutdown
        // signal, so traces and metrics were silently lost on every
        // restart.
        if let Err(e) = trace_provider.shutdown() {
            error!(error = ?e, "failed to shut down trace provider");
        }
        if let Err(e) = meter_provider.shutdown() {
            error!(error = ?e, "failed to shut down meter provider");
        }
    }

    server_result
}

/// Reads the graceful-shutdown drain deadline from `SHUTDOWN_TIMEOUT_SECS`
/// (#42). `src/modules/env/env.rs` is owned by another agent for this
/// change, so this reads the environment directly rather than adding a
/// field to `EnvConfig` - see the final report.
fn shutdown_timeout_from_env() -> Duration {
    parse_shutdown_timeout(std::env::var("SHUTDOWN_TIMEOUT_SECS").ok().as_deref())
}

/// Pure parsing logic behind [`shutdown_timeout_from_env`], split out so
/// tests can exercise it directly instead of mutating the real process
/// environment (which `cargo test`'s multi-threaded-by-default runner
/// would make racy against other tests).
fn parse_shutdown_timeout(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS))
}

/// Resolves once a SIGTERM or SIGINT (`Ctrl+C`) is received. SIGTERM is
/// what every container orchestrator (Kubernetes, Docker, ECS, ...) sends
/// first on a rolling update or scale-down; SIGINT covers `Ctrl+C` in a
/// local/dev shell.
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            error!(error = %e, "failed to install SIGINT handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                error!(error = %e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, starting graceful shutdown"),
        _ = terminate => info!("received SIGTERM, starting graceful shutdown"),
    }
}

/// Runs `app` on `listener`, and drains in-flight requests when
/// `shutdown_signal` resolves (#42) instead of dropping them: the acceptor
/// stops taking new connections immediately (axum's own
/// `with_graceful_shutdown` behavior), while in-flight requests are given
/// up to `drain_timeout` to finish.
///
/// Bounding the drain requires *not* simply wrapping
/// `axum::serve(..).with_graceful_shutdown(..)` in `tokio::time::timeout` -
/// that would also bound the server's entire pre-shutdown runtime, which is
/// wrong (the server would be killed after `drain_timeout` even with no
/// shutdown signal at all). Instead the server runs in its own task, and
/// only the *post-signal* join on that task is time-bounded, so the timeout
/// clock starts exactly when the drain itself starts.
///
/// `shutdown_signal` is generic (rather than hardcoded to OS signals) so
/// tests can inject a controlled trigger instead of sending a real
/// SIGTERM/SIGINT to the test process.
async fn serve_with_graceful_shutdown(
    listener: TcpListener,
    app: axum::Router,
    drain_timeout: Duration,
    shutdown_signal: impl std::future::Future<Output = ()> + Send + 'static,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let notify = Arc::new(Notify::new());
    let notify_for_server = notify.clone();

    // `PeerIpKeyExtractor` (the per-IP rate limiter, #43) needs the real
    // peer address in each request's extensions, which only
    // `into_make_service_with_connect_info` provides.
    let make_service = app.into_make_service_with_connect_info::<SocketAddr>();

    let server_task = tokio::spawn(async move {
        axum::serve(listener, make_service)
            .with_graceful_shutdown(async move {
                notify_for_server.notified().await;
            })
            .await
    });

    shutdown_signal.await;
    info!(
        drain_timeout_secs = drain_timeout.as_secs(),
        "shutdown signal received, draining in-flight requests"
    );
    notify.notify_one();

    match tokio::time::timeout(drain_timeout, server_task).await {
        Ok(Ok(Ok(()))) => {
            info!("server drained all in-flight requests and shut down cleanly");
            Ok(())
        }
        Ok(Ok(Err(e))) => Err(Box::new(e) as Box<dyn std::error::Error + Send + Sync>),
        Ok(Err(join_err)) => Err(Box::new(join_err) as Box<dyn std::error::Error + Send + Sync>),
        Err(_) => {
            warn!(
                drain_timeout_secs = drain_timeout.as_secs(),
                "graceful shutdown timed out before all in-flight requests finished draining; exiting anyway"
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::routing::get;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    /// #42: a request already in flight when the shutdown signal fires must
    /// still complete successfully, and once the signal has fired the
    /// server must stop accepting brand-new connections - draining, not
    /// hard-dropping.
    #[tokio::test]
    async fn graceful_shutdown_drains_in_flight_request_and_refuses_new_ones() {
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));

        let app = {
            let started = started.clone();
            let completed = completed.clone();
            Router::new().route(
                "/slow",
                get(move || {
                    let started = started.clone();
                    let completed = completed.clone();
                    async move {
                        started.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(300)).await;
                        completed.fetch_add(1, Ordering::SeqCst);
                        "done"
                    }
                }),
            )
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_signal = async move {
            let _ = signal_rx.await;
        };

        let serve_handle = tokio::spawn(serve_with_graceful_shutdown(
            listener,
            app,
            Duration::from_secs(5),
            shutdown_signal,
        ));

        // Kick off a slow in-flight request without awaiting its completion.
        let client = reqwest::Client::new();
        let in_flight = {
            let client = client.clone();
            let url = format!("http://{addr}/slow");
            tokio::spawn(async move { client.get(url).send().await })
        };

        // Give it time to actually start (enter the handler) before firing
        // the "signal".
        let started_at = Instant::now();
        while started.load(Ordering::SeqCst) == 0 && started_at.elapsed() < Duration::from_secs(2) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "request should have started"
        );

        // Fire the shutdown signal while the request is still in flight.
        let _ = signal_tx.send(());

        // New connection attempts after the signal must fail: the acceptor
        // stops taking connections as soon as graceful shutdown starts, and
        // this is the one deterministic client-observable proof of that.
        // Poll briefly since there's an inherent race between "signal sent"
        // and "acceptor loop actually stops".
        let mut refused = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if client
                .get(format!("http://{addr}/health-check-connection-should-fail"))
                .timeout(Duration::from_millis(200))
                .send()
                .await
                .is_err()
            {
                refused = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            refused,
            "server must refuse new connections once shutdown starts"
        );

        // The in-flight request must still complete successfully - drained,
        // not dropped.
        let response = in_flight
            .await
            .expect("join")
            .expect("in-flight request must complete despite shutdown");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(completed.load(Ordering::SeqCst), 1);

        // And the server task itself must have exited cleanly, well within
        // the drain timeout.
        let result = tokio::time::timeout(Duration::from_secs(5), serve_handle)
            .await
            .expect("serve_with_graceful_shutdown should return before the outer test timeout")
            .expect("join");
        assert!(result.is_ok(), "expected clean shutdown, got {result:?}");
    }

    /// #42: if in-flight work doesn't finish before `drain_timeout`, the
    /// function must still return (so the process can exit and flush
    /// telemetry) rather than hang forever.
    #[tokio::test]
    async fn graceful_shutdown_returns_even_if_drain_deadline_is_exceeded() {
        let app = Router::new().route(
            "/forever",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                "unreachable in this test"
            }),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");

        let (signal_tx, signal_rx) = tokio::sync::oneshot::channel::<()>();
        let shutdown_signal = async move {
            let _ = signal_rx.await;
        };

        let serve_handle = tokio::spawn(serve_with_graceful_shutdown(
            listener,
            app,
            Duration::from_millis(200),
            shutdown_signal,
        ));

        let client = reqwest::Client::new();
        let _in_flight = tokio::spawn({
            let url = format!("http://{addr}/forever");
            async move { client.get(url).send().await }
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = signal_tx.send(());

        let started_at = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(2), serve_handle)
            .await
            .expect("serve_with_graceful_shutdown must return promptly after its own drain_timeout elapses")
            .expect("join");

        assert!(
            result.is_ok(),
            "a timed-out drain should still be reported as handled, not an error"
        );
        assert!(
            started_at.elapsed() < Duration::from_secs(2),
            "should have returned close to drain_timeout (200ms), not waited for the 60s handler"
        );
    }

    #[test]
    fn parse_shutdown_timeout_defaults_when_unset() {
        assert_eq!(
            parse_shutdown_timeout(None),
            Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)
        );
    }

    #[test]
    fn parse_shutdown_timeout_reads_valid_override() {
        assert_eq!(parse_shutdown_timeout(Some("45")), Duration::from_secs(45));
    }

    #[test]
    fn parse_shutdown_timeout_rejects_zero_and_garbage() {
        assert_eq!(
            parse_shutdown_timeout(Some("0")),
            Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)
        );
        assert_eq!(
            parse_shutdown_timeout(Some("not-a-number")),
            Duration::from_secs(DEFAULT_SHUTDOWN_TIMEOUT_SECS)
        );
    }
}
