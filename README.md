# EmgR - Image Resizing Service

EmgR is a high-performance image resizing service built with Rust, designed to efficiently process and deliver images in various formats and sizes. It leverages asynchronous processing with Tokio and the Axum web framework for robust and scalable performance.

## Features

*   **Dynamic Image Resizing**: Resize images on-the-fly by specifying URL, width, height, and desired output format.
*   **Multiple Output Formats**: Supports common image formats like JPG, PNG, and WebP.
*   **Efficient Caching**: (Implicit) Resized images are cached to ensure fast delivery for subsequent requests.
*   **Storage Agility**: Supports multiple storage backends for resized images:
    *   Local filesystem
    *   AWS S3 (or S3-compatible services like MinIO)
    *   In-memory (primarily for testing or specific use-cases)
*   **OpenAPI Specification**: Clearly defined API using OpenAPI v3.
*   **Containerized**: Easily deployable using Docker and Docker Compose.
*   **Observability**: Integrated with Jaeger for tracing via OpenTelemetry.

## Technology Stack

*   **Language**: Rust (Edition 2024)
*   **Web Framework**: Axum
*   **Async Runtime**: Tokio
*   **Image Processing**: `image` crate
*   **Containerization**: Docker, Docker Compose
*   **API Specification**: OpenAPI 3.0
*   **Tracing**: Jaeger, OpenTelemetry
*   **Dependencies**: `reqwest` (HTTP client), `sha2` (hashing), `envconfig` (configuration), `validator` (input validation), `aws-sdk-s3` (for S3 storage).

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

2.  **Initialize the project (Generates server code from OpenAPI spec):**
    This step uses `docker compose` to run the OpenAPI generator.
    ```bash
    make init
    ```

3.  **Build the project images:**
    This command builds the Docker images defined in [`compose.yaml`](compose.yaml:1).
    ```bash
    make build
    ```
    *Note: `make init` is a prerequisite for `make build`.*

4.  **Start the application and dependent services (including Jaeger for tracing):**
    This brings up the `app` service and its dependencies (like `tracking` for Jaeger). The application will be accessible on port `13001`.
    ```bash
    make up
    ```
    Alternatively, to start only the application:
    ```bash
    make up-app
    ```

5.  **Verify the application is running:**
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

You can test the image resizing functionality using `curl`. The service listens on `localhost:13001`.

```bash
curl -LI 'http://localhost:13001/api/images/resize?url=https%3A%2F%2Fimages.pexels.com%2Fphotos%2F32138887%2Fpexels-photo-32138887.jpeg%3Fcs%3Dsrgb%26dl%3Dpexels-branka-krnjaja-1475677195-32138887.jpg%26fm%3Djpg%26w%3D1280%26h%3D1910&width=1000&height=1000&format=jpg'
```

You should see a `302 Found` response, with a `Location` header pointing to the resized image:

```plaintext
HTTP/1.1 302 Found
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
Or, open the direct resize URL (which will perform the resize and then redirect):
```bash
open 'http://localhost:13001/api/images/resize?url=https%3A%2F%2Fimages.pexels.com%2Fphotos%2F32138887%2Fpexels-photo-32138887.jpeg%3Fcs%3Dsrgb%26dl%3Dpexels-branka-krnjaja-1475677195-32138887.jpg%26fm%3Djpg%26w%3D1280%26h%3D1910&width=200&height=200&format=jpg'
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

The API is defined in [`openapi.yaml`](openapi.yaml:1). Key endpoints include:

*   `GET /api/images/resize`
    *   **Summary**: Resizes an image based on the provided parameters.
    *   **Query Parameters**:
        *   `url` (string, required): The URL of the image to resize.
        *   `width` (integer, required): The desired width of the resized image (min: 100, max: 2048).
        *   `height` (integer, required): The desired height of the resized image (min: 100, max: 2048).
        *   `format` (string, required): The desired output format (`png`, `webp`, `jpg`).
    *   **Responses**:
        *   `302 Found`: Redirects to the path of the resized image. The `Location` header contains the URL to the processed image.

*   `GET /api/images/files/{key}`
    *   **Summary**: Downloads a previously resized image.
    *   **Path Parameters**:
        *   `key` (string, required): The unique key (hash) of the image file.
    *   **Responses**:
        *   `200 OK`: Returns the image file with the appropriate `Content-Type` (e.g., `image/png`, `image/jpeg`).

## Configuration

The application can be configured via environment variables, as seen in [`compose.yaml`](compose.yaml:1):

*   `CDN_BASE_URL`: The base URL for constructing links to served image files (e.g., `http://localhost:13001/api/images/files`).
*   `LOG_LEVEL`: Sets the logging verbosity (e.g., `info`, `debug`).
*   `OTLP_SPAN_ENDPOINT`: Endpoint for OpenTelemetry trace collector (Jaeger).
*   `OTLP_METRIC_ENDPOINT`: Endpoint for OpenTelemetry metrics collector.
*   `OTLP_SERVICE_NAME`: Service name for OpenTelemetry.

## Contributing

Please read `CONTRIBUTING.md` for details on our code of conduct, and the process for submitting pull requests to us. (Note: `CONTRIBUTING.md` to be created)

## License

This project is licensed under the MIT License - see the [`LICENSE`](LICENSE:0) file for details.