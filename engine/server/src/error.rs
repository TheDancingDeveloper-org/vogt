use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::json;

pub type Result<T> = std::result::Result<T, ApiError>;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    /// A launch asked for a model no known-model source lists (WI-4138).
    /// Its own variant so the response carries a stable machine-readable
    /// `code` and the message names the model, never a secret or a provider.
    #[error("unknown model: {0}")]
    UnknownModel(String),
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
    /// An internal failure that keeps its source error. Renders exactly as
    /// `Internal(format!("{context}: {source}"))` did, so the body is
    /// unchanged, but the source stays reachable for logging (WI-4655).
    #[error("internal: {context}: {source}")]
    InternalSource {
        context: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// An optional upstream could not be reached. Distinct from `Internal`
    /// because nothing the engine owns is broken, and the PWA degrades rather
    /// than reporting a server fault.
    #[error("bad gateway: {0}")]
    BadGateway(String),
    /// The host is too short of disk to take on more work (WI-1810).
    #[error("insufficient storage: {0}")]
    InsufficientStorage(String),
    /// A dependency the engine needs is temporarily unavailable (503).
    /// `retry_after` seconds, when known, is sent as a `Retry-After` header.
    #[error("unavailable: {message}")]
    Unavailable {
        message: String,
        retry_after: Option<u64>,
    },
    #[error("account at capacity: {0}")]
    AtCapacity(String),
    /// The host already runs `ENGINE_HOST_SESSION_CAP` live host sessions.
    /// `counts` says how many are live per origin, so the caller can see what
    /// holds the slots (WI-4428).
    #[error("host session cap reached: {cap} live host sessions")]
    HostCapacity {
        cap: usize,
        counts: serde_json::Value,
    },
}

impl ApiError {
    /// Internal error that carries its source: `internal_src("failed to x", e)`.
    pub fn internal_src(
        context: impl Into<String>,
        source: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        ApiError::InternalSource {
            context: context.into(),
            source: source.into(),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::UnknownModel(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::AtCapacity(_) => StatusCode::TOO_MANY_REQUESTS,
            ApiError::HostCapacity { .. } => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Config(_)
            | ApiError::Internal(_)
            | ApiError::InternalSource { .. }
            | ApiError::Pty(_)
            | ApiError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            ApiError::Unavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::InsufficientStorage(_) => StatusCode::INSUFFICIENT_STORAGE,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let status = self.status();
        if status.is_server_error() {
            if let ApiError::InternalSource { context, source } = &self {
                tracing::error!(%status, error = %self, %context, source = ?source, "request failed");
            } else {
                tracing::error!(%status, error = %self, "request failed");
            }
        }
        let mut body = json!({ "error": self.to_string() });
        if matches!(self, ApiError::UnknownModel(_)) {
            body["code"] = json!("unknown_model");
        }
        if let ApiError::HostCapacity { cap, counts } = &self {
            body["cap"] = json!(cap);
            body["counts"] = counts.clone();
        }
        let retry_after = match &self {
            ApiError::Unavailable { retry_after, .. } => *retry_after,
            _ => None,
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(secs) = retry_after {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, secs.into());
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_source_renders_like_formatted_internal() {
        let e = ApiError::internal_src("failed to x", std::io::Error::other("boom"));
        assert_eq!(e.to_string(), "internal: failed to x: boom");
        assert_eq!(
            e.to_string(),
            ApiError::Internal("failed to x: boom".into()).to_string()
        );
        assert_eq!(e.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn unavailable_is_503_with_retry_after() {
        let r = ApiError::Unavailable {
            message: "busy".into(),
            retry_after: Some(7),
        }
        .into_response();
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(r.headers()["retry-after"], "7");
        let r = ApiError::Unavailable {
            message: "busy".into(),
            retry_after: None,
        }
        .into_response();
        assert!(r.headers().get("retry-after").is_none());
    }
}
