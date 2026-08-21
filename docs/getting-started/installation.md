# Installation

This guide gets `emgr` running locally - from a fresh clone, through the two
startup checks that fail closed by design, to a working `/health` response.

## Prerequisites

- A stable Rust toolchain. `Cargo.toml` sets `edition = "2024"`, which needs
  Rust 1.85 or newer; CI builds against whatever `dtolnay/rust-toolchain@stable`
  currently resolves to, so there's no lower pin to target beyond that.
- `nasm`, **only when building on x86_64** (Linux/macOS amd64). It's needed
  by `mozjpeg-sys` for libjpeg-turbo's x86 SIMD path. aarch64 (Apple
  Silicon, arm64 Linux) doesn't need it - it uses its own NEON path. If
  you're on x86_64 and skip it, the build either fails outright or falls
  back to scalar C, quietly losing most of the mozjpeg decode speedup.
- [Docker](https://docs.docker.com/get-docker/) and
  [Docker Compose](https://docs.docker.com/compose/) v2 (optional - see
  [Docker deployment](../deployment/docker.md)).
- [Kubernetes](https://kubernetes.io/docs/setup/) and
  [Helm](https://helm.sh/docs/intro/install/) 3 (optional - see the
  [Helm chart](../deployment/helm-chart.md) guide).

## Clone the repository

```bash
git clone https://github.com/vaam-store/image-resizer.git
cd image-resizer
```

A fresh clone builds with plain `cargo build` - no code generation step,
no Docker step. `make init` (an old OpenAPI-codegen bootstrap) and the
`openapi.yaml`/`packages/gen-server` it drove no longer exist: the HTTP
router in `src/modules/api`, `src/modules/router` and `src/modules/url` is
hand-written (removed in [GH #53](https://github.com/vaam-store/image-resizer/issues/53)).

## Pick a feature set

`Cargo.toml` sets `default = []`. That's deliberate, but it means a bare
`cargo build` compiles fine and then the binary refuses to start:

```text
Error: No storage features are enabled
```

Every build must name at least one storage backend explicitly, from
`Cargo.toml`'s `[features]`:

| Feature | What it does |
|---|---|
| `local_fs` | Store resized images on local disk. No external dependency - the easiest way to get started. |
| `s3` | Store resized images in S3 or an S3-compatible service (MinIO). Needs a reachable endpoint - see [Docker deployment](../deployment/docker.md) for a MinIO-backed `compose.yaml` setup. |
| `in_memory` | An in-process cache. Only reachable in this crate's own test builds ([GH #39](https://github.com/vaam-store/image-resizer/issues/39)) - selecting it in a release build falls through to the same "no storage backend" error above. Useful for `cargo test`, not for running the service. |
| `otel` | OpenTelemetry tracing + a Prometheus `/metrics` endpoint. Combine with a storage feature, e.g. `local_fs,otel`. |

You can enable more than one storage feature in the same binary - see
`STORAGE_TYPE` in [Configuration](configuration.md) for how the backend is
then chosen at runtime.

```bash
# Build with local filesystem storage
cargo build --features local_fs

# Build with S3/MinIO storage
cargo build --features s3

# Build with local filesystem storage plus tracing/metrics
cargo build --features local_fs,otel
```

`--no-default-features` is a no-op today (`default = []`), but CI always
passes it explicitly so these stay genuinely isolated feature sets if a
non-empty default is ever added later - worth doing the same locally.

## Configure the two startup checks

Both of these are enforced in `ApiService::create` at process startup, not
per-request - a misconfigured deployment is refused outright rather than
serving broken responses.

### 1. Signed URLs (always required)

Signing is the default, not opt-in ([GH #27](https://github.com/vaam-store/image-resizer/issues/27)).
`emgr` refuses to start unless **both** `SIGNING_KEY` and `SIGNING_SALT`
(hex-encoded) are set, **or** you explicitly opt out:

```bash
# Real key/salt (recommended, even for local dev)
export SIGNING_KEY=$(openssl rand -hex 32)
export SIGNING_SALT=$(openssl rand -hex 16)

# Or, explicitly opt out for local development only:
export ALLOW_UNSIGNED_REQUESTS=true
```

With `ALLOW_UNSIGNED_REQUESTS=true`, a request whose signature segment is
the literal `unsigned` bypasses verification. It never weakens
verification of a real signature - it only widens that one escape path.
See [Configuration](configuration.md) and the
[API reference](../user-guide/api-reference.md) for the signed-URL format,
and [Examples](../user-guide/examples.md) for computing a real signature.

### 2. `/metrics` authentication (`otel` builds only)

If (and only if) the binary was built with `--features otel`, a second,
independent check applies: `emgr` refuses to start unless
`METRICS_AUTH_TOKEN` is set, **or** you explicitly opt out with
`ALLOW_UNAUTHENTICATED_METRICS=true` ([GH #77](https://github.com/vaam-store/image-resizer/issues/77)).
`/metrics` exposes request rates, cache hit ratios, error counts and
latency histograms - reconnaissance-grade information about the service's
traffic - so this fails closed exactly like signing does.

```bash
export METRICS_AUTH_TOKEN=$(openssl rand -hex 32)
# Or: export ALLOW_UNAUTHENTICATED_METRICS=true
```

A non-`otel` build has no `/metrics` endpoint at all and doesn't require
either variable. `/health` is unauthenticated on both builds - orchestrator
probes can't easily carry a secret.

## Run it

```bash
export LOCAL_FS_STORAGE_PATH=./data/images
export ALLOW_UNSIGNED_REQUESTS=true   # or the real SIGNING_KEY/SIGNING_SALT above

cargo run --features local_fs --bin emgr
```

`--bin emgr` is required - the crate also ships `healthcheck` (the Docker
`HEALTHCHECK` binary) and `benchmark` (an HTTP load-test tool, see
[Testing](../development/testing.md)), so `cargo run` alone can't guess
which one you mean.

By default the service listens on `0.0.0.0:3000` (`HOST` / `PORT`):

```bash
curl -i http://localhost:3000/health
```

```text
HTTP/1.1 200 OK
content-type: text/plain; charset=utf-8
...

OK
```

For the full environment variable reference (storage backend selection,
the SSRF source-fetch guard, resolution limits, performance tuning,
observability), see [Configuration](configuration.md) - it's generated
against `src/modules/env/env.rs` and CI-checked for drift.

## Docker

See [Docker deployment](../deployment/docker.md) for the four build
targets (`fs_deploy`, `fs_otel_deploy`, `s3_deploy`, `s3_otel_deploy`) and
`compose.yaml`.

## Kubernetes

See the [Helm chart](../deployment/helm-chart.md) guide for deploying to
Kubernetes.
