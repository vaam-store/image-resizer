# Performance Configuration

Every performance-relevant tunable is set through environment variables, no code changes required. This page is the knobs; for what the mechanisms behind them actually do, see [`PERFORMANCE_OPTIMIZATIONS.md`](../../PERFORMANCE_OPTIMIZATIONS.md); for measured numbers (criterion micro-benchmarks and the end-to-end comparison against imgproxy), see [`.bench-baseline/BASELINE.md`](../../.bench-baseline/BASELINE.md).

## Environment Variables

### Basic Performance Settings

All read by `envconfig` in `src/modules/env/env.rs` and converted into a
`PerformanceConfig` (`src/config/performance.rs`).

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_CONCURRENT_DOWNLOADS` | `20` | Maximum number of concurrent image downloads (`download_semaphore`) |
| `MAX_CONCURRENT_PROCESSING` | CPU count | Maximum number of concurrent decode/resize/encode tasks (`processing_semaphore`, #30) |
| `HTTP_TIMEOUT_SECS` | `30` | HTTP client timeout in seconds |
| `MAX_IMAGE_SIZE_MB` | `50` | Maximum source image size in megabytes, enforced per downloaded chunk (#22) |
| `CPU_THREAD_POOL_SIZE` | CPU count | Advisory only - see the note below |
| `ENABLE_HTTP2` | `true` in code's own `Default`/preset values, but **`false`** in practice when unset - see the note below | Enable HTTP/2 for downloads |
| `CONNECTION_POOL_SIZE` | `50` | Connection pool size per host |
| `KEEP_ALIVE_TIMEOUT_SECS` | `60` | Keep-alive timeout for connections in seconds |

> **`ENABLE_HTTP2` default discrepancy, verified against
> `src/config/performance.rs`:** `PerformanceConfig::default()` and the
> `high_throughput`/`low_latency` presets below all set `enable_http2:
> true`. But the code path actually used at startup with no
> `PERFORMANCE_PROFILE` set - the fallback arm of `impl From<&EnvConfig>
> for PerformanceConfig` - reads `env_config.enable_http2.unwrap_or(false)`.
> A deployment that sets neither `PERFORMANCE_PROFILE` nor `ENABLE_HTTP2`
> therefore runs with HTTP/2 **disabled**, not enabled as every other
> default in this file would suggest. Set `ENABLE_HTTP2=true` explicitly if
> you want it and aren't using one of the profiles below (both of which set
> it via their own struct literal, independent of this fallback).
>
> **`CPU_THREAD_POOL_SIZE` is read and stored (`PerformanceConfig::cpu_thread_pool_size`,
> exposed via `get_cpu_thread_pool_size()`) but nothing in `src/` currently
> calls `get_cpu_thread_pool_size()`** - CPU-bound work runs on Tokio's own
> blocking-task pool via `spawn_blocking`, gated by `MAX_CONCURRENT_PROCESSING`'s
> semaphore, not a separately-sized pool. Setting this variable is
> harmless but currently has no effect; `MAX_CONCURRENT_PROCESSING` is the
> variable that actually bounds CPU concurrency.

### Router-level saturation and rate limiting

A second, independent set of knobs bounds total request concurrency and
per-IP request rate at the router (`src/modules/router/middlewares.rs`,
#43) - separate from `MAX_CONCURRENT_PROCESSING` above, which only bounds
the CPU-bound stage of an already-admitted request. These are read
directly from the process environment (not via `envconfig`/`EnvConfig`),
so `docs-env-drift` CI's `EnvConfig`-vs-`.env.example` check does not cover
them - they are documented here instead.

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_CONCURRENT_REQUESTS` | `512` | Total requests handled concurrently across all routes. Beyond this, a request is shed with `503` immediately rather than queued. |
| `REQUEST_TIMEOUT_SECS` | `30` | Hard ceiling on end-to-end request time; a request still running past this is shed with `503`. |
| `RATE_LIMIT_BURST` | `20` | Per-IP token-bucket burst size before rate limiting engages. |
| `RATE_LIMIT_PERIOD_MS` | `100` | Per-IP token-bucket refill period (default: one additional request roughly every 100ms, i.e. ~10 req/s sustained per IP). |

A `0` value for any of the four is treated as unset (falls back to the
default above) rather than as a literal zero limit.

### Performance Profiles

You can use predefined performance profiles by setting the `PERFORMANCE_PROFILE` environment variable:

| Profile | Description |
|---------|-------------|
| `high_throughput` | Optimized for maximum throughput with higher resource usage |
| `low_latency` | Optimized for minimal response time |
| `memory_efficient` | Optimized for minimal memory usage |

