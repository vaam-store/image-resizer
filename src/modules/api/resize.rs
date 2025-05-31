use crate::modules::api::handler::ApiService;
use async_trait::async_trait;
use axum::http::Method;
use axum_extra::extract::{CookieJar, Host};
use gen_server::apis::images::{DownloadResponse, Images, ResizeResponse};
use gen_server::models::{DownloadPathParams, ResizeQueryParams};
use gen_server::types::ByteArray;

#[async_trait]
impl Images for ApiService {
    async fn resize(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        query_params: &ResizeQueryParams,
    ) -> Result<ResizeResponse, ()> {
        let url = self.resize_service.resize(query_params).await;

        match url {
            Ok(url) => Ok(
                ResizeResponse::Status302_TheImageWasResizeAndInTheLocationYou {
                    location: Some(url),
                },
            ),
            Err(_) => Err(()),
        }
    }

    // TODO This is having an issue when downloading
    async fn download(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        path_params: &DownloadPathParams,
    ) -> Result<DownloadResponse, ()> {
        let byte_array = self.resize_service.download(path_params).await;

        match byte_array {
            Ok(data) => Ok(DownloadResponse::Status200_OperationPerformedSuccessfully(
                ByteArray(data),
            )),
            Err(_) => Err(()),
        }
    }
}
