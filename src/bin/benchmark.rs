//! Load-test harness for the image-resize service's signed-path API
//! endpoint.
//!
//! Fixes relative to the previous version of this file:
//! - ONE shared `reqwest::Client` (previously a fresh client - fresh TCP +
//!   TLS handshake - was built inside every spawned task).
//! - A configurable, discarded warm-up phase runs before every concurrency
//!   level, so the first level measured isn't cold while later ones are warm.
//! - Reports p50/p90/p99/p99.9 and mean latency, throughput (req/s), and
//!   counts broken down by outcome (success / timeout / connection error /
//!   non-2xx) - not a single avg/min/max and a pass/fail boolean.
//! - Test images are served from a tiny local axum server started inside
//!   this process (the same deterministic fixture corpus the criterion
//!   benches use, see `benches/fixtures.rs`), so the "origin" the target
//!   server downloads from is local and adds no internet variance. There is
//!   no more `BENCHMARK_TEST_URLS` pointing at picsum.photos.
//! - Redirects are still followed (`reqwest`'s default policy) - the target
//!   service replies 301 to a CDN URL, and following it end-to-end (storage
//!   round trip included) is the correct thing to measure.
//! - Emits a machine-readable JSON report (`BENCHMARK_JSON_OUTPUT`)
//!   alongside the human-readable table, so CI can diff runs over time.

#[path = "../../benches/fixtures.rs"]
mod fixtures;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use envconfig::Envconfig;
use futures::stream::{self, StreamExt};
use serde::Serialize;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::time::sleep;

#[derive(Envconfig, Clone, Debug)]
pub struct BenchmarkConfig {
    /// Host of the *target* image-resize service under test.
    #[envconfig(from = "BENCHMARK_HOST", default = "localhost")]
    pub host: String,

    /// Port of the target image-resize service under test.
    #[envconfig(from = "BENCHMARK_PORT", default = "3000")]
    pub port: u16,

    #[envconfig(from = "BENCHMARK_CONCURRENCY_LEVELS", default = "1,5,10,20,50")]
    pub concurrency_levels: String,

    /// How many timed requests to run at each concurrency level. Decoupled
    /// from `concurrency` itself so percentiles (especially p99.9) have a
    /// real sample size instead of one data point per level.
    #[envconfig(from = "BENCHMARK_REQUESTS_PER_LEVEL", default = "200")]
    pub requests_per_level: usize,

    /// Requests to run (and discard) before timing each level, at that
    /// level's concurrency, so every level is measured warm.
    #[envconfig(from = "BENCHMARK_WARMUP_REQUESTS", default = "10")]
    pub warmup_requests: usize,

    /// Which generated fixtures (see `benches/fixtures.rs`) to rotate
    /// through as source images. Replaces the old `BENCHMARK_TEST_URLS`,
    /// which pulled real images from picsum.photos over the network.
    #[envconfig(
        from = "BENCHMARK_FIXTURE_NAMES",
        default = "photo_like,flat,alpha,tiny"
    )]
    pub fixture_names: String,

    #[envconfig(
        from = "BENCHMARK_RESIZE_PARAMS",
        default = "300x300,800x,x600,1200x800"
    )]
    pub resize_params: String,

    #[envconfig(from = "BENCHMARK_WAIT_BETWEEN_TESTS", default = "2")]
    pub wait_between_tests: u64,

    #[envconfig(from = "BENCHMARK_REQUEST_TIMEOUT", default = "60")]
    pub request_timeout: u64,

    #[envconfig(from = "BENCHMARK_OUTPUT_FORMAT", default = "jpg")]
    pub output_format: String,

    /// Host the local fixture-serving HTTP origin binds to. Must be
    /// reachable by the target service (usually the same machine).
    #[envconfig(from = "BENCHMARK_FIXTURE_HOST", default = "127.0.0.1")]
    pub fixture_host: String,

    /// Port for the local fixture origin. `0` picks a free port.
    #[envconfig(from = "BENCHMARK_FIXTURE_PORT", default = "0")]
    pub fixture_port: u16,

    /// Where to write the machine-readable JSON report.
    #[envconfig(
        from = "BENCHMARK_JSON_OUTPUT",
        default = "target/benchmark-results.json"
    )]
    pub json_output: String,
}

