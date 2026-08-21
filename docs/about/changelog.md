# Changelog

The previous version of this page was a 20-line template that predated
essentially all of the functionality below. This reconstruction groups
the project's `git log --oneline --merges origin/main` history by theme
rather than listing every commit; PR numbers (`#NN`) link to
`https://github.com/vaam-store/image-resizer/pull/NN`.

`emgr` doesn't cut dated, tagged releases yet - `Cargo.toml`'s `version`
has stayed `0.1.2` throughout everything below, and
`.github/workflows/build.yml` has no release-tag trigger: every published
Docker image is either a floating `<flavor>-latest` or a per-commit
`<flavor>-<sha>` (see [Docker deployment](../deployment/docker.md)).
Once real, tagged releases start, this file should switch to genuine
dated version headings per [Keep a Changelog](https://keepachangelog.com/en/1.0.0/);
until then, everything below is "Unreleased" in that format's sense.

## ⚠️ Breaking changes for existing deployments

- **Signed URLs are on by default** ([#27](https://github.com/vaam-store/image-resizer/issues/27)).
  A deployment that previously ran without `SIGNING_KEY`/`SIGNING_SALT`
  now refuses to start. Set both (hex-encoded), or set
  `ALLOW_UNSIGNED_REQUESTS=true` to keep the old, unsigned behavior.
- **`/metrics` now requires authentication on `otel` builds**
  ([#77](https://github.com/vaam-store/image-resizer/issues/77)). A
  deployment scraping `/metrics` without configuring
  `METRICS_AUTH_TOKEN` now refuses to start. Set it and point
  Prometheus's scrape config at it via its `authorization` block, or set
  `ALLOW_UNAUTHENTICATED_METRICS=true`.
- **OpenAPI code generation is gone**
  ([#53](https://github.com/vaam-store/image-resizer/issues/53)).
  `openapi.yaml` and the generated `packages/gen-server` crate no longer
  exist - the HTTP router is hand-written. The wire API is unchanged, but
  any tooling that depended on the generated crate or the spec file needs
  updating. `make init`, the codegen bootstrap that used to be a
  prerequisite for a fresh clone, is gone with it.
- **Resize types honour imgproxy's real semantics instead of always
  cropping** ([#59](https://github.com/vaam-store/image-resizer/issues/59),
  [#1](https://github.com/vaam-store/image-resizer/issues/1)). A request
  using a resize type other than `fill` now gets a different result than
  before.

## Security

- SSRF-guarded source fetching: scheme validation, private/loopback/
  link-local IP-range blocking (each with an explicit opt-in), a source
  allowlist (`ALLOWED_SOURCES`), and re-validation on every redirect hop -
  not just the original URL
  ([#21](https://github.com/vaam-store/image-resizer/issues/21)).
- `ALLOWED_SOURCES` fixed to actually authorise the private origins it
  lists, instead of being silently overridden by the private-range guard
  ([#57](https://github.com/vaam-store/image-resizer/issues/57)).
- HMAC-SHA256 signed URLs, imgproxy-compatible, on by default
  ([#27](https://github.com/vaam-store/image-resizer/issues/27)).
- Cache-key validation shared by every storage backend, closing an
  arbitrary-file-read via an unvalidated key (traversal, absolute paths,
  percent-decoded forms) - see `tests/storage_key_validation.rs`
  ([#23](https://github.com/vaam-store/image-resizer/issues/23)).
- Resolution and output-size limits (`MAX_SRC_RESOLUTION_MP`,
  `MAX_OUTPUT_WIDTH`/`MAX_OUTPUT_HEIGHT`, `MAX_ANIMATION_FRAMES`) -
  guards against decode bombs and many-tiny-frame animation bombs
  ([#26](https://github.com/vaam-store/image-resizer/issues/26)).
- `/metrics` bearer-token authentication, fail-closed at startup on
  `otel` builds, mirroring the signed-URL check
  ([#77](https://github.com/vaam-store/image-resizer/issues/77)).
- `cargo-deny` wired into CI: RUSTSEC advisory scanning, license checks,
  duplicate/banned-crate checks, source-registry restriction (epic
  [#9](https://github.com/vaam-store/image-resizer/issues/9)).
- Alpha-channel compositing and transparent-pixel normalization fixed for
  target formats without alpha support
  ([#34](https://github.com/vaam-store/image-resizer/issues/34),
  [#60](https://github.com/vaam-store/image-resizer/issues/60)).
- Base container images (Rust builder, distroless runtime) pinned by
  digest rather than a floating tag
  ([#48](https://github.com/vaam-store/image-resizer/issues/48)).

## Performance

- SIMD image resampling via `fast_image_resize`, replacing the `image`
  crate's scalar resampler (`#63` stage 1).
- DCT-scaled JPEG decode via mozjpeg/libjpeg-turbo: decodes close to the
  requested output size directly, instead of decoding full-size and
  downsampling afterward (`#63` stage 2).
- Full-size JPEG decode also routed through mozjpeg - measured ~1.5x
  faster than the `image` crate's own decoder even without DCT scaling
  ([#67](https://github.com/vaam-store/image-resizer/issues/67)).
- JPEG encoding cut over to mozjpeg/libjpeg-turbo's `Compress`, replacing
  the `image` crate's baseline encoder, alongside progressive-JPEG and
  chroma-subsampling controls
  ([#76](https://github.com/vaam-store/image-resizer/issues/76)).
- CPU-bound image processing moved onto `tokio::spawn_blocking`, keeping
  the async runtime responsive under load; a configurable performance
  profile system (`PERFORMANCE_PROFILE`: high-throughput / low-latency /
  memory-efficient) and concurrency limits (epic
  [#9](https://github.com/vaam-store/image-resizer/issues/9)).
- Cgroup-aware CPU-count detection for Tokio worker-thread and
  concurrency sizing, instead of trusting the host's full core count from
  inside a resource-limited container
  ([#44](https://github.com/vaam-store/image-resizer/issues/44)).
- A criterion micro-benchmark suite (`benches/`) plus a three-way
  end-to-end harness against imgproxy (`bench-imgproxy/`), wired into a
  CI regression gate with a 15% threshold and a PR comment report
  ([#20](https://github.com/vaam-store/image-resizer/issues/20)).

## Formats and processing options

- Lossy and lossless WebP output, plus AVIF output
  ([#35](https://github.com/vaam-store/image-resizer/issues/35),
  [#4](https://github.com/vaam-store/image-resizer/issues/4),
  [#49](https://github.com/vaam-store/image-resizer/issues/49)).
- Animated GIF/WebP output, with a configurable frame-count cap
  ([#49](https://github.com/vaam-store/image-resizer/issues/49)).
- EXIF auto-orientation applied and ICC colour profiles forwarded to the
  output ([#33](https://github.com/vaam-store/image-resizer/issues/33),
  [#5](https://github.com/vaam-store/image-resizer/issues/5)).
- Per-format quality control
  ([#35](https://github.com/vaam-store/image-resizer/issues/35)).
- Progressive JPEG and chroma-subsampling toggles, plus a `max_bytes`
  output-size cap
  ([#76](https://github.com/vaam-store/image-resizer/issues/76)).
- imgproxy-compatible resize types (`fill`, `fit`, and the rest) instead
  of always cropping regardless of the requested type
  ([#59](https://github.com/vaam-store/image-resizer/issues/59)).
- Gravity, crop and geometry options; watermarks; named presets
  (`PRESETS`) and a processing-option allowlist
  (`ALLOWED_PROCESSING_OPTIONS`)
  ([#49](https://github.com/vaam-store/image-resizer/issues/49),
  [#50](https://github.com/vaam-store/image-resizer/issues/50),
  [#51](https://github.com/vaam-store/image-resizer/issues/51),
  [#52](https://github.com/vaam-store/image-resizer/issues/52)).

## Architecture and reliability

- OpenAPI code generation removed: the HTTP router
  (`src/modules/api`, `src/modules/router`, `src/modules/url`) is
  hand-written now, so a fresh clone builds with plain `cargo build` and
  no Docker/codegen step
  ([#53](https://github.com/vaam-store/image-resizer/issues/53)).
- Cache lifecycle grew TTL support and reachable stale/evicted states,
  and local-filesystem writes became atomic (a directory could
  previously be mistaken for a cache hit, and a partial write could be
  served) - see `tests/storage_local_fs_atomicity.rs`
  ([#38](https://github.com/vaam-store/image-resizer/issues/38),
  [#40](https://github.com/vaam-store/image-resizer/issues/40)).
- Graceful shutdown: SIGTERM/SIGINT drain in-flight requests before exit,
  and OpenTelemetry providers flush on exit instead of losing buffered
  telemetry on every restart
  ([#42](https://github.com/vaam-store/image-resizer/issues/42)).
- Docker images are now actually run-tested in CI before being pushed,
  after the `s3`/`s3_otel` images shipped completely unable to start for
  a period (a glibc mismatch between the Rust builder and the distroless
  runtime - see [Docker deployment](../deployment/docker.md#the-builderruntime-base-image-pin---do-not-bump-casually))
  ([#62](https://github.com/vaam-store/image-resizer/issues/62)).

## Tooling and CI

- A real CI pipeline: a per-feature-set test matrix (`local_fs`, `s3`,
  `local_fs,otel`), Clippy, `cargo-deny`, and the benchmark regression
  gate above - previously only a Docker build and a linter ran at all
  (epic [#9](https://github.com/vaam-store/image-resizer/issues/9),
  [#46](https://github.com/vaam-store/image-resizer/issues/46)).
- A docs/env-var drift check (`.github/scripts/check_env_docs.py`) fails
  CI if `src/modules/env/env.rs` and
  [Configuration](../getting-started/configuration.md) disagree in
  either direction ([#47](https://github.com/vaam-store/image-resizer/issues/47)).
- A Knative Serverless Helm chart (`helm/serverless/`) added alongside
  the Deployment-based chart (`helm/emgr/`), for scale-to-zero setups.
- A `PodDisruptionBudget` and liveness/readiness/startup probes added to
  the Deployment Helm chart - previously a voluntary disruption could
  evict every replica at once, and a wedged container was never
  restarted ([#48](https://github.com/vaam-store/image-resizer/issues/48)).
- ADRs recording the image-engine choice, the URL/API shape, and
  measurement writeups for the WebP and AVIF encoder decisions
  (`adr/0001` through `adr/0004`).
