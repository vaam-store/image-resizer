use prometheus::{Encoder, TextEncoder, gather};

pub async fn metrics_handler() -> String {
    let mut buffer = Vec::new();
    let encoder = TextEncoder::new();
    encoder.encode(&gather(), &mut buffer).unwrap();
    // return metrics
    String::from_utf8(buffer).unwrap()
}