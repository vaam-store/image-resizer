# Image Resize Service

Welcome to the documentation for the Image Resize Service, a high-performance image resizing API built with Rust.

## Features

- Fast image resizing with multiple storage backends
- Caching for improved performance
- Metrics and health monitoring
- Kubernetes deployment support via Helm
- Configurable storage options (S3, local filesystem, in-memory)

## Quick Start

```bash
# Clone the repository
git clone https://github.com/vaam-store/image-resizer.git
cd image-resizer

# Build and run with Docker Compose
docker-compose up -d
```

Visit the [Getting Started](getting-started/installation.md) section for more detailed instructions.

## API Overview

The Image Resize Service provides a RESTful API for resizing and manipulating images. See the [API Reference](user-guide/api-reference.md) for detailed documentation.

## Architecture

This service is built with a modular architecture using Rust for high performance. Learn more about the [architecture](architecture/overview.md) and [components](architecture/components.md).

## Deployment

The service can be deployed using [Docker](deployment/docker.md) or [Kubernetes with Helm](deployment/helm-chart.md).