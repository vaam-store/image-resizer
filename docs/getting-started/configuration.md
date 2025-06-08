# Configuration

The Image Resize Service can be configured using environment variables or a `.env` file in the project root.

## Environment Variables

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `PORT` | HTTP server port | `8080` | No |
| `LOG_LEVEL` | Log level (debug, info, warn, error) | `info` | No |
| `STORAGE_TYPE` | Storage backend type (s3, local, memory) | `memory` | No |
| `CACHE_ENABLED` | Enable caching | `true` | No |
| `CACHE_TTL_SECONDS` | Cache time-to-live in seconds | `3600` | No |

## Storage Configuration

### S3 Storage

When using `STORAGE_TYPE=s3`, the following additional variables are required:

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `S3_BUCKET` | S3 bucket name | - | Yes |
| `S3_REGION` | AWS region | - | Yes |
| `AWS_ACCESS_KEY_ID` | AWS access key ID | - | Yes |
| `AWS_SECRET_ACCESS_KEY` | AWS secret access key | - | Yes |
| `S3_ENDPOINT` | Custom S3 endpoint (for MinIO, etc.) | - | No |

### Local Filesystem Storage

When using `STORAGE_TYPE=local`, the following additional variables are required:

| Variable | Description | Default | Required |
|----------|-------------|---------|----------|
| `LOCAL_STORAGE_PATH` | Path to local storage directory | `./storage` | No |

## Example .env File

```dotenv
PORT=8080
LOG_LEVEL=info
STORAGE_TYPE=s3
CACHE_ENABLED=true
CACHE_TTL_SECONDS=3600

# S3 Configuration
S3_BUCKET=my-images
S3_REGION=us-west-2
AWS_ACCESS_KEY_ID=your-access-key
AWS_SECRET_ACCESS_KEY=your-secret-key
```

## Docker Environment Variables

When running with Docker, you can pass environment variables using the `-e` flag:

```bash
docker run -p 8080:8080 \
  -e STORAGE_TYPE=s3 \
  -e S3_BUCKET=my-images \
  -e S3_REGION=us-west-2 \
  -e AWS_ACCESS_KEY_ID=your-access-key \
  -e AWS_SECRET_ACCESS_KEY=your-secret-key \
  image-resizer:latest
```

## Helm Chart Configuration

When deploying with Helm, you can configure the service by modifying the `values.yaml` file. See the [Helm Chart](../deployment/helm-chart.md) documentation for details.