# EmgR

Welcome to the documentation for **EmgR**, a high-performance, imgproxy-compatible
image resizing service built in Rust. It fetches a source image over HTTP(S),
resizes/transforms it according to a signed URL, caches the result, and
redirects the caller to it.

## Features

- On-the-fly resizing via an [imgproxy-compatible signed URL scheme](user-guide/api-reference.md) -
  resize/crop, gravity, quality, blur, grayscale, watermarks and named presets.
- JPEG, PNG, WebP (lossy and lossless), AVIF and animated GIF/WebP output,
  through a SIMD resampler (`fast_image_resize`) and a mozjpeg-backed JPEG
  decode/encode path (DCT-scaled decode, progressive/subsampling control).
- Pluggable storage backends selected at **compile time** via Cargo feature
  flags: local filesystem, S3/MinIO, or in-memory (test builds only).
- HMAC-signed URLs **on by default**, an SSRF-guarded source fetcher, and
  configurable resolution/output limits.
- OpenTelemetry tracing and a bearer-token-protected `/metrics` endpoint,
  available behind the `otel` feature.
- Kubernetes deployment via a Helm chart (or a Knative Service chart for
  serverless setups).

## Before you start

Two things surprise newcomers, and both are deliberate:

1. **`emgr` has no default storage backend.** `Cargo.toml` sets
   `default = []`, so a plain `cargo build` compiles but the resulting
   binary refuses to start ("No storage features are enabled"). Every
   build must name at least one of `local_fs`, `s3`, or `in_memory`
   explicitly.
2. **The service fails closed at startup** unless signed URLs are
   configured (`SIGNING_KEY` + `SIGNING_SALT`) or you explicitly opt out
   with `ALLOW_UNSIGNED_REQUESTS=true`. A build with the `otel` feature
   adds a second, independent check for `/metrics`.

See [Getting Started → Installation](getting-started/installation.md) for
the full explanation, the exact commands, and every feature combination CI
tests.

## Quick start

```bash
git clone https://github.com/vaam-store/image-resizer.git
cd image-resizer

# local_fs storage, signing turned off for local dev only - see
# Installation for the signed-URL alternative. --bin is required because
# the crate also ships `healthcheck` and `benchmark` binaries.
LOCAL_FS_STORAGE_PATH=./data/images ALLOW_UNSIGNED_REQUESTS=true \
  cargo run --no-default-features --features local_fs --bin emgr
```

```bash
curl -i http://localhost:3000/health
```

## API overview

The service exposes a RESTful, imgproxy-compatible resize endpoint plus a
download endpoint for already-cached results. See the
[API Reference](user-guide/api-reference.md) for the full URL grammar,
status codes and processing options, and [Examples](user-guide/examples.md)
for computing a valid signature in Python/JavaScript/bash.

## Architecture

The service is a hand-written Axum router (no OpenAPI code generation) over
a small set of modular Rust services. See the
[architecture overview](architecture/overview.md) and
[components](architecture/components.md) pages.

## Deployment

Run it with [Docker](deployment/docker.md), or deploy it to Kubernetes with
the [Helm chart](deployment/helm-chart.md).
