use crate::modules::api::handler::ApiService;
use async_trait::async_trait;
use axum::http::Method;
use axum_extra::extract::{CookieJar, Host};
use gen_server::apis::image::{Image, ResizeAnImageResponse};
use gen_server::models::ResizeAnImageQueryParams;

#[async_trait]
impl Image for ApiService {
    async fn resize_an_image(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        query_params: &ResizeAnImageQueryParams,
    ) -> Result<ResizeAnImageResponse, ()> {
        let url = self.resize_service.resize(query_params).await;
        match url {
            Ok(url) => Ok(
                ResizeAnImageResponse::Status302_TheImageWasResizeAndInTheLocationYou {
                    location: Some(url),
                },
            ),
            Err(_) => Err(()),
        }
    }
}
