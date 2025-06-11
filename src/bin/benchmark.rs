use anyhow::Result;
use envconfig::Envconfig;
use futures::future::join_all;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Envconfig, Clone, Debug)]
pub struct BenchmarkConfig {
    #[envconfig(from = "BENCHMARK_HOST", default = "localhost")]
    pub host: String,

    #[envconfig(from = "BENCHMARK_PORT", default = "8080")]
    pub port: u16,

    #[envconfig(from = "BENCHMARK_CONCURRENCY_LEVELS", default = "1,5,10,20,50")]
    pub concurrency_levels: String,

    #[envconfig(
        from = "BENCHMARK_TEST_URLS",
        default = "https://picsum.photos/1920/1080,https://picsum.photos/800/600,https://picsum.photos/400/300"
    )]
    pub test_urls: String,

    #[envconfig(
        from = "BENCHMARK_RESIZE_PARAMS",
        default = "300x300,800x,x600,1200x800"
    )]
    pub resize_params: String,

    #[envconfig(from = "BENCHMARK_WAIT_BETWEEN_TESTS", default = "4")]
    pub wait_between_tests: u64,

    #[envconfig(from = "BENCHMARK_REQUEST_TIMEOUT", default = "60")]
    pub request_timeout: u64,

    #[envconfig(from = "BENCHMARK_OUTPUT_FORMAT", default = "jpg")]
    pub output_format: String,
}

