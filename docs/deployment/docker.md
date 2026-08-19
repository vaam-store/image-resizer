# Docker Deployment

This guide explains how to deploy the Image Resize Service using Docker.

## Prerequisites

- Docker installed

## Building the Docker Image

A `Dockerfile` is provided in the project root.

The `Dockerfile` defines four deploy targets (no default/unnamed final
stage), so `--target` is required: `fs_deploy` (local filesystem
storage), `fs_otel_deploy` (+ OpenTelemetry), `s3_deploy` (MinIO/S3
storage), `s3_otel_deploy` (+ OpenTelemetry). `healthcheck.rs` is built
into all four automatically.

```bash
# Navigate to the project root
cd /path/to/image-resizer

# Build the Docker image (local filesystem storage, no OTel)
docker build --target fs_deploy -t image-resizer:latest .
```

## Running the Docker Container

### Basic run

The service listens on port `3000` by default (`PORT`, `HOST`):

```bash
docker run -d -p 3000:3000 --name image-resizer-app image-resizer:latest
```

### With environment variables

You can configure the service using environment variables. See the [Configuration](../getting-started/configuration.md) guide for the full list.

```bash
docker run -d -p 3000:3000 \
  -e STORAGE_TYPE=S3 \
  -e MINIO_ENDPOINT_URL=https://s3.amazonaws.com \
  -e MINIO_BUCKET=my-image-bucket \
  -e MINIO_ACCESS_KEY_ID=YOUR_ACCESS_KEY \
  -e MINIO_SECRET_ACCESS_KEY=YOUR_SECRET_KEY \
  -e MINIO_REGION=us-east-1 \
  --name image-resizer-app \
  ghcr.io/vaam-store/image-resizer:s3-latest
```

(`ghcr.io/vaam-store/image-resizer:s3-latest`, built from the `s3_deploy`
target, is what `.github/workflows/build.yml` publishes - only build
locally with `docker build --target s3_deploy` if you need an
unpublished change.)

### Using Docker Compose

A `compose.yaml` file is provided for easier local development and deployment.

```bash
# Start the service
docker-compose up -d

# Stop the service
docker-compose down

# View logs
docker-compose logs -f
```

The `compose.yaml` file typically includes:
- The application service definition.
- Potentially other services like a local MinIO instance for S3 testing.

Refer to the `compose.yaml` in the project root for the exact configuration.

## Managing the Container

- **View logs**: `docker logs image-resizer-app`
- **Stop the container**: `docker stop image-resizer-app`
- **Start the container**: `docker start image-resizer-app`
- **Remove the container**: `docker rm image-resizer-app`

## Pushing to a Docker Registry

If you want to deploy the image to a remote environment (like Kubernetes), you'll need to push it to a Docker registry (e.g., Docker Hub, AWS ECR, Google GCR).

```bash
# Tag the image (replace <your-registry-username> and <repository-name>)
docker tag image-resizer:latest <your-registry-username>/<repository-name>:latest

# Log in to your Docker registry
docker login

# Push the image
docker push <your-registry-username>/<repository-name>:latest