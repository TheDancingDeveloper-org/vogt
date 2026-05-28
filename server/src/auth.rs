use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::Response,
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::app::AppState;

static AUTH_FAILURES: AtomicU64 = AtomicU64::new(0);

/// Header we attach to every response so operators can correlate audit log
/// lines with a specific request. Echoes back an incoming `X-Request-Id` if
/// present, otherwise mints a fresh one.
static REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

/// Bearer-token gate. Constant-time compare to avoid timing oracles even on a tailnet.
///
/// Also emits an audit log entry for every mutating request (POST/PUT/PATCH/
/// DELETE) — the dev pod has the host Docker socket mounted, so we want a
/// trail of who did what. The single bearer token doesn't give us a user
/// identity, so the log line records method + path + request id; that's
/// enough to correlate suspicious activity from the same client.
pub async fn require_bearer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let request_id = headers
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);

    let Some(token) = token else {
        record_auth_failure(&method, &path, &request_id, "missing").await;
        return Err(StatusCode::UNAUTHORIZED);
    };

    if !bool::from(token.as_bytes().ct_eq(state.config.token.as_bytes())) {
        record_auth_failure(&method, &path, &request_id, "wrong-token").await;
        return Err(StatusCode::UNAUTHORIZED);
    }

    let is_mutating = matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(&REQUEST_ID_HEADER, value);
    }

    if is_mutating {
        tracing::info!(
            target: "mydevenv2::audit",
            request_id = %request_id,
            method = %method,
            path = %path,
            status = response.status().as_u16(),
            "mutating request"
        );
    }

    Ok(response)
}

async fn record_auth_failure(method: &Method, path: &str, request_id: &str, reason: &'static str) {
    let count = AUTH_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::warn!(
        target: "mydevenv2::audit",
        request_id = %request_id,
        method = %method,
        path = %path,
        reason = reason,
        total_failures = count,
        "auth failure"
    );
    // Cheap rate-limiting: scale a delay with the running failure count so a
    // mistyped curl is unaffected but a brute-forcer slows to a crawl. Capped
    // so a transient bad client doesn't pin a worker.
    let delay_ms = (count.min(50) * 50).min(2000);
    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
}
