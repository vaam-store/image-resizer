use std::collections::HashMap;

use axum::{body::Body, extract::*, response::Response, routing::*};
use axum_extra::{
    TypedHeader,
    extract::{CookieJar, Query as QueryExtra},
};
use bytes::Bytes;
use headers::Host;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header::CONTENT_TYPE};
use tracing::error;
use validator::{Validate, ValidationErrors};

#[allow(unused_imports)]
use crate::{apis, models};
use crate::{header, types::*};
#[allow(unused_imports)]
use crate::{
    models::check_xss_map, models::check_xss_map_nested, models::check_xss_map_string,
    models::check_xss_string, models::check_xss_vec_string,
};


/// Setup API Server.
pub fn new<I, A, E>(api_impl: I) -> Router
where
    I: AsRef<A> + Clone + Send + Sync + 'static,
    A: apis::images::Images<E> + Send + Sync + 'static,
    E: std::fmt::Debug + Send + Sync + 'static,
    
{
    // build our application with a route
    Router::new()
        .route("/api/images/files/{key}",
            get(download::<I, A, E>)
        )
        .route("/api/images/resize",
            get(resize::<I, A, E>)
        )
        .with_state(api_impl)
}


#[tracing::instrument(skip_all)]
fn download_validation(
  path_params: models::DownloadPathParams,
) -> std::result::Result<(
  models::DownloadPathParams,
), ValidationErrors>
{
  path_params.validate()?;

Ok((
  path_params,
))
}
/// Download - GET /api/images/files/{key}
#[tracing::instrument(skip_all)]
async fn download<I, A, E>(
  method: Method,
  TypedHeader(host): TypedHeader<Host>,
  cookies: CookieJar,
  Path(path_params): Path<models::DownloadPathParams>,
 State(api_impl): State<I>,
) -> Result<Response, StatusCode>
where
    I: AsRef<A> + Send + Sync,
    A: apis::images::Images<E> + Send + Sync,
    E: std::fmt::Debug + Send + Sync + 'static,
        {




      #[allow(clippy::redundant_closure)]
      let validation = tokio::task::spawn_blocking(move ||
    download_validation(
        path_params,
    )
  ).await.unwrap();

  let Ok((
    path_params,
  )) = validation else {
    return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(validation.unwrap_err().to_string()))
            .map_err(|_| StatusCode::BAD_REQUEST);
  };



  let result = api_impl.as_ref().download(
      
      &method,
      &host,
      &cookies,
        &path_params,
  ).await;

  let resp = match result {
                                            Ok(rsp) => match rsp {
                                                apis::images::DownloadResponse::Status200_OperationPerformedSuccessfully
                                                    {
                                                        body,
                                                        cache_control
                                                    }
                                                => {
                                                let mut response = Response::builder();
                    if let Some(cache_control) = cache_control {
                        let cache_control = match header::IntoHeaderValue(cache_control).try_into() {
                            Ok(val) => val,
                            Err(e) => {
                                return Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(Body::from(format!("An internal server error occurred handling cache_control header - {e}"))).map_err(|e| { error!(error = ?e); StatusCode::INTERNAL_SERVER_ERROR });
                            }
                        };

                        let mut response_headers = response.headers_mut().unwrap();
                        response_headers.insert(
                              HeaderName::from_static("cache-control"),
                              cache_control
                        );
                    }
                                                  let mut response = response.status(200);
                                                  {
                                                    let mut response_headers = response.headers_mut().unwrap();
                                                    response_headers.insert(
                                                        CONTENT_TYPE,
                                                        HeaderValue::from_static("image/png"));
                                                  }

                                                  let body_content = body.0;
                                                  response.body(Body::from(body_content))
                                                },
                                            },
                                            Err(why) => {
                                                    // Application code returned an error. This should not happen, as the implementation should
                                                    // return a valid response.
                                                    return api_impl.as_ref().handle_error(&method, &host, &cookies, why).await;
                                            },
                                        };


                                        resp.map_err(|e| { error!(error = ?e); StatusCode::INTERNAL_SERVER_ERROR })
}


#[tracing::instrument(skip_all)]
fn resize_validation(
  query_params: models::ResizeQueryParams,
) -> std::result::Result<(
  models::ResizeQueryParams,
), ValidationErrors>
{
  query_params.validate()?;

Ok((
  query_params,
))
}
/// Resize - GET /api/images/resize
#[tracing::instrument(skip_all)]
async fn resize<I, A, E>(
  method: Method,
  TypedHeader(host): TypedHeader<Host>,
  cookies: CookieJar,
  QueryExtra(query_params): QueryExtra<models::ResizeQueryParams>,
 State(api_impl): State<I>,
) -> Result<Response, StatusCode>
where
    I: AsRef<A> + Send + Sync,
    A: apis::images::Images<E> + Send + Sync,
    E: std::fmt::Debug + Send + Sync + 'static,
        {




      #[allow(clippy::redundant_closure)]
      let validation = tokio::task::spawn_blocking(move ||
    resize_validation(
        query_params,
    )
  ).await.unwrap();

  let Ok((
    query_params,
  )) = validation else {
    return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(validation.unwrap_err().to_string()))
            .map_err(|_| StatusCode::BAD_REQUEST);
  };



  let result = api_impl.as_ref().resize(
      
      &method,
      &host,
      &cookies,
        &query_params,
  ).await;

  let resp = match result {
                                            Ok(rsp) => match rsp {
                                                apis::images::ResizeResponse::Status301_TheImageWasResizeAndInTheLocationYou
                                                    {
                                                        location
                                                    }
                                                => {
                                                let mut response = Response::builder();
                    if let Some(location) = location {
                        let location = match header::IntoHeaderValue(location).try_into() {
                            Ok(val) => val,
                            Err(e) => {
                                return Response::builder()
                                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                                        .body(Body::from(format!("An internal server error occurred handling location header - {e}"))).map_err(|e| { error!(error = ?e); StatusCode::INTERNAL_SERVER_ERROR });
                            }
                        };

                        let mut response_headers = response.headers_mut().unwrap();
                        response_headers.insert(
                              HeaderName::from_static("location"),
                              location
                        );
                    }
                                                  let mut response = response.status(301);
                                                  response.body(Body::empty())
                                                },
                                            },
                                            Err(why) => {
                                                    // Application code returned an error. This should not happen, as the implementation should
                                                    // return a valid response.
                                                    return api_impl.as_ref().handle_error(&method, &host, &cookies, why).await;
                                            },
                                        };


                                        resp.map_err(|e| { error!(error = ?e); StatusCode::INTERNAL_SERVER_ERROR })
}


#[allow(dead_code)]
#[inline]
fn response_with_status_code_only(code: StatusCode) -> Result<Response, StatusCode> {
   Response::builder()
          .status(code)
          .body(Body::empty())
          .map_err(|_| code)
}
