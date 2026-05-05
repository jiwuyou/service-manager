use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,

    #[error("not found")]
    NotFound,

    #[error("provider not found: {0}")]
    ProviderNotFound(String),

    #[error("{0}")]
    BadRequest(String),

    #[error("internal error")]
    Internal,

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, AppError>;

#[derive(Debug, Serialize)]
struct ApiErrorEnvelope {
    error: ApiError,
}

#[derive(Debug, Serialize)]
struct ApiError {
    code: &'static str,
    message: String,
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::ProviderNotFound(_) => StatusCode::BAD_REQUEST,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Json(_) => StatusCode::BAD_REQUEST,
            AppError::Internal | AppError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "unauthorized",
            AppError::NotFound => "not_found",
            AppError::ProviderNotFound(_) => "provider_not_found",
            AppError::BadRequest(_) => "bad_request",
            AppError::Json(_) => "bad_request",
            AppError::Internal | AppError::Io(_) => "internal_error",
        }
    }

    fn message(&self) -> String {
        match self {
            AppError::Unauthorized => "missing or invalid bearer token".to_string(),
            AppError::NotFound => "resource not found".to_string(),
            AppError::ProviderNotFound(s) => format!("provider not found: {s:?}"),
            AppError::BadRequest(s) => s.clone(),
            AppError::Internal => "internal error".to_string(),
            AppError::Io(e) => e.to_string(),
            AppError::Json(e) => e.to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = ApiErrorEnvelope {
            error: ApiError {
                code: self.code(),
                message: self.message(),
            },
        };

        (status, Json(body)).into_response()
    }
}
