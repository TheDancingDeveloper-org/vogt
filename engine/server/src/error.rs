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
    #[error("config: {0}")]
    Config(String),
    #[error("pty error: {0}")]
    Pty(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("internal: {0}")]
    Internal(String),
    /// An optional upstream (today: the ContextKeeper sidecar) could not be
    /// reached. Distinct from `Internal` because nothing MyDevEnv2 owns is
    /// broken, and the PWA degrades rather than reporting a server fault.
    #[error("bad gateway: {0}")]
    BadGateway(String),
    /// An upstream answered, and its own status and detail are the useful
    /// reply — an open recovery circuit reports its retry time this way.
    #[error("upstream error: {status}")]
    Upstream {
        status: u16,
        detail: serde_json::Value,
    },
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::Config(_) | ApiError::Internal(_) | ApiError::Pty(_) | ApiError::Io(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            ApiError::Upstream { status, .. } => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_GATEWAY)
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        // An upstream's own detail travels through unchanged: the PWA renders
        // the failure category and the circuit's retry time from it.
        let body = match &self {
            ApiError::Upstream { detail, .. } => {
                Json(json!({ "error": self.to_string(), "detail": detail }))
            }
            _ => Json(json!({ "error": self.to_string() })),
        };
        (status, body).into_response()
    }
}
