# Configuration

`emgr` is configured entirely through environment variables, read by
[`src/modules/env/env.rs`](https://github.com/vaam-store/image-resizer/blob/main/src/modules/env/env.rs)
via [`envconfig`](https://docs.rs/envconfig). That file is the source of
truth this page is generated against - a CI check
(`.github/workflows/ci.yml`'s `docs-env-drift` job, `.github/scripts/check_env_docs.py`)
fails the build if this table drifts from it in either direction.

!!! note "This page previously documented variables that don't exist"
    *CACHE_ENABLED*, *CACHE_TTL_SECONDS*, *S3_BUCKET*, *S3_REGION* and
    *LOCAL_STORAGE_PATH* were listed here before but were never read by
    the code (GH #47). There is no response-cache TTL setting today (see
    [GH #40](https://github.com/vaam-store/image-resizer/issues/40)); the
    real storage variables are `MINIO_BUCKET`, `MINIO_REGION` and
    `LOCAL_FS_STORAGE_PATH`, documented below.

## Server

| Variable | Description | Default |
|---|---|---|
| `HOST` | Interface the HTTP server binds to. | `0.0.0.0` |
| `PORT` | HTTP server port. | `3000` |
| `CDN_BASE_URL` | Base URL used to build the `Location` header on a successful resize - see [Redirect status code](../user-guide/api-reference.md) (a `301`, not `302`). | `http://localhost:9000/image-cache` |

## Storage backend selection

| Variable | Description | Default |
|---|---|---|
| `STORAGE_TYPE` | Which backend to use. Accepted values: `LOCAL_FS` (aliases `LOCALFS`, `LOCAL`), `S3` (alias `MINIO`), `IN_MEMORY` (aliases `INMEMORY`, `MEMORY` - test/dev only; refuses to start in a release build). Only takes effect for whichever storage feature(s) the binary was compiled with. | _unset_ |
| `STORAGE_SUB_PATH` | Prefix prepended to every generated cache key, for either backend. | `""` (empty) |

### Local filesystem storage

Requires the binary to be built with `--features local_fs`.

| Variable | Description | Default |
|---|---|---|
| `LOCAL_FS_STORAGE_PATH` | Local path resized images are written to. | `./data/images` |

### MinIO / S3 storage

Requires the binary to be built with `--features s3`. MinIO and AWS S3
share the same client (MinIO is S3-compatible), so there is a single set
of variables for both.

| Variable | Description | Default |
|---|---|---|
| `MINIO_ENDPOINT_URL` | S3-compatible endpoint URL. | `http://localhost:9000` |
| `MINIO_ACCESS_KEY_ID` | Access key. | `minioadmin` |
| `MINIO_SECRET_ACCESS_KEY` | Secret key. | `minioadmin` |
| `MINIO_BUCKET` | Bucket resized images are written to. | `image-cache` |
| `MINIO_REGION` | Bucket region. | `us-east-1` |

## Signed URLs

Added under [GH #27](https://github.com/vaam-store/image-resizer/issues/27),
alongside the imgproxy-compatible signed-path URL scheme itself (see the
[API reference](../user-guide/api-reference.md)). Signing is the default,
not opt-in: with neither a key/salt configured nor
`ALLOW_UNSIGNED_REQUESTS=true` set, the process refuses to start rather than
silently serving `403` to every request.

| Variable | Description | Default |
|---|---|---|
| `SIGNING_KEY` | Hex-encoded HMAC-SHA256 key used to verify signed URLs. Required unless `ALLOW_UNSIGNED_REQUESTS=true`. imgproxy equivalent: *IMGPROXY_KEY*. | _unset_ |
| `SIGNING_SALT` | Hex-encoded salt mixed into every signed URL's HMAC input. Required unless `ALLOW_UNSIGNED_REQUESTS=true`. imgproxy equivalent: *IMGPROXY_SALT*. | _unset_ |
| `ALLOW_UNSIGNED_REQUESTS` | Opt-in escape hatch for local development: when `true`, a request whose signature segment is the literal `unsigned` bypasses verification entirely. Does not weaken verification of a real signature - it only widens the `unsigned` escape path. | `false` |

## SSRF / source-fetch guard

Added under [GH #21](https://github.com/vaam-store/image-resizer/issues/21)
to stop the `url` query parameter from being used to reach internal
services. Mirrors imgproxy's equivalent settings, named in each row below.

| Variable | Description | Default |
|---|---|---|
| `MAX_REDIRECTS` | Maximum redirects the source fetch follows; every hop is re-validated (scheme, allowlist, resolved address). imgproxy: *IMGPROXY_MAX_REDIRECTS*. | `5` |
| `ALLOWED_SOURCES` | Comma-separated allowlist of source URL prefixes. Unset allows any `http(s)` URL, still subject to the private-range guard below. A host matching an entry here is also exempted from the private-IP-range block (RFC1918/CGNAT/IPv6 ULA) for that hop, so an explicitly-named internal origin (a Kubernetes Service ClusterIP, an internal MinIO, a private CDN shield) is reachable ([GH #57](https://github.com/vaam-store/image-resizer/issues/57)) - loopback and link-local are unaffected and keep their own flags below. Re-checked on every redirect hop. imgproxy: *IMGPROXY_ALLOWED_SOURCES*. | _unset_ |
| `ALLOW_LOOPBACK_SOURCE_ADDRESSES` | Opt-in to allow fetching from loopback addresses (blocked by default). | `false` |
| `ALLOW_LINK_LOCAL_SOURCE_ADDRESSES` | Opt-in to allow fetching from link-local addresses (blocked by default). | `false` |

## Resolution and output limits

Added under [GH #26](https://github.com/vaam-store/image-resizer/issues/26).
See the [API reference](../user-guide/api-reference.md) for how these
interact with the `width`/`height` query parameters.

| Variable | Description | Default |
|---|---|---|
| `MAX_SRC_RESOLUTION_MP` | Maximum decoded *source* resolution in megapixels, checked against header dimensions before a full decode. imgproxy default: *IMGPROXY_MAX_SRC_RESOLUTION* = 50. | `50` |
| `MAX_OUTPUT_WIDTH` | Maximum requested output width in pixels. | `4096` |
| `MAX_OUTPUT_HEIGHT` | Maximum requested output height in pixels. | `4096` |
| `MAX_ANIMATION_FRAMES` | Maximum number of frames read from an animated GIF/WebP source before the animated encode path ([GH #49](https://github.com/vaam-store/image-resizer/issues/49)) refuses the request. Enforced while iterating frames, not after decoding all of them, so it bounds work spent on attacker-supplied input rather than just rejecting after the fact - a many-tiny-frames animation can be individually well within `MAX_SRC_RESOLUTION_MP` per frame while still being a real memory/CPU amplification via frame count alone. | `512` |

## Watermarking, presets and the processing-option allowlist

Added under [GH #52](https://github.com/vaam-store/image-resizer/issues/52).
See the [API reference](../user-guide/api-reference.md) for the `wm:`/`wmu:`,
`pr:` request-option syntax these variables back.

| Variable | Description | Default |
|---|---|---|
| `WATERMARK_URL` | Default watermark image URL, used when a request sets `wm:` without its own `wmu:{base64url}`. Fetched through the same SSRF guard (`ALLOWED_SOURCES`, private-range block, redirect re-validation) as any other source URL. A request's own `wmu:` always takes priority over this default. imgproxy: *IMGPROXY_WATERMARK_URL*. | _unset_ |
| `PRESETS` | Preset definitions: comma-separated `{name}={options}` entries, `{options}` itself `/`-separated processing-option segments - e.g. `thumbnail=rs:fill:300:300/q:80,default=el:1`. A preset named `default` is special: it is prepended ahead of every request's own segments automatically, even when the request never names a preset at all. A preset's own definition cannot contain a `pr:` segment (presets don't recurse). imgproxy: *IMGPROXY_PRESETS*. | _unset_ |
| `ALLOWED_PROCESSING_OPTIONS` | Comma-separated allowlist of processing-option short codes (e.g. `rs,q,pr`) permitted directly in a request URL. Unset/blank means unrestricted. Restricts what a request can do directly - it does **not** apply to options used *inside* a preset's own definition, which is what lets an operator hand out a restricted set of presets while forbidding the raw options they're built from. imgproxy: *IMGPROXY_ALLOWED_PROCESSING_OPTIONS*. | _unset_ (unrestricted) |

## JPEG encoding

Added under [GH #76](https://github.com/vaam-store/image-resizer/issues/76).
JPEG output is encoded via `mozjpeg`/libjpeg-turbo rather than the `image`
crate's own encoder, which has no progressive-mode switch and hardcodes
4:2:2 chroma subsampling. See the [API reference](../user-guide/api-reference.md)
for the `jpgo:{progressive}:{no_subsample}` and `mb:{bytes}` request-option
syntax these variables set the deployment-wide default for - a request's
own `jpgo:` segment always overrides these when present.

| Variable | Description | Default |
|---|---|---|
| `JPEG_PROGRESSIVE` | Encode JPEG output progressively (multi-scan) instead of baseline sequential when a request doesn't set `jpgo:`'s `progressive` slot itself. Progressive JPEGs render incrementally and are often, but not always, smaller - see this change's own benchmark report rather than assuming a size win on every image. imgproxy: *IMGPROXY_JPEG_PROGRESSIVE*. | `false` |
| `JPEG_NO_SUBSAMPLING` | Encode JPEG chroma at full resolution (4:4:4) instead of this crate's default 4:2:2 when a request doesn't set `jpgo:`'s `no_subsample` slot itself. 4:4:4 preserves colour detail on saturated edges (screenshots, logos, text on colour) at the cost of a larger file. imgproxy: *IMGPROXY_JPEG_NO_SUBSAMPLING*. | `false` |

## Performance tuning

`PERFORMANCE_PROFILE` selects a preset (see
[`src/config/performance.rs`](https://github.com/vaam-store/image-resizer/blob/main/src/config/performance.rs));
every other variable in this section, if set, overrides that preset's
value for just that one field.

| Variable | Description | Default |
|---|---|---|
| `PERFORMANCE_PROFILE` | One of `high_throughput`, `low_latency`, `memory_efficient` (case-insensitive). Unset (or blank/whitespace-only) falls back to per-field defaults below rather than a named preset. **An unrecognised, non-blank value (e.g. a typo like `hgh_throughput`) refuses to start the process at all**, mirroring `SigningConfig`/`MetricsAuthConfig`'s fail-closed convention, rather than silently falling back to per-field defaults with a different effective configuration than the operator asked for — issue #83. | _unset_ |
| `MAX_CONCURRENT_DOWNLOADS` | Maximum concurrent source-image downloads. | `20` |
| `MAX_CONCURRENT_PROCESSING` | Maximum concurrent image-processing tasks. | Host core count |
| `HTTP_TIMEOUT_SECS` | Timeout for the source-image HTTP client. | `30` |
| `MAX_IMAGE_SIZE_MB` | Maximum accepted source image size. | `50` |
| `CPU_THREAD_POOL_SIZE` | CPU-bound thread pool size for decode/resize/encode. | Host core count |
| `ENABLE_HTTP2` | Enable HTTP/2 for the source-image HTTP client. | `true` (`false` under the `memory_efficient` profile, which trades it away for lower per-connection memory) |
| `CONNECTION_POOL_SIZE` | Source-image HTTP client's per-host connection pool size. | `50` |
| `KEEP_ALIVE_TIMEOUT_SECS` | Source-image HTTP client's keep-alive timeout. | `60` |

## Observability

Requires the binary to be built with `--features otel`. See
[Docker deployment](../deployment/docker.md) for how `compose.yaml` wires
these to the bundled Jaeger instance.

| Variable | Description | Default |
|---|---|---|
| `LOG_LEVEL` | Log verbosity (`trace`, `debug`, `info`, `warn`, `error`). | `debug` |
| `OTLP_SPAN_ENDPOINT` | OTLP gRPC endpoint for traces. | `http://localhost:4317` |
| `OTLP_METRIC_ENDPOINT` | OTLP HTTP endpoint for metrics. | `http://localhost:4318/v1/metrics` |
| `OTLP_SERVICE_NAME` | Service name reported in traces/metrics. | `rust-app-example` |

### `/metrics` and `/health` authentication

Added under [GH #77](https://github.com/vaam-store/image-resizer/issues/77),
a leftover from #27's stated scope. `/metrics` is a Prometheus endpoint -
it exposes request rates, cache hit ratios, error counts and latency
histograms, which is precise reconnaissance for an attacker probing for
expensive requests. Mirroring `SIGNING_KEY`/`SIGNING_SALT`
(see [Signed URLs](#signed-urls) above), requiring a token is the
default, not opt-in: with neither `METRICS_AUTH_TOKEN` configured nor
`ALLOW_UNAUTHENTICATED_METRICS=true` set, the process refuses to start
rather than silently serving traffic telemetry to anyone who asks. This
only applies to builds with `--features otel` - a build without it never
mounts `/metrics` at all, so there is nothing to protect and no startup
check to satisfy.

| Variable | Description | Default |
|---|---|---|
| `METRICS_AUTH_TOKEN` | Bearer token `/metrics` requires via `Authorization: Bearer <token>`. Compared in constant time. Required unless `ALLOW_UNAUTHENTICATED_METRICS=true`. Configure the same value in your Prometheus scraper's `authorization` scrape config. | _unset_ |
| `ALLOW_UNAUTHENTICATED_METRICS` | Opt-in escape hatch: when `true`, `/metrics` is served without requiring a token at all. Does not weaken verification of a real token - it only ever widens the unauthenticated-access escape path. | `false` |

A request with a missing or invalid token gets `401 Unauthorized` with a
`WWW-Authenticate: Bearer` header, not `403` - unlike signed-URL
rejection, there's a real credential the caller can supply on a retry.

`/health` is left unauthenticated on purpose, not by oversight: it is the
target of Kubernetes liveness/readiness/startup probes
(`helm/serverless/templates/knative-service.yaml`), which hit it directly
over HTTP and have no practical way to carry a bearer token. It also only
ever returns the literal string `OK` - none of the traffic/cache/latency
detail `/metrics` exposes - so the risk the two endpoints present isn't
comparable. If `/health` ever needs restricting, do it at the network
layer (an ingress rule or `NetworkPolicy` scoped to the cluster's own
probe traffic), not with application-layer auth that would break the
probes it exists to serve.

## Example `.env` file

See [`.env.example`](https://github.com/vaam-store/image-resizer/blob/main/.env.example)
in the repository root for a copy-pasteable starting point covering every
variable above. Copy it to `.env` (which is gitignored - never commit
real credentials there).

```dotenv
STORAGE_TYPE=LOCAL_FS
LOCAL_FS_STORAGE_PATH=./data/images
CDN_BASE_URL=http://localhost:3000/api/images/files
```

## Docker environment variables

When running with Docker, pass environment variables with `-e`:

```bash
docker run -p 3000:3000 \
  -e STORAGE_TYPE=LOCAL_FS \
  -e LOCAL_FS_STORAGE_PATH=/app/data/images \
  -e CDN_BASE_URL=http://localhost:3000/api/images/files \
  ghcr.io/vaam-store/image-resizer:fs-latest
```

## Helm chart configuration

When deploying with Helm, configure the service via `values.yaml`'s
`configMaps.config.data` block. See the [Helm Chart](../deployment/helm-chart.md)
documentation for details.
