mod modules;
mod services;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create API service
    let api_service = std::sync::Arc::new(crate::modules::api::handler::ApiService::create()?);

    // Create a basic router
    let app =
        axum::Router::new().route("/", axum::routing::get(|| async { "Image Resize Service" }));

    // Start the server
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Server running on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}
