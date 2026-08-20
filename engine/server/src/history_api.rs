use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderValue},
    response::Response,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    error::{ApiError, Result},
    history::{SearchResult, SessionLogPreview, SessionMetadata},
};

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    50
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    20
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default = "default_tail_bytes")]
    tail_bytes: u64,
}

fn default_tail_bytes() -> u64 {
    64 * 1024
}

#[derive(Debug, Deserialize)]
pub struct CleanupReq {
    #[serde(default = "default_retention_days")]
    retention_days: u32,
}

fn default_retention_days() -> u32 {
    30
}

/// List archived sessions with pagination.
///
/// A disabled history answers 404, not 500 — as the assistant routes already
/// do when unprovisioned. `Internal` said the server was
/// broken when the truth was that a feature had not been turned on, and the
/// difference matters to whoever is paged about it.
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SessionMetadata>>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    let sessions = history.list_sessions(q.limit, q.offset).await?;
    Ok(Json(sessions))
}

/// Search session output via full-text search
pub async fn search_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchResult>>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    let results = history.search(&q.q, q.limit).await?;
    Ok(Json(results))
}

/// Get session metadata by ID
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionMetadata>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    let session = history.get_session(id).await?;
    Ok(Json(session))
}

/// Read the tail of the archived raw output log for replay-oriented history views.
pub async fn get_session_log(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<LogQuery>,
) -> Result<Json<SessionLogPreview>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    let preview = history.read_log_preview(id, q.tail_bytes).await?;
    Ok(Json(preview))
}

/// Download the archived raw output log for a session.
pub async fn download_session_log(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Response<Body>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;
    let session = history.get_session(id).await?;
    let path = history.log_path(id);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ApiError::NotFound,
            _ => ApiError::Internal(format!("failed to stat session log: {e}")),
        })?;
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ApiError::NotFound,
            _ => ApiError::Internal(format!("failed to open session log: {e}")),
        })?;
    let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
    let body = Body::from_stream(stream);
    let safe_name = session
        .name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let fallback = if safe_name.is_empty() {
        "session"
    } else {
        safe_name.as_str()
    };
    let disposition = format!("attachment; filename=\"{}-{}.log\"", fallback, id);
    let response = Response::builder()
        .header(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        )
        .header(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_str(&disposition)
                .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
        )
        .header(header::CONTENT_LENGTH, HeaderValue::from(meta.len()))
        .body(body)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(response)
}

/// Delete archived session
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    if !history.delete_session(id).await? {
        return Err(ApiError::NotFound);
    }

    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn cleanup_sessions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CleanupReq>,
) -> Result<Json<serde_json::Value>> {
    let history = state.history.as_ref().ok_or(ApiError::NotFound)?;

    let removed = history.cleanup_old_sessions(req.retention_days).await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "removed_sessions": removed,
        "retention_days": req.retention_days,
    })))
}
