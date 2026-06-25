use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    error::{ApiError, Result},
    history::{SearchResult, SessionMetadata},
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

/// List archived sessions with pagination
pub async fn list_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Vec<SessionMetadata>>> {
    let history = state
        .history
        .as_ref()
        .ok_or_else(|| ApiError::Internal("history not enabled".into()))?;

    let sessions = history.list_sessions(q.limit, q.offset).await?;
    Ok(Json(sessions))
}

/// Search session output via full-text search
pub async fn search_sessions(
    State(state): State<Arc<AppState>>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Vec<SearchResult>>> {
    let history = state
        .history
        .as_ref()
        .ok_or_else(|| ApiError::Internal("history not enabled".into()))?;

    let results = history.search(&q.q, q.limit).await?;
    Ok(Json(results))
}

/// Get session metadata by ID
pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionMetadata>> {
    let history = state
        .history
        .as_ref()
        .ok_or_else(|| ApiError::Internal("history not enabled".into()))?;

    let session = history.get_session(id).await?;
    Ok(Json(session))
}

/// Delete archived session
pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let history = state
        .history
        .as_ref()
        .ok_or_else(|| ApiError::Internal("history not enabled".into()))?;

    // Delete from database
    sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id.to_string())
        .execute(&history.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("delete failed: {}", e)))?;

    Ok(Json(serde_json::json!({ "ok": true })))
}
