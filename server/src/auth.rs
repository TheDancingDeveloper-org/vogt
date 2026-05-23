use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;

use crate::app::AppState;

/// Bearer-token gate. Constant-time compare to avoid timing oracles even on a tailnet.
pub async fn require_bearer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    let Some(token) = token else {
        return Err(StatusCode::UNAUTHORIZED);
    };

    if bool::from(token.as_bytes().ct_eq(state.config.token.as_bytes())) {
        Ok(next.run(request).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Accepts either an Authorization header OR a `?token=…` query param.
/// The query-param fallback is for browser WebSocket clients (which can't set headers).
pub fn ws_token_ok(state: &AppState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let header_tok = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    let candidate = header_tok.or(query_token);
    let Some(tok) = candidate else { return false };
    bool::from(tok.as_bytes().ct_eq(state.config.token.as_bytes()))
}
