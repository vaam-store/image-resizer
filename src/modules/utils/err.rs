use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use std::fmt::Display;
use std::io;
use thiserror::Error;
use tracing::error;

#[allow(dead_code)]
pub fn app_panic<T: Into<String> + Display>(message: T) -> ! {
    let msg = format!("{}: {}", "Panic", message);
    error!("{}", msg);
    panic!("{}", msg);
}

#[derive(Error, Debug)]
pub enum AppError {
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),

    #[error("I/O error: {0}")]
    AnyError(#[from] anyhow::Error),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match self {
            AppError::IoError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            AppError::AnyError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.to_string()).into_response()
    }
}