impl BenchmarkConfig {
    /// Parse concurrency levels from comma-separated string
    pub fn get_concurrency_levels(&self) -> Vec<usize> {
        self.concurrency_levels
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// Parse test URLs from comma-separated string
    pub fn get_test_urls(&self) -> Vec<String> {
        self.test_urls
            .split(',')
            .map(|s| s.trim().to_string())
            .collect()
    }

    /// Parse resize parameters from comma-separated string
    /// Format: "WIDTHxHEIGHT" where WIDTH or HEIGHT can be empty for aspect ratio preservation
    pub fn get_resize_params(&self) -> Vec<(Option<u32>, Option<u32>)> {
        self.resize_params
            .split(',')
            .filter_map(|s| {
                let s = s.trim();
                if let Some((width_str, height_str)) = s.split_once('x') {
                    let width = if width_str.is_empty() {
                        None
                    } else {
                        width_str.parse().ok()
                    };
                    let height = if height_str.is_empty() {
                        None
                    } else {
                        height_str.parse().ok()
                    };
                    Some((width, height))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get the base URL for the benchmark target
    pub fn get_base_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<(), String> {
        if self.get_concurrency_levels().is_empty() {
            return Err("No valid concurrency levels configured".to_string());
        }

        if self.get_test_urls().is_empty() {
            return Err("No valid test URLs configured".to_string());
        }

        if self.get_resize_params().is_empty() {
            return Err("No valid resize parameters configured".to_string());
        }

        if self.request_timeout == 0 {
            return Err("Request timeout must be greater than 0".to_string());
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = BenchmarkConfig::init_from_env()?;

    // Validate configuration
    if let Err(e) = config.validate() {
        eprintln!("❌ Configuration error: {}", e);
        return Ok(());
    }

    println!("🚀 Image Resize Performance Benchmark");
    println!("=====================================");
    println!("📋 Configuration:");
    println!("   Host: {}", config.host);
    println!("   Port: {}", config.port);
    println!(
        "   Concurrency levels: {:?}",
        config.get_concurrency_levels()
    );
    println!("   Test URLs count: {}", config.get_test_urls().len());
    println!(
        "   Resize params count: {}",
        config.get_resize_params().len()
    );
    println!("   Output format: {}", config.output_format);
    println!("   Request timeout: {}s", config.request_timeout);
    println!("   Wait between tests: {}s", config.wait_between_tests);
    println!();

    let concurrency_levels = config.get_concurrency_levels();
    let test_urls = config.get_test_urls();
    let resize_params = config.get_resize_params();

    for concurrency in &concurrency_levels {
        println!("\n📊 Testing with {} concurrent requests", *concurrency);
        println!("----------------------------------------");

        let start_time = Instant::now();
        let mut tasks = Vec::new();

        for i in 0..*concurrency {
            let config_clone = config.clone();
            let test_urls_clone = test_urls.clone();
            let resize_params_clone = resize_params.clone();

            let task = tokio::spawn(async move {
                let client = reqwest::Client::builder()
                    .timeout(Duration::from_secs(config_clone.request_timeout))
                    .build()
                    .unwrap();
                let request_start = Instant::now();

                let url = &test_urls_clone[i % test_urls_clone.len()];
                let (width, height) = resize_params_clone[i % resize_params_clone.len()];

                // Build query parameters
                let mut query_params = Vec::new();
                if let Some(w) = width {
                    query_params.push(format!("width={}", w));
                }
                if let Some(h) = height {
                    query_params.push(format!("height={}", h));
                }
                query_params.push(format!("format={}", config_clone.output_format));

                let params = if query_params.is_empty() {
                    String::new()
                } else {
                    format!("&{}", query_params.join("&"))
                };

                let encoded_url = urlencoding::encode(url);
                let url_with_params = format!(
                    "{}/api/images/resize?url={}{}",
                    config_clone.get_base_url(),
                    encoded_url,
                    params
                );

                match client.get(&url_with_params).send().await {
                    Ok(response) => {
                        let status = response.status();
                        let duration = request_start.elapsed();
                        (status.is_success(), duration, response.content_length())
                    }
                    Err(_) => (false, request_start.elapsed(), None),
                }
            });

            tasks.push(task);
        }

        let results = join_all(tasks).await;
        let total_duration = start_time.elapsed();

        // Calculate statistics
        let mut successful_requests = 0;
        let mut total_response_time = Duration::new(0, 0);
        let mut min_response_time = Duration::from_secs(u64::MAX);
        let mut max_response_time = Duration::new(0, 0);
        let mut total_bytes = 0u64;

        for result in results {
            if let Ok((success, duration, content_length)) = result {
                if success {
                    successful_requests += 1;
                    total_response_time += duration;
                    min_response_time = min_response_time.min(duration);
                    max_response_time = max_response_time.max(duration);
                    if let Some(bytes) = content_length {
                        total_bytes += bytes;
                    }
                }
            }
        }

        if successful_requests > 0 {
            let avg_response_time = total_response_time / successful_requests;
            let requests_per_second = successful_requests as f64 / total_duration.as_secs_f64();
            let throughput_mbps =
                (total_bytes as f64 / (1024.0 * 1024.0)) / total_duration.as_secs_f64();

            println!(
                "✅ Successful requests: {}/{}",
                successful_requests, concurrency
            );
            println!("⏱️  Total time: {:.2}s", total_duration.as_secs_f64());
            println!("📈 Requests/sec: {:.2}", requests_per_second);
            println!("🚀 Throughput: {:.2} MB/s", throughput_mbps);
            println!(
                "⚡ Avg response time: {:.2}ms",
                avg_response_time.as_millis()
            );
            println!(
                "🔥 Min response time: {:.2}ms",
                min_response_time.as_millis()
            );
            println!(
                "🐌 Max response time: {:.2}ms",
                max_response_time.as_millis()
            );
        } else {
            println!("❌ All requests failed");
        }

        // Wait between tests
        sleep(Duration::from_secs(config.wait_between_tests)).await;
    }

    println!("\n🎯 Performance Recommendations:");
    println!("================================");
    println!("1. Monitor CPU usage during peak load");
    println!("2. Check memory consumption patterns");
    println!("3. Verify network bandwidth utilization");
    println!("4. Test with different image sizes and formats");
    println!("5. Profile with tools like `perf` or `flamegraph`");
    println!("\n💡 Configuration Tips:");
    println!("- Use BENCHMARK_HOST and BENCHMARK_PORT to target different servers");
    println!("- Customize BENCHMARK_CONCURRENCY_LEVELS (e.g., '1,10,50,100')");
    println!("- Add your own test URLs with BENCHMARK_TEST_URLS");
    println!(
        "- Configure resize parameters with BENCHMARK_RESIZE_PARAMS (e.g., '100x100,500x,x300')"
    );

    Ok(())
}
