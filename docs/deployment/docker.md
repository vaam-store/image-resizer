# Docker Deployment

This guide explains how to deploy the Image Resize Service using Docker.

## Prerequisites

- Docker installed

## Building the Docker Image

A `Dockerfile` is provided in the project root.

```bash
# Navigate to the project root
cd /path/to/image-resize

# Build the Docker image
docker build -t image-resize:latest .
```

## Running the Docker Container

### Basic Run

```bash
docker run -d -p 8080:8080 --name image-resize-app image-resize:latest
```

This will run the service in detached mode and map port 8080 of the container to port 8080 on the host.

### With Environment Variables

You can configure the service using environment variables. See the [Configuration](../getting-started/configuration.md) guide for available variables.

```bash
docker run -d -p 8080:8080 \
  -e PORT=8080 \
  -e STORAGE_TYPE=s3 \
  -e S3_BUCKET=my-image-bucket \
  -e AWS_ACCESS_KEY_ID=YOUR_ACCESS_KEY \
  -e AWS_SECRET_ACCESS_KEY=YOUR_SECRET_KEY \
  -e S3_REGION=us-east-1 \
  --name image-resize-app \
  image-resize:latest
```

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

- **View logs**: `docker logs image-resize-app`
- **Stop the container**: `docker stop image-resize-app`
- **Start the container**: `docker start image-resize-app`
- **Remove the container**: `docker rm image-resize-app`

## Pushing to a Docker Registry

If you want to deploy the image to a remote environment (like Kubernetes), you'll need to push it to a Docker registry (e.g., Docker Hub, AWS ECR, Google GCR).

```bash
# Tag the image (replace <your-registry-username> and <repository-name>)
docker tag image-resize:latest <your-registry-username>/<repository-name>:latest

# Log in to your Docker registry
docker login

# Push the image
docker push <your-registry-username>/<repository-name>:latest