impl BenchmarkConfig {
    pub fn get_concurrency_levels(&self) -> Vec<usize> {
        self.concurrency_levels
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    pub fn get_fixture_names(&self) -> Vec<String> {
        self.fixture_names
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Format: "WIDTHxHEIGHT" where WIDTH or HEIGHT can be empty for aspect
    /// ratio preservation.
    pub fn get_resize_params(&self) -> Vec<(Option<u32>, Option<u32>)> {
        self.resize_params
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                let (width_str, height_str) = s.split_once('x')?;
                let width = if width_str.is_empty() {
                    None
                } else {
                    width_str.parse().ok()
                };
                let height = if height_str.is_empty() {
                    None
                } else {
                    height_str.parse().ok()
                };
                Some((width, height))
            })
            .collect()
    }

    pub fn get_base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.get_concurrency_levels().is_empty() {
            return Err("No valid concurrency levels configured".to_string());
        }
        if self.get_fixture_names().is_empty() {
            return Err("No valid fixture names configured".to_string());
        }
        if self.get_resize_params().is_empty() {
            return Err("No valid resize parameters configured".to_string());
        }
        if self.request_timeout == 0 {
            return Err("Request timeout must be greater than 0".to_string());
        }
        if self.requests_per_level == 0 {
            return Err("BENCHMARK_REQUESTS_PER_LEVEL must be greater than 0".to_string());
        }
        Ok(())
    }
}

// ---- Local fixture origin server -------------------------------------