When using a profile, individual environment variables will override the profile defaults.

## Examples

### Basic Configuration

```bash
# Set custom download limits
export MAX_CONCURRENT_DOWNLOADS=50
export HTTP_TIMEOUT_SECS=15

# Start the service
./emgr
```

### Using Performance Profiles

```bash
# Use high throughput profile
export PERFORMANCE_PROFILE=high_throughput

# Override specific settings
export MAX_CONCURRENT_DOWNLOADS=100

# Start the service
./emgr
```

### Memory-Constrained Environment

```bash
# Use memory efficient profile
export PERFORMANCE_PROFILE=memory_efficient

# Further reduce memory usage
export MAX_CONCURRENT_DOWNLOADS=3
export MAX_IMAGE_SIZE_MB=10

# Start the service
./emgr
```

## Profile Details

### High Throughput Profile
- `MAX_CONCURRENT_DOWNLOADS`: 50
- `MAX_CONCURRENT_PROCESSING`: CPU count × 2
- `HTTP_TIMEOUT_SECS`: 15
- `MAX_IMAGE_SIZE_MB`: 100
- `CPU_THREAD_POOL_SIZE`: CPU count
- `ENABLE_HTTP2`: true
- `CONNECTION_POOL_SIZE`: 100
- `KEEP_ALIVE_TIMEOUT_SECS`: 120

### Low Latency Profile
- `MAX_CONCURRENT_DOWNLOADS`: 10
- `MAX_CONCURRENT_PROCESSING`: CPU count
- `HTTP_TIMEOUT_SECS`: 10
- `MAX_IMAGE_SIZE_MB`: 20
- `CPU_THREAD_POOL_SIZE`: CPU count
- `ENABLE_HTTP2`: true
- `CONNECTION_POOL_SIZE`: 25
- `KEEP_ALIVE_TIMEOUT_SECS`: 30

### Memory Efficient Profile
- `MAX_CONCURRENT_DOWNLOADS`: 5
- `MAX_CONCURRENT_PROCESSING`: CPU count ÷ 2
- `HTTP_TIMEOUT_SECS`: 45
- `MAX_IMAGE_SIZE_MB`: 10
- `CPU_THREAD_POOL_SIZE`: CPU count ÷ 2
- `ENABLE_HTTP2`: false (HTTP/1.1 uses less memory)
- `CONNECTION_POOL_SIZE`: 10
- `KEEP_ALIVE_TIMEOUT_SECS`: 30

## Monitoring and Tuning

Monitor your application's performance metrics to determine optimal settings:

- **CPU usage** - adjust `MAX_CONCURRENT_PROCESSING` and `CPU_THREAD_POOL_SIZE`
- **Memory usage** - adjust `MAX_CONCURRENT_DOWNLOADS` and `MAX_IMAGE_SIZE_MB`
- **Network performance** - adjust `CONNECTION_POOL_SIZE` and `ENABLE_HTTP2`
- **Response times** - adjust `HTTP_TIMEOUT_SECS` and `KEEP_ALIVE_TIMEOUT_SECS`

Start with a profile that matches your use case, then fine-tune individual parameters based on your specific requirements and monitoring data.

## Benchmark Configuration

The included benchmark tool is now fully configurable through environment variables, allowing you to customize performance testing for your specific environment and requirements.

### Benchmark Environment Variables

Source of truth: `src/bin/benchmark.rs`'s `BenchmarkConfig`.

| Variable | Default | Description |
|----------|---------|-------------|
| `BENCHMARK_HOST` | `localhost` | Target host of the service under test |
| `BENCHMARK_PORT` | `3000` | Target port of the service under test |
| `BENCHMARK_CONCURRENCY_LEVELS` | `1,5,10,20,50` | Comma-separated list of concurrency levels to test |
| `BENCHMARK_REQUESTS_PER_LEVEL` | `200` | Timed requests run at each concurrency level - the sample size percentiles (especially p99.9) are computed from |
| `BENCHMARK_WARMUP_REQUESTS` | `10` | Requests run and discarded before each level is timed, so every level is measured warm |
| `BENCHMARK_FIXTURE_NAMES` | `photo_like,flat,alpha,tiny` | Comma-separated list of generated fixtures (see `benches/fixtures.rs`) to rotate through as source images |
| `BENCHMARK_RESIZE_PARAMS` | `300x300,800x,x600,1200x800` | Comma-separated list of resize parameters (format: `WIDTHxHEIGHT`) |
| `BENCHMARK_WAIT_BETWEEN_TESTS` | `2` | Seconds to wait between different concurrency level tests |
| `BENCHMARK_REQUEST_TIMEOUT` | `60` | Per-request timeout in seconds |
| `BENCHMARK_OUTPUT_FORMAT` | `jpg` | Output image format for resize requests (`jpg`\|`png`\|`webp`\|`avif`\|`gif`) |
| `BENCHMARK_FIXTURE_HOST` | `127.0.0.1` | Bind address of the local fixture-serving HTTP origin the benchmark starts inside its own process |
| `BENCHMARK_FIXTURE_PORT` | `0` (random free port) | Port for the local fixture origin above |
| `BENCHMARK_JSON_OUTPUT` | `target/benchmark-results.json` | Path to write the machine-readable JSON report to, alongside the human-readable table |

