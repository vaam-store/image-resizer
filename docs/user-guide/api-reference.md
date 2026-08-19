# API Reference

`emgr` exposes a small HTTP API for resizing and then downloading images.
This page is generated against [`openapi.yaml`](https://github.com/vaam-store/image-resizer/blob/main/openapi.yaml)
in the repository root, which is the single source of truth - if this
page and the spec ever disagree, trust the spec (or better, open an
issue: see [GH #47](https://github.com/vaam-store/image-resizer/issues/47),
which is what rewrote this page after it had drifted from both the spec
and the code for some time - wrong base path, a `quality`/`fit` API that
was never implemented, and status codes that didn't match either).

## Base URL

There is no versioned path prefix (no `/api/v1`) - every route below is
relative to the service root, e.g. `http://localhost:3000`.

## Endpoints

### Resize an image

```
GET /api/images/resize
```

Fetches the source image, resizes/transforms it, stores the result, and
redirects to it - it does not stream the resized bytes back directly (see
[Redirect an image](#download-a-resized-image) below).

#### Query parameters

| Parameter | Type | Description | Required |
|---|---|---|---|
| `url` | string (URI) | Source image URL. | Yes |
| `width` | integer | Target width in pixels. Min 10, max 4096, default 200. | No |
| `height` | integer | Target height in pixels. Min 10, max 4096, default 200. | No |
| `format` | string | Output format: `png`, `webp`, or `jpg`. Default `jpg`. | No |
| `blur_sigma` | number | Gaussian blur sigma. Min 0, max 100, default 5. | No |
| `grayscale` | boolean | Convert to grayscale. | No |

There is no `quality` or `fit` parameter - both appeared on this page
before but were never implemented.

#### Example request

```
GET /api/images/resize?url=https%3A%2F%2Fexample.com%2Fimage.jpg&width=800&height=600&format=webp
```

#### Responses

| Status | Meaning |
|---|---|
| `301 Moved Permanently` | Resize succeeded. `Location` header points at the [download endpoint](#download-a-resized-image) for the result - never a redirect back to the caller-supplied `url` (see [GH #25](https://github.com/vaam-store/image-resizer/issues/25)). |
| `400 Bad Request` | The source URL doesn't decode as an image, or exceeds a configured size/resolution limit. `Cache-Control: no-store`. |
| `502 Bad Gateway` | The origin server for the source image failed (unreachable, non-2xx, or the connection dropped mid-transfer). `Cache-Control: no-store`. |
| `503 Service Unavailable` | The service is shedding load (download/processing concurrency limits reached). `Cache-Control: no-store`. |

Earlier versions of this page said `302 Found` - the code has always
returned `301` (`ResizeResponse::Status301_...` in
`src/modules/api/resize.rs`), and `openapi.yaml` already agreed; only
this page was wrong.

### Download a resized image

```
GET /api/images/files/{key}
```

Downloads a previously-resized image by its cache key (the value of the
`Location` header from a successful resize above - not something you
construct by hand). The key is exactly what
`CacheService::generate_key` produces: an optional `STORAGE_SUB_PATH`
prefix followed by a 64-character lowercase hex SHA-256 digest and one of
`.jpg`/`.png`/`.webp`. Anything else is rejected before it reaches any
storage backend (see [GH #23](https://github.com/vaam-store/image-resizer/issues/23)).

#### Responses

| Status | Meaning |
|---|---|
| `200 OK` | Returns the image bytes with the matching `Content-Type` (`image/png`, `image/jpeg`, `image/webp`, or `application/octet-stream`) and `Cache-Control: public, max-age=31536000, immutable`. |
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

Error responses (`400`/`404`/`502`/`503` above) return a plain-text body
(`content-type: text/plain`), not the `{"error": {"code": ..., "message":
...}}` JSON envelope this page previously described - that shape was
never implemented.
