# Components

This page maps every real module under `src/` to what it does. It is a companion to
`overview.md`, which covers the request flow, the redirect-based delivery trade-off, and the two
required diagrams. Every module and file path below was checked against the code on `main`,
not assumed from names.

## Entry point

### `src/main.rs`

Builds the Tokio runtime by hand (not `#[tokio::main]`) so `worker_threads` can be a runtime
value read from `TOKIO_WORKER_THREADS`, cgroup-aware via `modules::utils::cgroup::effective_cpu_count`
rather than `num_cpus::get()` directly (a cgroup-limited container otherwise oversizes the
runtime for the host's full core count). Sets `mimalloc::MiMalloc` as the global allocator.
Installs a SIGTERM/SIGINT-triggered graceful shutdown with a configurable drain deadline
(`SHUTDOWN_TIMEOUT_SECS`, default 20s — comfortably under Kubernetes' default 30s
`terminationGracePeriodSeconds`).

### `src/bin/healthcheck.rs`, `src/bin/benchmark.rs`

Two extra binaries: a standalone healthcheck probe, and a load-generating benchmark client
(serves fixture images locally and drives the resize endpoint against them — see
`.bench-baseline/BASELINE.md` for how its output feeds the performance numbers cited in
`overview.md`).

## Router and middleware — `src/modules/router/`

- **`router.rs`** — hand-written route table (`build_app`): `GET /health` (unauthenticated,
  Kubernetes probe target), `GET /api/images/files/{key}` (unsigned download route),
  `GET /{signature}/{*rest}` (the imgproxy-compatible signed resize route, mounted at the root —
  axum/matchit prefers a literal path segment like `api`/`health` over this route's dynamic first
  segment, so it never shadows the others), and, only when built with `--features otel`,
  `GET /metrics` behind `metrics_auth::require_metrics_auth`. There is no `gen_server`-generated
  router anywhere in this file or this codebase — see "Removed" below.
- **`middlewares.rs`** — `MiddlewareConfig` (env-driven: `REQUEST_TIMEOUT_SECS`,
  `MAX_CONCURRENT_REQUESTS`, `RATE_LIMIT_BURST`, `RATE_LIMIT_PERIOD_MS`) plus
  `apply_common_middlewares`, which layers, outer to inner: CORS, `tower_governor`'s per-IP
  token-bucket `GovernorLayer` (429 on excess — its per-key state is pruned every 60s via
  `limiter.retain_recent()` so it doesn't grow unbounded over the process lifetime), a conditional-
  GET middleware for the download route (`ETag`/`If-None-Match`/`304`, computed directly from the
  requested key without touching storage), a `Semaphore`-backed saturation/timeout guard (503 on
  either), and `tower-http`'s `CompressionLayer` (br/deflate/gzip/zstd) closest to the app.

## API handlers — `src/modules/api/`

- **`handler.rs`** — `ApiService`, the shared application state: `resize_service`, `signing`,
  `presets`/`allowed_options` (issue #52), and (under `otel`) `metrics_auth`. `ApiService::create`
  builds every sub-config from `EnvConfig` and fails closed at startup for signing, `/metrics` auth,
  and preset parsing — a misconfiguration is a boot-time error, not a per-request surprise.
- **`resize.rs`** — `GET /{signature}/{processing_options}/{plain|base64 source}.{extension}`.
  Splits the path (`modules::url::split`), verifies the signature, parses options+source
  (`SignedRequest::parse_with_config`), resolves `.auto` via `modules::negotiation::resolve`, calls
  `ResizeService::resize`, and builds an explicit `301` (not axum's `Redirect::permanent`, which
  issues `308`) with `Cache-Control: public, max-age=31536000, immutable` and, only for a negotiated
  `.auto` request, `Vary: Accept`.
- **`download.rs`** — `GET /api/images/files/{key}`, deliberately unsigned (this route only ever
  serves bytes the resize route already produced and cached under a content-derived key; the strict
  key grammar in `services::storage::key_validation` is the guard here, not a signature). Derives
  `Content-Type` from the key's own extension.

## URL grammar and signing

### `src/modules/url/`

Implements the imgproxy-compatible signed-URL grammar
`/{signature}/{processing_options}/{plain|base64 source}.{extension}`:

- **`mod.rs`** — `split` (cheap extraction of the signature segment and the exact signed byte
  string, run before any full parse so an unauthenticated caller can't use parse-error content as
  an oracle) and `SignedRequest::parse`/`parse_with_config` (full options+source grammar, presets
  and the processing-option allowlist applied ahead of the plain grammar parse).
- **`options.rs`** — `ProcessingOptions`, parsing `/`-delimited `code:arg1:arg2` segments (`rs`,
  `q`, `bl`, `g`, `el`, `fq`, `webpo`, crop/gravity, rotate/flip/trim/extend/padding/zoom/dpr/
  min-width/min-height, watermark, preset references).
- **`presets.rs`** — `PresetRegistry` (parses the `PRESETS` env var: named, reusable option-segment
  lists, imgproxy-compatible; a preset named `default` is auto-prepended to every request) and
  `AllowedOptions` (parses `ALLOWED_PROCESSING_OPTIONS`, an allowlist of which option codes a
  deployment permits at the top level of a URL).
- **`source.rs`** — `SourceSpec`/`parse_source`: either `plain/{percent-encoded URL}.{extension}`
  or a single base64url-encoded segment, with the trailing `.{extension}` mandatory and mapped to
  `ImageFormat` (including the `.auto` negotiation trigger).

### `src/modules/signing/`

- **`verify.rs`** — `compute_signature`/`verify_signature`/`sign`: `HMAC-SHA256(key, salt ||
  signed_path)`, base64url (no padding), constant-time comparison via the `subtle` crate.
- **`config.rs`** — `SigningConfig::from_env`, which fails closed at startup (issue #27): a
  deployment must either configure a real key/salt or explicitly set
  `ALLOW_UNSIGNED_REQUESTS=true` to use the `/unsigned/...` escape hatch.

### `src/modules/negotiation.rs`

`resolve(format, accept)` — resolves `ImageFormat::Auto` against the request's `Accept` header,
preferring AVIF, then WebP, falling back to JPEG, weighted by each entry's `q` parameter. Any
non-`Auto` format passes through unchanged with `negotiated = false`.

### `src/modules/metrics_auth/`

Bearer-token authentication for `GET /metrics` (issue #77), gated behind the `otel` feature since
that's the only build where `/metrics` is ever mounted at all. `MetricsAuthConfig::from_env` fails
closed at startup, mirroring `SigningConfig`.

## Services — `src/services/`

### `resize/handler.rs` — `ResizeService`

The orchestrator tying cache, image processing, and storage together:

- `resize(params)` — generates the cache key, checks the cache, and on a miss runs
  single-flight coalescing before calling `do_resize_work`. See `overview.md`'s "Concurrency
  model" and its cache-miss sequence diagram for the full mechanics of `InFlightMap`/`InFlightGuard`
  (issue #37).
- `do_resize_work` — the actual download → process → upload pipeline for a confirmed miss.
- `download(params)` — serves the unsigned `/api/images/files/{key}` route by reading directly
  from storage; a missing/expired key surfaces as a "not found" message that
  `AppError::classify_download_error` maps to `404`.
- `resize_batch` — bounded-concurrency batch resizing over `futures::stream::buffer_unordered`.

### `image/handler.rs` — `ImageService`

Owns the two semaphores (`download_semaphore`, `processing_semaphore`), the SSRF-guarded fetch
path, and the decode/resize/encode pipeline:

- `fetch_validated`/`download_image` — see `overview.md`'s "Security boundary" section for the
  SSRF guard itself (implemented in `source_guard.rs`, described next). `download_image` also
  enforces the streaming size cap.
- `process_image` — acquires `processing_semaphore` (non-blocking `try_acquire_owned`, `503` on
  exhaustion) and runs decode/resize/encode on `spawn_blocking`, with a `tx.is_closed()` check to
  skip queued-but-not-yet-started work when the caller has already disconnected.
- `process_image_blocking_with_limits*` — header-only dimension peek and resolution check
  (decompression-bomb guard) before any full decode; EXIF autorotate, trim, and explicit crop are
  applied in that order (matching imgproxy's own pipeline ordering) before the actual resize;
  animated GIF/WebP sources are detected and routed to `encode_animation` separately.
- `decode_with_limits`/`decode_jpeg_scaled` — mozjpeg-first JPEG decode (DCT-scaled when a resize
  makes a smaller decode safe), falling back to the `image`-crate decoder (`decode_with_image_crate`,
  also the only path for PNG/WebP) on any mozjpeg failure.
- `encode_with_max_bytes` — binary-searches JPEG quality down until encoded output fits a
  requested `max_bytes` budget (issue #76), bounded to a fixed number of extra encode attempts.

### `image/source_guard.rs`

Pure, deterministic SSRF validation logic (the one exception is `resolve_validated_addr`, which
performs the actual DNS lookup): `validate_scheme`, `is_allowed_source`/`matches_allowed_prefix`,
`is_blocked_ip_with_policy` (and its IPv4/IPv6 halves), and IPv4-literal decoding covering
decimal/octal/hex smuggling forms. See `overview.md`'s "Security boundary" for the full threat
model this covers and why redirect revalidation and DNS pinning are both necessary.

### `cache/handler.rs` — `CacheService`

`generate_key(params)` — the SHA-256, length-prefixed, versioned cache key described in
`overview.md`'s "Cache key design". `CACHE_KEY_VERSION` is currently `9`; its doc comment on this
file is the authoritative history of what each version bump added and, in one case (issue #67),
why a bump was deliberately *not* taken.

### `storage/`

- **`core.rs`** — the `StorageBackend` trait: `upload_image`/`upload_image_with_ttl`,
  `check_cache`, `get_image`, `delete`. `ttl: None` means "never expires."
- **`handler.rs`** — `StorageService`/`StorageConfig`/`StorageType`, selecting a backend by Cargo
  feature and (when more than one is compiled in) `STORAGE_TYPE`. Also enforces that a configured
  `key_prefix` (from `STORAGE_SUB_PATH`) matches what `CacheService::generate_key` actually
  produces.
- **`s3_handler.rs`** (feature `s3`) — S3/MinIO-compatible backend via `aws-sdk-s3`.
- **`local_fs_handler.rs`** (feature `local_fs`) — local-directory backend.
- **`in_memory_handler.rs`** (feature `in_memory`, test-only) — unbounded `HashMap`-backed
  backend, `#[cfg(all(test, feature = "in_memory"))]` — does not exist in a release build at all,
  regardless of the Cargo feature flag; selecting it via `STORAGE_TYPE` outside tests fails fast at
  startup instead of running an uncapped cache in production.
- **`key_validation.rs`** — `validate_cache_key`, the strict grammar
  (`<prefix><64 lowercase hex>.<jpg|png|webp|avif|gif>`) every backend is checked against before a
  key ever reaches it, closing path-traversal and S3 IDOR in one place rather than per backend.

### `health/handler.rs`

`health()` — returns the literal string `"OK"`. Deliberately unauthenticated; see
`overview.md`'s "Security boundary" for why.

### `metrics/handler.rs`

`metrics_handler()` — encodes the global Prometheus registry (`prometheus::gather()`) as text.
Only compiled/mounted under the `otel` feature; protected by `modules::metrics_auth` at the router
layer, not in this handler itself.

## Models — `src/models/params.rs`

`ResizeQuery` (the fully-parsed representation of a resize request — url, width/height,
`ResizeType`, `ImageFormat`, quality/format-quality overrides, crop/gravity, geometry operations,
watermark, etc.) and `DownloadPathParams` (`{ key: String }`, hand-written — see "Removed" below).

## Configuration

- **`src/modules/env/env.rs`** — `EnvConfig`, the `envconfig`-derived top-level environment
  binding (HTTP host/port, storage credentials, `MAX_IMAGE_SIZE_MB`, `MAX_SRC_RESOLUTION_MP`,
  `ALLOWED_SOURCES`, `ALLOW_LOOPBACK_SOURCE_ADDRESSES`/`ALLOW_LINK_LOCAL_SOURCE_ADDRESSES`,
  `max_redirects`, presets/allowlist strings, etc.).
- **`src/config/performance.rs`** — `PerformanceConfig`, derived from `EnvConfig` plus a small set
  of named default profiles (default/high-throughput/low-latency/memory-constrained), each setting
  `max_image_size`, `max_src_resolution_mp`, `max_redirects`, download/processing concurrency
  limits, and the SSRF-guard override flags.

## Utility modules — `src/modules/utils/`

- **`date.rs`** — a minimal hand-rolled RFC 7231 `IMF-fixdate` formatter (no date/time crate is a
  dependency of this crate), used for the download route's `Last-Modified` header.
- **`etag.rs`** — `if_none_match_satisfied`, RFC 7232 §3.2 strong-comparison matching against the
  single server-computed `ETag` the conditional-download middleware produces.
- **`err.rs`** — `AppError` and its `classify_download_error`/`classify_resize_error`
  constructors, mapping internal failures to HTTP status codes (see `overview.md`'s rejection-state
  diagram for the concrete mapping). Every variant renders with `Cache-Control: no-store`.
- **`cgroup.rs`** — `effective_cpu_count`, reading cgroup v2 `cpu.max` or cgroup v1
  `cpu.cfs_quota_us`/`cpu.cfs_period_us` directly (falling back to `num_cpus::get()`), used by
  `main.rs` to size the Tokio runtime.

## Tracing — `src/modules/tracer/`

OpenTelemetry tracing/metrics initialization (`init.rs`), compiled only under the `otel` feature.

## Removed since the last documentation pass

The following existed in an earlier version of this service and no longer do. They are recorded
here only as history, not as current architecture:

- **The generated OpenAPI server** (`gen-server`, a `packages/` directory, `openapi.yaml`). Issue
  #53 replaced it with the hand-written router in `src/modules/router/router.rs` and the
  hand-written handlers in `src/modules/api/`. Verified: `grep -rc 'gen-server\|gen_server\|openapi'
  Cargo.toml` and `find . -iname 'openapi.yaml'` both return nothing in this tree.
- **`rayon`** — the old CPU-bound processing pool. Replaced by `tokio::task::spawn_blocking`
  bounded by `processing_semaphore`; see `image/handler.rs`'s doc comment on `process_image` for
  why rayon's intra-job work-stealing was never actually used. Not a dependency in `Cargo.toml`.
- **`o2o`**, **`lru`**, **`axum-extra`** — none appear in `Cargo.toml`'s dependency list; no code
  under `src/` references them.
