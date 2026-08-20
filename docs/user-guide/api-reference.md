# API Reference

`emgr` exposes a small HTTP API for resizing and then downloading images.
This page is the source of truth for it - there is no `openapi.yaml` any
more. [GH #53](https://github.com/vaam-store/image-resizer/issues/53)
removed OpenAPI code generation and replaced the query-parameter
`GET /api/images/resize?...` endpoint with an imgproxy-compatible signed
URL path, implemented under
[#27](https://github.com/vaam-store/image-resizer/issues/27) alongside HMAC
signing (see the [rationale in ADR 0002](https://github.com/vaam-store/image-resizer/blob/main/adr/0002-url-api-shape.md)).
This is a hard cutover: the old query-parameter route is gone, not aliased.

## Base URL

There is no versioned path prefix (no `/api/v1`) - every route below is
relative to the service root, e.g. `http://localhost:3000`.

## Endpoints

### Resize an image (signed URL)

```
GET /{signature}/{processing_options}/{plain|base64 source}.{extension}
```

Fetches the source image, resizes/transforms it, stores the result, and
redirects to it (`301`) - it does not stream the resized bytes back
directly (see [Download a resized image](#download-a-resized-image)
below). Modeled on imgproxy's own URL shape closely enough that a client
library written for imgproxy produces something this service accepts.

#### `{signature}`

Either:

- A URL-safe base64 (no padding) HMAC-SHA256 signature over
  `salt || {processing_options}/{source}.{extension}` (the request path
  *after* the signature segment, leading `/` included, exactly as received
  on the wire), keyed by `SIGNING_KEY`/`SIGNING_SALT`
  ([Configuration](../getting-started/configuration.md#signed-urls)).
  Verified in constant time.
- The literal string `unsigned` - only accepted when the operator has set
  `ALLOW_UNSIGNED_REQUESTS=true`. Refused with `403` otherwise. Signing is
  the default; this is a local-development escape hatch, not a normal mode.

An invalid, missing, or (when not explicitly allowed) `unsigned` signature
returns `403 Forbidden` before any further parsing happens.

#### `{processing_options}`

Zero or more `/`-delimited segments, each `code:arg1:arg2:...`, mirroring
imgproxy's own short option codes:

| Code | Meaning | Example |
|---|---|---|
| `rs` | Resize: `rs:{type}:{width}:{height}`. `{type}` is accepted for imgproxy URL compatibility but doesn't currently change behaviour (resize mode is derived from which of width/height are set - see below). `0` for either dimension means "not set". | `rs:fill:300:300` |
| `q` | Output encode quality, `0`-`100`. | `q:80` |
| `bl` | Gaussian blur sigma. | `bl:5` |
| `g` | Grayscale: `true`/`false`/`1`/`0`. | `g:true` |
| `el` | Enlarge: allow upscaling past the source resolution (`true`/`false`/`1`/`0` as `1`/`0`). Default `0` (refused) - see [GH #36](https://github.com/vaam-store/image-resizer/issues/36). | `el:1` |

An unknown option code, wrong argument count, or a value out of range
returns `400 Bad Request`.

Width/height resize behaviour, unchanged from before #53: both set resizes
and crops to fill exactly that box; only one set resizes preserving aspect
ratio; neither set leaves dimensions untouched. Output never exceeds the
source's resolution unless `el:1` opts in.

#### `{plain|base64 source}.{extension}`

The trailing `.{extension}` is mandatory (one of `jpg`, `jpeg`, `png`,
`webp`) and always determines the output format - it is stripped from
whatever precedes it, regardless of what that looks like.

- **Base64 form** (default): a single URL-safe base64 (no padding) segment
  encoding the source URL, e.g. `aHR0cHM6Ly9leGFtcGxlLmNvbS9waG90by5qcGc.webp`
  decodes to `https://example.com/photo.jpg` with output format `webp`.
- **Plain form**: prefix with `plain/` followed by the literal,
  percent-encoded-where-needed source URL, e.g.
  `plain/https://example.com/photo.jpg.webp` - the `.webp` at the very end
  is still the grammar's extension, not part of the URL, so the decoded
  source here is `https://example.com/photo.jpg`.

A malformed source (missing/unrecognized extension, invalid base64, empty)
returns `400 Bad Request`.

#### Worked example

With `SIGNING_KEY=6d792d7369676e696e672d6b6579` and
`SIGNING_SALT=6d792d73616c74` (hex for `my-signing-key` / `my-salt` -
placeholders only, never use these for anything real), resizing
`https://images.example.com/photo.jpg` to fill 300x300 at quality 80,
output JPEG:

```
GET /de7BKgwO8wFeNZWRWgp3UB9jKwOkVoYM_eMKau2ECgw/rs:fill:300:300/q:80/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg
```

(Pinned by a test - `src/modules/signing/verify.rs`'s
`documented_example` module - so this example can't silently drift from
what the code actually computes.)

#### Responses

| Status | Meaning |
|---|---|
| `301 Moved Permanently` | Resize succeeded. `Location` header points at the [download endpoint](#download-a-resized-image) for the result - never a redirect back to the caller-supplied source (see [GH #25](https://github.com/vaam-store/image-resizer/issues/25)). |
| `400 Bad Request` | The signed-URL path is malformed (bad processing option, missing/unrecognized extension, invalid base64/percent-encoding, ...), or the source URL doesn't decode as an image, or exceeds a configured size/resolution limit. `Cache-Control: no-store`. |
| `403 Forbidden` | The signature is missing, invalid, or `unsigned` while `ALLOW_UNSIGNED_REQUESTS` isn't set. `Cache-Control: no-store`. |
| `502 Bad Gateway` | The origin server for the source image failed (unreachable, non-2xx, or the connection dropped mid-transfer). `Cache-Control: no-store`. |
| `503 Service Unavailable` | The service is shedding load (download/processing concurrency limits reached). `Cache-Control: no-store`. |

### Download a resized image

```
GET /api/images/files/{key}
```

Unchanged from before #53, and deliberately **not** part of the signed-URL
scheme above: it only ever serves bytes already produced and cached by a
successful resize, addressed by a content hash that
`key_validation::validate_cache_key` rejects anything malformed against
(traversal, absolute paths, wrong shape) - there's no attacker-controlled
fetch or CPU cost here to gate with a signature, unlike the resize route.

Downloads a previously-resized image by its cache key (the value of the
`Location` header from a successful resize above - not something you
construct by hand). The key is exactly what
`CacheService::generate_key` produces: an optional `STORAGE_SUB_PATH`
prefix followed by a 64-character lowercase hex SHA-256 digest and one of
`.jpg`/`.png`/`.webp`.

#### Responses

| Status | Meaning |
|---|---|
| `200 OK` | Returns the image bytes with `Content-Type` derived from the key's own extension (`image/jpeg`, `image/png`, or `image/webp`) and `Cache-Control: public, max-age=31536000, immutable`. |
| `404 Not Found` | No image exists for the given key. `Cache-Control: no-store`. |
| `502 Bad Gateway` | The storage backend failed to serve the image. `Cache-Control: no-store`. |

### Health check

```
GET /health
```

Returns `200 OK` with a plain-text body of `OK` - not JSON, and no
version field. `GET /` redirects here (`307 Temporary Redirect`).

### Metrics

```
GET /metrics
```

Only mounted when the binary is built with `--features otel` (see
[`src/modules/router/router.rs`](https://github.com/vaam-store/image-resizer/blob/main/src/modules/router/router.rs));
absent otherwise. Returns metrics in Prometheus text format.

## Error response body

Error responses (`400`/`403`/`404`/`502`/`503` above) return a plain-text
body (`content-type: text/plain`).
