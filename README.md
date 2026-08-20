# EmgR - Image Resizing Service

EmgR is a high-performance image resizing service built with Rust, designed to efficiently process and deliver images in various formats and sizes. It leverages asynchronous processing with Tokio and the Axum web framework for robust and scalable performance.

## Features

*   **Dynamic Image Resizing**: Resize images on-the-fly via an imgproxy-compatible signed URL path, specifying source, dimensions, and output format.
*   **Multiple Output Formats**: Supports common image formats like JPG, PNG, and WebP.
*   **Efficient Caching**: (Implicit) Resized images are cached to ensure fast delivery for subsequent requests.
*   **Storage Agility**: Supports multiple storage backends for resized images:
    *   Local filesystem
    *   AWS S3 (or S3-compatible services like MinIO)
    *   In-memory (primarily for testing or specific use-cases)
*   **Signed URLs**: HMAC-SHA256 request signing, matching imgproxy's scheme closely enough that a client library written for imgproxy works against this service (see [GH #27](https://github.com/vaam-store/image-resizer/issues/27)). Signing is the default, not opt-in.
*   **Containerized**: Easily deployable using Docker and Docker Compose.
*   **Observability**: Integrated with Jaeger for tracing via OpenTelemetry.

## Technology Stack

*   **Language**: Rust (Edition 2024)
*   **Web Framework**: Axum (hand-written router - no OpenAPI code generation, see [GH #53](https://github.com/vaam-store/image-resizer/issues/53))
*   **Async Runtime**: Tokio
*   **Image Processing**: `image` crate
*   **Containerization**: Docker, Docker Compose
*   **Tracing**: Jaeger, OpenTelemetry
*   **Dependencies**: `reqwest` (HTTP client), `sha2`/`hmac` (hashing/signing), `envconfig` (configuration), `aws-sdk-s3` (for S3 storage).

## Getting Started

These instructions will get you a copy of the project up and running on your local machine for development and testing purposes.

### Prerequisites

*   Docker and Docker Compose
*   `make` (optional, for using Makefile commands)
*   `curl` (for testing)

### Installation & Running

1.  **Clone the repository:**
    ```bash
    git clone https://your-repository-url/emgr.git
    cd emgr
    ```
    A fresh clone builds with plain `cargo build` - no Docker step needed
    first (that used to require `make init` to run OpenAPI codegen; removed
    under [GH #53](https://github.com/vaam-store/image-resizer/issues/53)).

2.  **Build the project images:**
    This command builds the Docker images defined in [`compose.yaml`](compose.yaml:1).
    ```bash
    make build
    ```

3.  **Start the application and dependent services (including Jaeger for tracing):**
    This brings up the `app` service and its dependencies (like `tracking` for Jaeger). The application will be accessible on port `13001`.
    ```bash
    make up
    ```
    Alternatively, to start only the application:
    ```bash
    make up-app
    ```

4.  **Verify the application is running:**
    You can check the status of the containers:
    ```bash
    make ps
    # or
    docker compose -p emgr ps
    ```
    View application logs:
    ```bash
    make logs-app
    ```
    Jaeger UI will be available at: `http://localhost:16686`

### Testing the Resize Endpoint

The resize endpoint takes an HMAC-signed URL path, not query parameters -
see the [API reference](docs/user-guide/api-reference.md) for the full
grammar and [Examples](docs/user-guide/examples.md) for how to compute a
signature in Python/JavaScript/bash. With the placeholder
`SIGNING_KEY=6d792d7369676e696e672d6b6579` /
`SIGNING_SALT=6d792d73616c74` from [`.env.example`](.env.example) (never
use these for anything real) and the service listening on `localhost:13001`:

```bash
curl -LI 'http://localhost:13001/de7BKgwO8wFeNZWRWgp3UB9jKwOkVoYM_eMKau2ECgw/rs:fill:300:300/q:80/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg'
```

You should see a `301 Moved Permanently` response, with a `Location` header pointing to the resized image:

```plaintext
HTTP/1.1 301 Moved Permanently
location: http://localhost:13001/api/images/files/your-image-hash.jpg
vary: origin, access-control-request-method, access-control-request-headers
access-control-allow-origin: *
content-length: 0
date: Sat, 31 May 2025 XX:XX:XX GMT

HTTP/1.1 200 OK
content-type: image/jpeg
vary: origin, access-control-request-method, access-control-request-headers
access-control-allow-origin: *
content-length: XXXXXX
date: Sat, 31 May 2025 XX:XX:XX GMT
```

You can then open the `location` URL in your browser to view the resized image, for example:
```bash
open http://localhost:13001/api/images/files/your-image-hash.jpg
```
Or, open the signed resize URL directly (which will perform the resize and then redirect):
```bash
open 'http://localhost:13001/de7BKgwO8wFeNZWRWgp3UB9jKwOkVoYM_eMKau2ECgw/rs:fill:300:300/q:80/aHR0cHM6Ly9pbWFnZXMuZXhhbXBsZS5jb20vcGhvdG8uanBn.jpg'
```

### Other Useful Commands

*   **Stop the project:**
    ```bash
    make down
    ```
*   **Destroy the project (stops and removes containers, networks, and volumes):**
    ```bash
    make destroy
    ```
*   **View all logs:**
    ```bash
    make logs
    ```
*   **Show help (lists all Makefile targets):**
    ```bash
    make help
    ```

## API Endpoints

The full grammar and every response code live in the
[API reference](docs/user-guide/api-reference.md) - there is no
`openapi.yaml` any more ([GH #53](https://github.com/vaam-store/image-resizer/issues/53)
replaced OpenAPI code generation with a hand-written router). Summary:

*   `GET /{signature}/{processing_options}/{plain|base64 source}.{extension}`
    *   **Summary**: HMAC-signed, imgproxy-compatible resize URL. Fetches the source, resizes/transforms it, caches the result, and redirects to it.
    *   **`{signature}`**: base64url HMAC-SHA256 over the rest of the path, keyed by `SIGNING_KEY`/`SIGNING_SALT` (see [Configuration](#configuration)) - or the literal `unsigned` when `ALLOW_UNSIGNED_REQUESTS=true` is set. Signing is the default, not opt-in.
    *   **`{processing_options}`**: zero or more `/`-delimited `code:args` segments - `rs:{type}:{w}:{h}` (resize/crop), `q:{0-100}` (quality), `bl:{sigma}` (blur), `g:{true|false}` (grayscale), `el:{1|0}` (allow upscaling).
    *   **Responses**:
        *   `301 Moved Permanently`: Redirects to the path of the resized image. The `Location` header contains the URL to the processed image. (Never a redirect to the caller-supplied source - see [GH #25](https://github.com/vaam-store/image-resizer/issues/25).)
        *   `400 Bad Request`: The signed-URL path is malformed, or the source URL doesn't decode as an image, or exceeds a configured limit.
        *   `403 Forbidden`: The signature is missing, invalid, or `unsigned` without `ALLOW_UNSIGNED_REQUESTS=true`.
        *   `502 Bad Gateway`: The origin server for the requested image failed.
        *   `503 Service Unavailable`: The service is shedding load (concurrency limits reached).

*   `GET /api/images/files/{key}`
    *   **Summary**: Downloads a previously resized image. Unsigned - only ever serves already-cached, hash-addressed bytes, not an arbitrary fetch.
    *   **Path Parameters**:
        *   `key` (string, required): The unique key (hash) of the image file.
    *   **Responses**:
        *   `200 OK`: Returns the image file with the appropriate `Content-Type` (e.g., `image/png`, `image/jpeg`).

## Configuration

The application is configured entirely via environment variables, read by
[`src/modules/env/env.rs`](src/modules/env/env.rs). The full, CI-checked
reference lives at
[`docs/getting-started/configuration.md`](docs/getting-started/configuration.md)
(covers storage backend selection, the SSRF source-fetch guard,
resolution/output limits, performance tuning, and observability). A
starting-point `.env` file is at [`.env.example`](.env.example) - copy it
to `.env` (gitignored). The most commonly-touched variables, as seen in
[`compose.yaml`](compose.yaml):

*   `STORAGE_TYPE`: Storage backend - `LOCAL_FS` or `S3` (alias `MINIO`).
*   `CDN_BASE_URL`: The base URL for constructing links to served image files (e.g., `http://localhost:13001/api/images/files`).
*   `SIGNING_KEY` / `SIGNING_SALT`: Hex-encoded HMAC-SHA256 key/salt for signed URLs ([GH #27](https://github.com/vaam-store/image-resizer/issues/27)). Required unless `ALLOW_UNSIGNED_REQUESTS=true` - signing is the default, the process refuses to start without one or the other.
*   `LOG_LEVEL`: Sets the logging verbosity (e.g., `info`, `debug`).
*   `OTLP_SPAN_ENDPOINT`: Endpoint for OpenTelemetry trace collector (Jaeger).
*   `OTLP_METRIC_ENDPOINT`: Endpoint for OpenTelemetry metrics collector.
*   `OTLP_SERVICE_NAME`: Service name for OpenTelemetry.

## Contributing

Please read `CONTRIBUTING.md` for details on our code of conduct, and the process for submitting pull requests to us. (Note: `CONTRIBUTING.md` to be created)

## License

This project is licensed under the MIT License - see the [`LICENSE`](LICENSE:0) file for details.