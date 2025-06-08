# Components

This page provides detailed information about the key components of the Image Resize Service.

## API Module

The API module (`src/modules/api/`) handles the HTTP API interface and parameter validation.

### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Request handlers
- `resize.rs`: Resize API implementation

The API module is responsible for:
- Parsing and validating request parameters
- Converting API requests to service calls
- Formatting service responses as API responses
- Error handling and response codes

## Router Module

The Router module (`src/modules/router/`) manages HTTP routing and middleware.

### Key Files:
- `mod.rs`: Module definition
- `router.rs`: Route definitions
- `middlewares.rs`: HTTP middleware implementations

The Router module is responsible for:
- Defining API routes
- Applying middleware (logging, tracing, etc.)
- Request/response handling

## Services

### Resize Service

The Resize Service (`src/services/resize/`) handles image resizing operations.

#### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Resize implementation

The Resize Service is responsible for:
- Processing resize parameters
- Applying resize operations to images
- Handling different resize modes (fit, cover, etc.)

### Image Service

The Image Service (`src/services/image/`) handles image processing and manipulation.

#### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Image processing implementation

The Image Service is responsible for:
- Loading images from various sources
- Image format conversion
- Image quality adjustments

### Storage Service

The Storage Service (`src/services/storage/`) provides a unified interface for different storage backends.

#### Key Files:
- `mod.rs`: Module definition
- `core.rs`: Storage trait definitions
- `handler.rs`: Storage implementation
- `s3_handler.rs`: S3 storage implementation
- `local_fs_handler.rs`: Local filesystem implementation
- `in_memory_handler.rs`: In-memory storage implementation

The Storage Service is responsible for:
- Storing and retrieving images
- Managing storage backends
- Handling storage errors

### Cache Service

The Cache Service (`src/services/cache/`) provides caching functionality.

#### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Cache implementation

The Cache Service is responsible for:
- Caching processed images
- Cache invalidation
- Cache hit/miss metrics

### Health Service

The Health Service (`src/services/health/`) monitors system health.

#### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Health check implementation

The Health Service is responsible for:
- Providing health check endpoints
- Monitoring system components
- Reporting system status

### Metrics Service

The Metrics Service (`src/services/metrics/`) collects and exposes metrics.

#### Key Files:
- `mod.rs`: Module definition
- `handler.rs`: Metrics implementation

The Metrics Service is responsible for:
- Collecting performance metrics
- Exposing Prometheus-compatible metrics
- Monitoring system usage

## Utility Modules

### Tracer Module

The Tracer Module (`src/modules/tracer/`) provides distributed tracing functionality.

#### Key Files:
- `mod.rs`: Module definition
- `init.rs`: Tracer initialization

### Utils Module

The Utils Module (`src/modules/utils/`) provides utility functions.

#### Key Files:
- `mod.rs`: Module definition
- `date.rs`: Date/time utilities
- `err.rs`: Error handling utilities

## Models

The Models (`src/models/`) define data structures used throughout the application.

### Key Files:
- `mod.rs`: Module definition
- `params.rs`: API parameter definitions