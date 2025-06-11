use futures::future::join_all;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🚀 Image Resize Performance Benchmark");
    println!("=====================================");

    // Concurrency levels to test
    let concurrency_levels = vec![1, 5, 10, 20, 50];

    for concurrency in concurrency_levels {
        println!("\n📊 Testing with {} concurrent requests", concurrency);
        println!("----------------------------------------");

        let start_time = Instant::now();
        let mut tasks = Vec::new();

        for i in 0..concurrency {
            let task = tokio::spawn(async move {
                let client = reqwest::Client::new();
                let request_start = Instant::now();

                // Test URLs - create them inside the task to avoid lifetime issues
                let test_urls = [
                    "https://picsum.photos/1920/1080", // Large image
                    "https://picsum.photos/800/600",   // Medium image
                    "https://picsum.photos/400/300",   // Small image
                ];

                let resize_params = [
                    (Some(300), Some(300)),  // Thumbnail
                    (Some(800), None),       // Width only
                    (None, Some(600)),       // Height only
                    (Some(1200), Some(800)), // Large resize
                ];

                let url = test_urls[i % test_urls.len()];
                let (width, height) = resize_params[i % resize_params.len()];

                // Simulate resize request
                let params = format!(
                    "?width={}&height={}&format=jpg",
                    width.map_or("".to_string(), |w| w.to_string()),
                    height.map_or("".to_string(), |h| h.to_string())
                );

                let encoded_url = urlencoding::encode(url);
                let url_with_params =
                    format!("http://localhost:8080/resize?url={}{}", encoded_url, params);

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
        sleep(Duration::from_secs(2)).await;
    }

    println!("\n🎯 Performance Recommendations:");
    println!("================================");
    println!("1. Monitor CPU usage during peak load");
    println!("2. Check memory consumption patterns");
    println!("3. Verify network bandwidth utilization");
    println!("4. Test with different image sizes and formats");
    println!("5. Profile with tools like `perf` or `flamegraph`");

    Ok(())
}
