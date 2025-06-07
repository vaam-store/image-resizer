# Architecture Overview

The Image Resize Service is designed with a modular architecture to provide high performance, scalability, and flexibility.

## High-Level Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   Client    │────▶│  API Layer  │────▶│  Services   │
└─────────────┘     └─────────────┘     └─────────────┘
                          │                    │
                          ▼                    ▼
                    ┌─────────────┐     ┌─────────────┐
                    │   Router    │     │   Storage   │
                    └─────────────┘     └─────────────┘
                                              │
                                              ▼
                                        ┌─────────────┐
                                        │    Cache    │
                                        └─────────────┘
```

## Key Components

### API Layer

The API layer handles incoming HTTP requests, validates parameters, and routes requests to the appropriate service.

### Router

The router manages HTTP routing, middleware integration, and request/response handling.

### Services

The service layer contains the core business logic:

- **Resize Service**: Handles image resizing operations
- **Image Service**: Manages image processing and manipulation
- **Storage Service**: Interfaces with storage backends
- **Cache Service**: Provides caching functionality
- **Health Service**: Monitors system health
- **Metrics Service**: Collects and exposes metrics

### Storage

The storage component provides a unified interface for different storage backends:

- S3 Storage
- Local Filesystem Storage
- In-Memory Storage

### Cache

The cache component improves performance by storing frequently accessed images.

## Request Flow

1. Client sends a request to resize an image
2. API layer receives the request and validates parameters
3. Router routes the request to the Resize Service
4. Resize Service checks the Cache for the requested image
5. If not in cache, Resize Service retrieves the image from the source URL
6. Resize Service processes the image according to the requested parameters
7. Processed image is stored in the Cache
8. Processed image is returned to the client

## Technology Stack

- **Language**: Rust
- **Web Framework**: Axum/Actix/Rocket (based on project structure)
- **Image Processing**: image-rs
- **Storage**: AWS SDK for S3, local filesystem
- **Containerization**: Docker
- **Orchestration**: Kubernetes via Helm
- **Monitoring**: Prometheus metrics