type FixtureStore = Arc<HashMap<String, (Bytes, &'static str)>>;

async fn serve_fixture(
    AxumPath(name): AxumPath<String>,
    State(store): State<FixtureStore>,
) -> impl IntoResponse {
    match store.get(&name) {
        Some((bytes, content_type)) => {
            ([(header::CONTENT_TYPE, *content_type)], bytes.clone()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Starts a local HTTP server (in this process) serving the requested
/// fixtures under `/fixtures/{name}`, and returns its bound address once it
/// is confirmed to be accepting connections.
async fn start_fixture_server(config: &BenchmarkConfig) -> Result<SocketAddr> {
    let mut store = HashMap::new();
    for name in config.get_fixture_names() {
        let bytes = fixtures::by_name(&name)
            .with_context(|| format!("unknown fixture name in BENCHMARK_FIXTURE_NAMES: {name}"))?;
        let content_type = fixtures::content_type_for(&name);
        store.insert(name, (Bytes::from(bytes), content_type));
    }
    let store: FixtureStore = Arc::new(store);

    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/fixtures/{name}", get(serve_fixture))
        .with_state(store);

    let bind_addr = format!("{}:{}", config.fixture_host, config.fixture_port);
    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind local fixture origin on {bind_addr}"))?;
    let local_addr = listener.local_addr()?;

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("fixture origin server error: {e}");
        }
    });

    wait_until_ready(local_addr).await?;
    Ok(local_addr)
}

async fn wait_until_ready(addr: SocketAddr) -> Result<()> {
    let client = reqwest::Client::new();
    let url = format!("http://{addr}/health");
    for _ in 0..100 {
        if client.get(&url).send().await.is_ok() {
            return Ok(());
        }
        sleep(Duration::from_millis(20)).await;
    }
    anyhow::bail!("local fixture origin at {addr} did not become ready in time")
}

// ---- Request execution --------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Success,
    NonSuccess,
    Timeout,
    ConnectionError,
}

struct RequestResult {
    duration: Duration,
    outcome: Outcome,
}

fn classify_error(err: &reqwest::Error) -> Outcome {
    if err.is_timeout() {
        Outcome::Timeout
    } else {
        // Any other transport-level failure (connect refused, DNS, body
        // read error, TLS, etc.) is bucketed as a connection error.
        Outcome::ConnectionError
    }
}

async fn send_one(client: &reqwest::Client, url: &str) -> RequestResult {
    let start = Instant::now();
    match client.get(url).send().await {
        Ok(response) => {
            let status = response.status();
            // Fully drain the body: gives an honest end-to-end latency
            // (including following the redirect and reading the final
            // payload) and lets the connection go back into the pool.
            match response.bytes().await {
                Ok(_) => {
                    let duration = start.elapsed();
                    let outcome = if status.is_success() {
                        Outcome::Success
                    } else {
                        Outcome::NonSuccess
                    };
                    RequestResult { duration, outcome }
                }
                Err(e) => RequestResult {
                    duration: start.elapsed(),
                    outcome: classify_error(&e),
                },
            }
        }
        Err(e) => RequestResult {
            duration: start.elapsed(),
            outcome: classify_error(&e),
        },
    }
}

/// Runs `total_requests` GETs against `urls` (round-robin), at most
/// `concurrency` in flight at once, using the shared `client`.
async fn run_requests(
    client: &reqwest::Client,
    urls: &[String],
    concurrency: usize,
    total_requests: usize,
) -> (Duration, Vec<RequestResult>) {
    let start = Instant::now();
    let results = stream::iter(0..total_requests)
        .map(|i| {
            let client = client.clone();
            let url = urls[i % urls.len()].clone();
            async move { send_one(&client, &url).await }
        })
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await;
    (start.elapsed(), results)
}

/// Builds a request URL for the signed-path grammar that replaced the old
/// `/api/images/resize?url=...` query endpoint (GH #53/#27):
///
/// ```text
/// /{signature}/{processing_options}/{base64url source}.{extension}
/// ```
///
/// The source is base64url-encoded rather than `plain/...` because the
/// fixture URL contains slashes, and a base64url segment never does — one
/// segment, no percent-encoding ambiguity about what actually got signed.
///
/// If `SIGNING_KEY`/`SIGNING_SALT` are set, the path is signed for real so
/// the benchmark exercises the verification path. Otherwise it emits the
/// `unsigned` escape, which the service only honours when
/// `ALLOW_UNSIGNED_REQUESTS=true`.
fn build_url(
    target_base_url: &str,
    fixture_base_url: &str,
    fixture_name: &str,
    width: Option<u32>,
    height: Option<u32>,
    output_format: &str,
) -> String {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let source_url = format!("{fixture_base_url}/fixtures/{fixture_name}");
    let encoded_source = URL_SAFE_NO_PAD.encode(source_url.as_bytes());

    // `0` means "not set", matching imgproxy's own rs/resize convention.
    let mut options = vec![format!(
        "rs:fill:{}:{}",
        width.unwrap_or(0),
        height.unwrap_or(0)
    )];
    options.push("el:0".to_string()); // never upscale — the service rejects it anyway

    let signed_path = format!(
        "/{}/{}.{}",
        options.join("/"),
        encoded_source,
        output_format
    );

    let signature = match (
        std::env::var("SIGNING_KEY").ok().filter(|v| !v.is_empty()),
        std::env::var("SIGNING_SALT").ok().filter(|v| !v.is_empty()),
    ) {
        (Some(key), Some(salt)) => {
            emgr::modules::signing::verify::sign(
                key.as_bytes(),
                salt.as_bytes(),
                &signed_path,
            )
        }
        _ => "unsigned".to_string(),
    };

    format!("{target_base_url}/{signature}{signed_path}")
}

// ---- Stats ----------------------------------------------------------------

#[derive(Debug, Default, Serialize)]
struct OutcomeCounts {
    success: usize,
    non_2xx: usize,
    timeout: usize,
    connection_error: usize,
}

impl OutcomeCounts {
    fn total(&self) -> usize {
        self.success + self.non_2xx + self.timeout + self.connection_error
    }
}

#[derive(Debug, Serialize)]
struct LatencyStatsMs {
    mean: f64,
    min: f64,
    max: f64,
    p50: f64,
    p90: f64,
    p99: f64,
    p999: f64,
}

fn percentile_sorted(sorted_ms: &[f64], p: f64) -> f64 {
    if sorted_ms.is_empty() {
        return 0.0;
    }
    let rank = (p * (sorted_ms.len() as f64 - 1.0)).round() as usize;
    sorted_ms[rank.min(sorted_ms.len() - 1)]
}

fn latency_stats(mut samples_ms: Vec<f64>) -> Option<LatencyStatsMs> {
    if samples_ms.is_empty() {
        return None;
    }
    samples_ms.sort_by(|a, b| a.partial_cmp(b).expect("latency sample is not NaN"));
    let mean = samples_ms.iter().sum::<f64>() / samples_ms.len() as f64;
    Some(LatencyStatsMs {
        mean,
        min: samples_ms[0],
        max: samples_ms[samples_ms.len() - 1],
        p50: percentile_sorted(&samples_ms, 0.50),
        p90: percentile_sorted(&samples_ms, 0.90),
        p99: percentile_sorted(&samples_ms, 0.99),
        p999: percentile_sorted(&samples_ms, 0.999),
    })
}

#[derive(Debug, Serialize)]
struct LevelReport {
    concurrency: usize,
    total_requests: usize,
    outcomes: OutcomeCounts,
    total_duration_secs: f64,
    throughput_rps: f64,
    latency_ms: Option<LatencyStatsMs>,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    generated_at_unix_secs: u64,
    target_base_url: String,
    fixture_origin_base_url: String,
    requests_per_level: usize,
    warmup_requests: usize,
    levels: Vec<LevelReport>,
}

fn print_level_report(report: &LevelReport) {
    let o = &report.outcomes;
    println!(
        "  requests: {} (success {}, non-2xx {}, timeout {}, connection error {})",
        report.total_requests, o.success, o.non_2xx, o.timeout, o.connection_error
    );
    println!(
        "  duration: {:.2}s   throughput: {:.2} req/s",
        report.total_duration_secs, report.throughput_rps
    );
    match &report.latency_ms {
        Some(l) => {
            println!(
                "  latency (ms, successes only): mean {:.1}  min {:.1}  p50 {:.1}  p90 {:.1}  p99 {:.1}  p99.9 {:.1}  max {:.1}",
                l.mean, l.min, l.p50, l.p90, l.p99, l.p999, l.max
            );
        }
        None => println!("  latency: no successful requests"),
    }
}

fn write_json_report(path: &str, report: &BenchmarkReport) -> Result<()> {
    let path = std::path::Path::new(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create directory {parent:?}"))?;
        }
    }
    let file = std::fs::File::create(path)
        .with_context(|| format!("failed to create JSON report at {path:?}"))?;
    serde_json::to_writer_pretty(file, report).context("failed to serialize JSON report")?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = BenchmarkConfig::init_from_env()?;
    if let Err(e) = config.validate() {
        eprintln!("Configuration error: {e}");
        return Ok(());
    }

    println!("Image Resize Load-Test Harness");
    println!("===============================");
    println!("Target:              {}", config.get_base_url());
    println!("Concurrency levels:  {:?}", config.get_concurrency_levels());
    println!("Requests per level:  {}", config.requests_per_level);
    println!("Warm-up requests:    {}", config.warmup_requests);
    println!("Fixtures:            {:?}", config.get_fixture_names());
    println!(
        "Resize params:       {} combos",
        config.get_resize_params().len()
    );
    println!("Output format:       {}", config.output_format);
    println!("Request timeout:     {}s", config.request_timeout);
    println!("JSON report:         {}", config.json_output);

    // Local origin serving the fixture corpus - no network dependency.
    let fixture_addr = start_fixture_server(&config).await?;
    let fixture_base_url = format!("http://{fixture_addr}");
    println!("Local fixture origin: {fixture_base_url}");
    println!();

    // ONE shared client for the whole run - reused pooled connections
    // instead of a fresh TCP+TLS handshake per request.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.request_timeout))
        .build()
        .context("failed to build shared reqwest client")?;

    let fixture_names = config.get_fixture_names();
    let resize_params = config.get_resize_params();
    let url_count = fixture_names.len().max(resize_params.len());
    let urls: Vec<String> = (0..url_count)
        .map(|i| {
            let fixture_name = &fixture_names[i % fixture_names.len()];
            let (width, height) = resize_params[i % resize_params.len()];
            build_url(
                &config.get_base_url(),
                &fixture_base_url,
                fixture_name,
                width,
                height,
                &config.output_format,
            )
        })
        .collect();

    let concurrency_levels = config.get_concurrency_levels();
    let mut level_reports = Vec::with_capacity(concurrency_levels.len());

    for concurrency in concurrency_levels {
        println!("=== Concurrency {concurrency} ===");

        if config.warmup_requests > 0 {
            println!(
                "  warming up ({} requests, discarded)...",
                config.warmup_requests
            );
            run_requests(&client, &urls, concurrency, config.warmup_requests).await;
        }

        let (duration, results) =
            run_requests(&client, &urls, concurrency, config.requests_per_level).await;

        let mut outcomes = OutcomeCounts::default();
        let mut latencies_ms = Vec::new();
        for r in &results {
            match r.outcome {
                Outcome::Success => {
                    outcomes.success += 1;
                    latencies_ms.push(r.duration.as_secs_f64() * 1000.0);
                }
                Outcome::NonSuccess => outcomes.non_2xx += 1,
                Outcome::Timeout => outcomes.timeout += 1,
                Outcome::ConnectionError => outcomes.connection_error += 1,
            }
        }

        let total_requests = outcomes.total();
        let throughput_rps = if duration.as_secs_f64() > 0.0 {
            outcomes.success as f64 / duration.as_secs_f64()
        } else {
            0.0
        };

        let report = LevelReport {
            concurrency,
            total_requests,
            latency_ms: latency_stats(latencies_ms),
            outcomes,
            total_duration_secs: duration.as_secs_f64(),
            throughput_rps,
        };
        print_level_report(&report);
        level_reports.push(report);
        println!();

        if config.wait_between_tests > 0 {
            sleep(Duration::from_secs(config.wait_between_tests)).await;
        }
    }

    let generated_at_unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let report = BenchmarkReport {
        generated_at_unix_secs,
        target_base_url: config.get_base_url(),
        fixture_origin_base_url: fixture_base_url,
        requests_per_level: config.requests_per_level,
        warmup_requests: config.warmup_requests,
        levels: level_reports,
    };
    write_json_report(&config.json_output, &report)?;
    println!("JSON report written to {}", config.json_output);

    println!();
    println!("Configuration (env vars):");
    println!("  BENCHMARK_HOST / BENCHMARK_PORT      - target service address");
    println!("  BENCHMARK_CONCURRENCY_LEVELS         - e.g. '1,10,50,100'");
    println!(
        "  BENCHMARK_REQUESTS_PER_LEVEL         - timed requests per level (sample size for percentiles)"
    );
    println!("  BENCHMARK_WARMUP_REQUESTS            - discarded requests run before each level");
    println!(
        "  BENCHMARK_FIXTURE_NAMES              - photo_like,flat,alpha,tiny (see benches/fixtures.rs)"
    );
    println!("  BENCHMARK_RESIZE_PARAMS              - e.g. '100x100,500x,x300'");
    println!("  BENCHMARK_OUTPUT_FORMAT              - jpg|png|webp");
    println!("  BENCHMARK_REQUEST_TIMEOUT            - per-request timeout, seconds");
    println!("  BENCHMARK_WAIT_BETWEEN_TESTS         - pause between levels, seconds");
    println!(
        "  BENCHMARK_FIXTURE_HOST/_PORT         - local origin bind address (0 = random port)"
    );
    println!("  BENCHMARK_JSON_OUTPUT                - path for the machine-readable report");

    Ok(())
}
