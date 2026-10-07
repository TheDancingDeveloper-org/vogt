use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("conflict: {0}")]
    Conflict(String),
    /// Authenticated, but this caller may not do this.
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("payload too large: {0}")]
    TooLarge(String),
    #[error("config: {0}")]
    Config(String),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
    /// An optional upstream could not be reached. Distinct from `Internal`
    /// because nothing the engine owns is broken, and the PWA degrades rather
    /// than reporting a server fault.
    #[error("bad gateway: {0}")]
    BadGateway(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Config(_) | ApiError::Internal(_) | ApiError::Pty(_) | ApiError::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        let body = Json(json!({ "error": self.to_string() }));
        (status, body).into_response()
    }
}
