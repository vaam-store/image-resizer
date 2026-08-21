# Docker Deployment

This guide explains how to build and run `emgr` with Docker.

## Prerequisites

- Docker, with Buildx (for `docker build`).
- Docker Compose v2 (`docker compose`, not the standalone `docker-compose`)
  if you use `compose.yaml`.

## Building the Docker image

The `Dockerfile` in the project root defines **four deploy targets** and no
default/unnamed final stage, so `--target` is required:

| Target | Storage | OpenTelemetry / `/metrics` |
|---|---|---|
| `fs_deploy` | local filesystem | no |
| `fs_otel_deploy` | local filesystem | yes |
| `s3_deploy` | S3 / MinIO | no |
| `s3_otel_deploy` | S3 / MinIO | yes |

`healthcheck` (the binary the image's own `HEALTHCHECK` runs) is built into
all four automatically.

```bash
cd /path/to/image-resizer

# Build the image (local filesystem storage, no OTel)
docker build --target fs_deploy -t emgr:latest .
```

### The builder/runtime base image pin - do not bump casually

The builder stage is pinned by digest to a **Debian 12 ("bookworm")** Rust
image, and the runtime stage to `gcr.io/distroless/cc-debian12` - on
purpose, and they must move together. A previous version of this Dockerfile
used a Debian 13 ("trixie") builder (glibc 2.41) against this same
bookworm-based runtime (glibc 2.36); the `s3` build's native C dependency
(`aws-lc-sys`) referenced `GLIBC_2.38` symbols, and the resulting image
built cleanly, pushed successfully, and then died instantly on start with:

```text
version 'GLIBC_2.38' not found
```

CI never caught it, because the build pipeline built and pushed images
without ever running one - see the "run every built image before pushing
it" fix (`.github/workflows/build.yml`) that closed that gap. `local_fs`
survived only by luck (it happens not to touch the symbols in question).

If you ever need to bump either base image, bump both in lockstep and
actually run the resulting image before merging - don't rely on CI's
static checks to catch a glibc mismatch, they won't.

## Running the container

`emgr` fails closed at startup - see
[Installation](../getting-started/installation.md#configure-the-two-startup-checks)
for the full explanation. In short, every run needs:

- `SIGNING_KEY` + `SIGNING_SALT` (hex-encoded), or `ALLOW_UNSIGNED_REQUESTS=true`.
- On an `*_otel_deploy` image only: `METRICS_AUTH_TOKEN`, or
  `ALLOW_UNAUTHENTICATED_METRICS=true`.

A container started without satisfying these exits immediately with a
clear error - it does not hang or serve broken responses.

### Basic run (local filesystem, signing disabled for local testing)

The service listens on port `3000` by default (`PORT`, `HOST`):

```bash
docker run -d -p 3000:3000 \
  -e ALLOW_UNSIGNED_REQUESTS=true \
  -e LOCAL_FS_STORAGE_PATH=/app/data/images \
  --name emgr-app \
  emgr:latest
```

### With S3/MinIO storage and real signing

You can configure the service using environment variables - see
[Configuration](../getting-started/configuration.md) for the full list.

```bash
docker run -d -p 3000:3000 \
  -e SIGNING_KEY=$(openssl rand -hex 32) \
  -e SIGNING_SALT=$(openssl rand -hex 16) \
  -e STORAGE_TYPE=S3 \
  -e MINIO_ENDPOINT_URL=https://s3.amazonaws.com \
  -e MINIO_BUCKET=my-image-bucket \
  -e MINIO_ACCESS_KEY_ID=YOUR_ACCESS_KEY \
  -e MINIO_SECRET_ACCESS_KEY=YOUR_SECRET_KEY \
  -e MINIO_REGION=us-east-1 \
  --name emgr-app \
  ghcr.io/vaam-store/image-resizer:s3-latest
```

`ghcr.io/vaam-store/image-resizer:s3-latest` (built from the `s3_deploy`
target) is what `.github/workflows/build.yml` publishes on every push to
`main`, alongside `fs-latest`, `fs_otel-latest` and `s3_otel-latest` (each
also gets a per-commit `<flavor>-<sha>` tag). There is no floating,
un-prefixed `latest` tag - build.yml has no release-tag trigger today, so
none of these are semver-stable either; pin a specific `<flavor>-<sha>`
for anything beyond local testing. Only build locally with
`docker build --target s3_deploy` if you need an unpublished change.

### Using Docker Compose

`compose.yaml` in the project root brings up the `app` service (local
filesystem, `fs_otel_deploy` target) and `app-s3` (`s3_otel_deploy`
target, backed by a local MinIO), plus a Jaeger all-in-one container for
the OTel traces both services emit - both are `*_otel_deploy` builds, so
both need `/metrics` auth configured, not just signing.

**As committed, neither service will start.** `compose.yaml`'s
`environment:` blocks for `app` and `app-s3` don't reference
`SIGNING_KEY`/`SIGNING_SALT`/`ALLOW_UNSIGNED_REQUESTS` or
`METRICS_AUTH_TOKEN`/`ALLOW_UNAUTHENTICATED_METRICS` at all - `.env`
(`cp .env.example .env`) does not fix this on its own, since Compose only
uses a root `.env` file for `${VAR}` substitution *inside* `compose.yaml`,
and nothing in `compose.yaml` reads those five variables into either
service. Confirm this yourself with `docker compose config` - the
resolved `environment:` for `app`/`app-s3` has no signing- or
metrics-auth-related keys, with or without a populated `.env`.

Until `compose.yaml` is updated to forward them, add an override file
(auto-loaded by `docker compose` alongside `compose.yaml`, no `-f` flag
needed):

```yaml
# compose.override.yaml (local only - not tracked in git)
services:
  app:
    environment:
      SIGNING_KEY: ${SIGNING_KEY}
      SIGNING_SALT: ${SIGNING_SALT}
      METRICS_AUTH_TOKEN: ${METRICS_AUTH_TOKEN}
  app-s3:
    environment:
      SIGNING_KEY: ${SIGNING_KEY}
      SIGNING_SALT: ${SIGNING_SALT}
      METRICS_AUTH_TOKEN: ${METRICS_AUTH_TOKEN}
```

```bash
cp .env.example .env   # also uncomment/set SIGNING_KEY, SIGNING_SALT, METRICS_AUTH_TOKEN in it

# Start everything
docker compose up -d --build

# Stop
docker compose down

# View logs
docker compose logs -f
```

The `Makefile` wraps the same compose invocations (`make up`, `make down`,
`make logs`, `make ps` - see `make help` for the full list) with the
project name pinned to `emgr`; the override file above applies to those
too, since `make` doesn't pass `-f`.

## Managing the container

- **View logs**: `docker logs emgr-app`
- **Stop the container**: `docker stop emgr-app`
- **Start the container**: `docker start emgr-app`
- **Remove the container**: `docker rm emgr-app`

## Pushing to a Docker registry

If you want to deploy a locally built image to a remote environment (like
Kubernetes), push it to a registry (Docker Hub, AWS ECR, Google GCR, ...):

```bash
# Tag the image (replace <your-registry-username> and <repository-name>)
docker tag emgr:latest <your-registry-username>/<repository-name>:latest

# Log in to your Docker registry
docker login

# Push the image
docker push <your-registry-username>/<repository-name>:latest
```

In this repository, `.github/workflows/build.yml` does this automatically
on every push, publishing all four flavours to
`ghcr.io/vaam-store/image-resizer` - see the tag naming note above.