The benchmark no longer fetches source images over the network. It used to
pull real photos from `picsum.photos` via a `BENCHMARK_TEST_URLS` variable;
that variable is gone. Test images are now served from a tiny local axum
server this binary starts inside its own process, using the same
deterministic, generated fixture corpus the criterion benches use
(`benches/fixtures.rs`) - so the "origin" the target service downloads from
adds no internet variance to the results, and a benchmark run needs no
network access at all. Redirects are still followed end-to-end (the target
service's `301` to its own storage-backed URL, `reqwest`'s default policy),
so a benchmark run against a real deployment still measures the full
download → process → upload → redirect → re-fetch path, not just the
redirect hop.

### Resize Parameters Format

The `BENCHMARK_RESIZE_PARAMS` variable accepts parameters in the format `WIDTHxHEIGHT`:
- `300x300` - Resize to 300x300 pixels
- `800x` - Resize to 800 pixels width, maintain aspect ratio
- `x600` - Resize to 600 pixels height, maintain aspect ratio
- `1200x800` - Resize to 1200x800 pixels

### Benchmark Examples

#### Basic Benchmark
```bash
# Run benchmark with default settings
cargo run --bin benchmark
```

#### Custom Target Server
```bash
# Test against a different server
export BENCHMARK_HOST=production-server.com
export BENCHMARK_PORT=443
cargo run --bin benchmark
```

#### High Concurrency Testing
```bash
# Test with higher concurrency levels
export BENCHMARK_CONCURRENCY_LEVELS=1,10,25,50,100,200
export BENCHMARK_REQUEST_TIMEOUT=60
cargo run --bin benchmark
```

#### Custom Fixtures And Output Format
```bash
# Choose which generated fixtures to rotate through (see benches/fixtures.rs
# for the full set) and a different output format
export BENCHMARK_FIXTURE_NAMES="photo_like,alpha"
export BENCHMARK_RESIZE_PARAMS="100x100,500x500,1000x,x800"
export BENCHMARK_OUTPUT_FORMAT=webp
cargo run --bin benchmark
```

#### Quick Performance Check
```bash
# Fast benchmark for CI/CD pipelines
export BENCHMARK_CONCURRENCY_LEVELS=1,5,10
export BENCHMARK_WAIT_BETWEEN_TESTS=1
export BENCHMARK_REQUEST_TIMEOUT=15
cargo run --bin benchmark
```

### Benchmark Output

For each concurrency level, the benchmark prints:
- **Request outcomes** - counts broken down by success, non-2xx, timeout, and connection error, not a single pass/fail count
- **Duration and throughput** - total wall-clock time for the level and the resulting requests/sec
- **Latency percentiles (successes only)** - mean, min, max, p50, p90, p99, and p99.9, in milliseconds

It also writes a machine-readable JSON report (`BENCHMARK_JSON_OUTPUT`, default `target/benchmark-results.json`) with the same data, so CI can diff runs over time.

### Integration with Performance Profiles

You can combine benchmark configuration with performance profiles to test different server configurations:

```bash
# Test high throughput profile
export PERFORMANCE_PROFILE=high_throughput
./emgr &

# Run benchmark against high throughput configuration
export BENCHMARK_CONCURRENCY_LEVELS=10,50,100,200
export BENCHMARK_REQUEST_TIMEOUT=60
cargo run --bin benchmark

# Stop server and test memory efficient profile
killall emgr
export PERFORMANCE_PROFILE=memory_efficient
./emgr &

# Run benchmark with lower concurrency
export BENCHMARK_CONCURRENCY_LEVELS=1,5,10
cargo run --bin benchmark
```

This configurable approach allows you to:
1. **Test different environments** - development, staging, production
2. **Validate performance profiles** - ensure profiles meet your requirements
3. **Automate performance testing** - integrate into CI/CD pipelines
4. **Custom load patterns** - simulate your specific usage patterns