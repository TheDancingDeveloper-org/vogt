use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail {
    pub message: String,
    #[serde(rename = "type")]
    pub error_type: String,
    pub code: String,
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    error_type: &'static str,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid_request",
            message,
        )
    }

    pub fn not_implemented(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "server_error",
            "provider_unconfigured",
            message,
        )
    }

    pub fn bad_gateway(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "server_error",
            "provider_error",
            message,
        )
    }

    fn new(
        status: StatusCode,
        error_type: &'static str,
        code: &'static str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            error_type,
            code,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                message: self.message,
                error_type: self.error_type.to_string(),
                code: self.code.to_string(),
            },
        };
        (self.status, Json(body)).into_response()
    }
}
