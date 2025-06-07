# API Reference

The Image Resize Service provides a RESTful API for resizing and manipulating images.

## OpenAPI Specification

The complete API specification is available in OpenAPI format in the [openapi.yaml](https://github.com/sse/image-resize/blob/main/openapi.yaml) file in the repository root.

## Base URL

```
http://localhost:8080/api/v1
```

When deployed, replace `localhost:8080` with your actual service domain.

## Endpoints

### Resize Image

```
GET /resize
```

Resizes an image according to the specified parameters.

#### Query Parameters

| Parameter | Type | Description | Required |
|-----------|------|-------------|----------|
| `url` | string | URL of the source image | Yes |
| `width` | integer | Target width in pixels | No |
| `height` | integer | Target height in pixels | No |
| `format` | string | Output format (jpeg, png, webp) | No |
| `quality` | integer | Output quality (1-100, for jpeg and webp) | No |
| `fit` | string | Fit method (cover, contain, fill, inside, outside) | No |

#### Example Request

```
GET /api/v1/resize?url=https://example.com/image.jpg&width=800&height=600&format=webp&quality=90&fit=cover
```

#### Response

The response is the resized image in the requested format.

#### Status Codes

| Status Code | Description |
|-------------|-------------|
| 200 | Success |
| 400 | Bad Request - Invalid parameters |
| 404 | Not Found - Source image not found |
| 500 | Internal Server Error |

### Health Check

```
GET /health
```

Returns the health status of the service.

#### Example Request

```
GET /api/v1/health
```

#### Example Response

```json
{
  "status": "ok",
  "version": "1.0.0"
}
```

### Metrics

```
GET /metrics
```

Returns service metrics in Prometheus format.

#### Example Request

```
GET /api/v1/metrics
```

## Error Responses

Error responses are returned in JSON format:

```json
{
  "error": {
    "code": "BAD_REQUEST",
    "message": "Invalid width parameter"
  }
}