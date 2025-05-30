use axum::{Router, response::IntoResponse, routing::get};

#[tokio::main]
async fn main() {
    // Build our application with a single route
    let app = Router::new().route("/", get(|| async { "Hello, World!" }));

    // Run it with hyper on localhost:3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}
