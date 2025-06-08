# Installation

This guide will help you install and run the Image Resize Service.

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) 1.70 or later
- [Docker](https://docs.docker.com/get-docker/) (optional, for containerized deployment)
- [Kubernetes](https://kubernetes.io/docs/setup/) (optional, for Kubernetes deployment)
- [Helm](https://helm.sh/docs/intro/install/) (optional, for Helm chart deployment)

## Local Development Setup

### Clone the Repository

```bash
git clone https://github.com/vymalo/image-resizer.git
cd image-resizer
```

### Build and Run Locally

```bash
# Build the project
cargo build

# Run the service
cargo run
```

The service will be available at `http://localhost:8080`.

## Docker Deployment

### Using Docker Compose

The easiest way to get started is using Docker Compose:

```bash
# Start the service
docker-compose up -d

# Check logs
docker-compose logs -f
```

### Building and Running the Docker Image

```bash
# Build the Docker image
docker build -t image-resizer:latest .

# Run the container
docker run -p 8080:8080 image-resizer:latest
```

## Kubernetes Deployment

See the [Helm Chart](../deployment/helm-chart.md) documentation for details on deploying to Kubernetes.