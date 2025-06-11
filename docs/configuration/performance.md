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

## Benchmark Configuration

The included benchmark tool is now fully configurable through environment variables, allowing you to customize performance testing for your specific environment and requirements.

### Benchmark Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `BENCHMARK_HOST` | `localhost` | Target host for benchmark requests |
| `BENCHMARK_PORT` | `8080` | Target port for benchmark requests |
| `BENCHMARK_CONCURRENCY_LEVELS` | `1,5,10,20,50` | Comma-separated list of concurrency levels to test |
| `BENCHMARK_TEST_URLS` | `https://picsum.photos/...` | Comma-separated list of test image URLs |
| `BENCHMARK_RESIZE_PARAMS` | `300x300,800x,x600,1200x800` | Comma-separated list of resize parameters (format: `WIDTHxHEIGHT`) |
| `BENCHMARK_WAIT_BETWEEN_TESTS` | `2` | Seconds to wait between different concurrency level tests |
| `BENCHMARK_REQUEST_TIMEOUT` | `30` | Request timeout in seconds |
| `BENCHMARK_OUTPUT_FORMAT` | `jpg` | Output image format for resize requests |

### Resize Parameters Format

The `BENCHMARK_RESIZE_PARAMS` variable accepts parameters in the format `WIDTHxHEIGHT`:
- `300x300` - Resize to 300x300 pixels
- `800x` - Resize to 800 pixels width, maintain aspect ratio
- `x600` - Resize to 600 pixels height, maintain aspect ratio
- `1200x800` - Resize to 1200x800 pixels

### Benchmark Examples

#### Basic Benchmark
```bash
# Run benchmark with default settings
cargo run --bin benchmark
```

#### Custom Target Server
```bash
# Test against a different server
export BENCHMARK_HOST=production-server.com
export BENCHMARK_PORT=443
cargo run --bin benchmark
```

#### High Concurrency Testing
```bash
# Test with higher concurrency levels
export BENCHMARK_CONCURRENCY_LEVELS=1,10,25,50,100,200
export BENCHMARK_REQUEST_TIMEOUT=60
cargo run --bin benchmark
```

#### Custom Test Images
```bash
# Use your own test images
export BENCHMARK_TEST_URLS="https://example.com/image1.jpg,https://example.com/image2.png,https://example.com/image3.webp"
export BENCHMARK_RESIZE_PARAMS="100x100,500x500,1000x,x800"
export BENCHMARK_OUTPUT_FORMAT=webp
cargo run --bin benchmark
```

#### Quick Performance Check
```bash
# Fast benchmark for CI/CD pipelines
export BENCHMARK_CONCURRENCY_LEVELS=1,5,10
export BENCHMARK_WAIT_BETWEEN_TESTS=1
export BENCHMARK_REQUEST_TIMEOUT=15
cargo run --bin benchmark
```

### Benchmark Output

The benchmark provides detailed performance metrics:
- **Successful requests** - Number of successful vs total requests
- **Total time** - Time taken for all requests at each concurrency level
- **Requests/sec** - Throughput measurement
- **Throughput** - Data transfer rate in MB/s
- **Response times** - Average, minimum, and maximum response times

### Integration with Performance Profiles

You can combine benchmark configuration with performance profiles to test different server configurations:

```bash
# Test high throughput profile
export PERFORMANCE_PROFILE=high_throughput
./emgr &

# Run benchmark against high throughput configuration
export BENCHMARK_CONCURRENCY_LEVELS=10,50,100,200
export BENCHMARK_REQUEST_TIMEOUT=60
cargo run --bin benchmark

# Stop server and test memory efficient profile
killall emgr
export PERFORMANCE_PROFILE=memory_efficient
./emgr &

# Run benchmark with lower concurrency
export BENCHMARK_CONCURRENCY_LEVELS=1,5,10
cargo run --bin benchmark
```

This configurable approach allows you to:
1. **Test different environments** - development, staging, production
2. **Validate performance profiles** - ensure profiles meet your requirements
3. **Automate performance testing** - integrate into CI/CD pipelines
4. **Custom load patterns** - simulate your specific usage patterns