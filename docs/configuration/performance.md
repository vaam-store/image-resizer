# Performance Configuration

The image resize service now supports configuring performance parameters through environment variables, providing flexibility without requiring code changes.

## Environment Variables

### Basic Performance Settings

| Variable | Default | Description |
|----------|---------|-------------|
| `MAX_CONCURRENT_DOWNLOADS` | `20` | Maximum number of concurrent image downloads |
| `MAX_CONCURRENT_PROCESSING` | CPU count | Maximum number of concurrent image processing tasks |
| `HTTP_TIMEOUT_SECS` | `30` | HTTP client timeout in seconds |
| `MAX_IMAGE_SIZE_MB` | `50` | Maximum image size in megabytes |
| `CPU_THREAD_POOL_SIZE` | CPU count | Size of the CPU thread pool for image processing |
| `ENABLE_HTTP2` | `true` | Enable HTTP/2 for downloads |
| `CONNECTION_POOL_SIZE` | `50` | Connection pool size per host |
| `KEEP_ALIVE_TIMEOUT_SECS` | `60` | Keep-alive timeout for connections in seconds |

### Performance Profiles

You can use predefined performance profiles by setting the `PERFORMANCE_PROFILE` environment variable:

| Profile | Description |
|---------|-------------|
| `high_throughput` | Optimized for maximum throughput with higher resource usage |
| `low_latency` | Optimized for minimal response time |
| `memory_efficient` | Optimized for minimal memory usage |

When using a profile, individual environment variables will override the profile defaults.

## Examples

### Basic Configuration

```bash
# Set custom download limits
export MAX_CONCURRENT_DOWNLOADS=50
export HTTP_TIMEOUT_SECS=15

# Start the service
./emgr
```

### Using Performance Profiles

```bash
# Use high throughput profile
export PERFORMANCE_PROFILE=high_throughput

# Override specific settings
export MAX_CONCURRENT_DOWNLOADS=100

# Start the service
./emgr
```

### Memory-Constrained Environment

```bash
# Use memory efficient profile
export PERFORMANCE_PROFILE=memory_efficient

# Further reduce memory usage
export MAX_CONCURRENT_DOWNLOADS=3
export MAX_IMAGE_SIZE_MB=10

# Start the service
./emgr
```

## Profile Details

### High Throughput Profile
- `MAX_CONCURRENT_DOWNLOADS`: 50
- `MAX_CONCURRENT_PROCESSING`: CPU count × 2
- `HTTP_TIMEOUT_SECS`: 15
- `MAX_IMAGE_SIZE_MB`: 100
- `CPU_THREAD_POOL_SIZE`: CPU count
- `ENABLE_HTTP2`: true
- `CONNECTION_POOL_SIZE`: 100
- `KEEP_ALIVE_TIMEOUT_SECS`: 120

### Low Latency Profile
- `MAX_CONCURRENT_DOWNLOADS`: 10
- `MAX_CONCURRENT_PROCESSING`: CPU count
- `HTTP_TIMEOUT_SECS`: 10
- `MAX_IMAGE_SIZE_MB`: 20
- `CPU_THREAD_POOL_SIZE`: CPU count
- `ENABLE_HTTP2`: true
- `CONNECTION_POOL_SIZE`: 25
- `KEEP_ALIVE_TIMEOUT_SECS`: 30

### Memory Efficient Profile
- `MAX_CONCURRENT_DOWNLOADS`: 5
- `MAX_CONCURRENT_PROCESSING`: CPU count ÷ 2
- `HTTP_TIMEOUT_SECS`: 45
- `MAX_IMAGE_SIZE_MB`: 10
- `CPU_THREAD_POOL_SIZE`: CPU count ÷ 2
- `ENABLE_HTTP2`: false (HTTP/1.1 uses less memory)
- `CONNECTION_POOL_SIZE`: 10
- `KEEP_ALIVE_TIMEOUT_SECS`: 30

## Migration from Hardcoded Configuration

Previously, performance settings were hardcoded in the application. With this update:

1. **Default behavior remains the same** - if no environment variables are set, the service uses the same defaults as before
2. **Gradual migration** - you can override individual settings without changing everything at once
3. **Profile-based configuration** - use predefined profiles for common use cases
4. **Fine-tuning** - combine profiles with individual overrides for optimal performance

## Monitoring and Tuning

Monitor your application's performance metrics to determine optimal settings:

- **CPU usage** - adjust `MAX_CONCURRENT_PROCESSING` and `CPU_THREAD_POOL_SIZE`
- **Memory usage** - adjust `MAX_CONCURRENT_DOWNLOADS` and `MAX_IMAGE_SIZE_MB`
- **Network performance** - adjust `CONNECTION_POOL_SIZE` and `ENABLE_HTTP2`
- **Response times** - adjust `HTTP_TIMEOUT_SECS` and `KEEP_ALIVE_TIMEOUT_SECS`

Start with a profile that matches your use case, then fine-tune individual parameters based on your specific requirements and monitoring data.