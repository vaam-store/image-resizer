use async_trait::async_trait;
use axum::extract::*;
use axum_extra::extract::CookieJar;
use bytes::Bytes;
use headers::Host;
use http::Method;
use serde::{Deserialize, Serialize};

use crate::{models, types::*};

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[must_use]
#[allow(clippy::large_enum_variant)]
pub enum DownloadResponse {
    /// Operation performed successfully.
    Status200_OperationPerformedSuccessfully
    {
        body: ByteArray,
        cache_control:
        Option<
        String
        >
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[must_use]
#[allow(clippy::large_enum_variant)]
pub enum ResizeResponse {
    /// The image was resize and in the location you'll get the link to it
    Status301_TheImageWasResizeAndInTheLocationYou
    {
        location:
        Option<
        String
        >
    }
}




/// Images
#[async_trait]
#[allow(clippy::ptr_arg)]
pub trait Images<E: std::fmt::Debug + Send + Sync + 'static = ()>: super::ErrorHandler<E> {
    /// Resize an image.
    ///
    /// Download - GET /api/images/files/{key}
    async fn download(
    &self,
    
    method: &Method,
    host: &Host,
    cookies: &CookieJar,
      path_params: &models::DownloadPathParams,
    ) -> Result<DownloadResponse, E>;

    /// Resize an image.
    ///
    /// Resize - GET /api/images/resize
    async fn resize(
    &self,
    
    method: &Method,
    host: &Host,
    cookies: &CookieJar,
      query_params: &models::ResizeQueryParams,
    ) -> Result<ResizeResponse, E>;
}
