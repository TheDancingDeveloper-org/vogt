// Session history storage and retrieval using SQLite.
// Logs session metadata and optionally PTY output for replay and search.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{FromRow, Row};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::{ApiError, Result};

/// Session history database manager
pub struct SessionHistory {
    pub pool: SqlitePool,
    log_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct SessionMetadata {
    pub id: String,
    pub name: String,
    pub created_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub scrollback_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_id: String,
    pub session_name: String,
    pub created_at: String,
    pub match_snippet: String,
    pub rank: f64,
}

/// Parameters for archiving a completed session. Grouped into a struct so the
/// archive call site stays readable and clippy's argument-count lint is happy.
#[derive(Debug, Clone)]
pub struct ArchiveRecord {
    pub id: Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub ended_at: Option<OffsetDateTime>,
    pub exit_code: Option<i32>,
    pub cwd: Option<String>,
    pub command: Option<String>,
    pub scrollback_bytes: u64,
}

impl SessionHistory {
    /// Initialize the session history database
    pub async fn new(state_dir: &Path) -> Result<Self> {
        let db_path = state_dir.join("history.db");
        let log_dir = state_dir.join("session-logs");

        // Create log directory if it doesn't exist
        std::fs::create_dir_all(&log_dir)
            .map_err(|e| ApiError::Internal(format!("failed to create log dir: {}", e)))?;

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to connect to history db: {}", e)))?;

        let history = Self { pool, log_dir };
        history.init_schema().await?;

        Ok(history)
    }

    /// Directory where per-session scrollback logs are persisted for replay.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    /// Initialize database schema
    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at TEXT NOT NULL,
                ended_at TEXT,
                exit_code INTEGER,
                cwd TEXT,
                command TEXT,
                scrollback_bytes INTEGER DEFAULT 0
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create sessions table: {}", e)))?;

        // Create FTS5 virtual table for full-text search
        sqlx::query(
            r#"
            CREATE VIRTUAL TABLE IF NOT EXISTS session_output_fts USING fts5(
                session_id UNINDEXED,
                output_text,
                tokenize = 'porter'
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create fts table: {}", e)))?;

        // Index on created_at for date-range queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_created ON sessions(created_at)")
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to create index: {}", e)))?;

        Ok(())
    }

    /// Archive a completed session
    pub async fn archive_session(&self, record: ArchiveRecord) -> Result<()> {
        let created_str = record
            .created_at
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {}", e)))?;

        let ended_str = record.ended_at.and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        });

        sqlx::query(
            r#"
            INSERT INTO sessions (id, name, created_at, ended_at, exit_code, cwd, command, scrollback_bytes)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                ended_at = excluded.ended_at,
                exit_code = excluded.exit_code,
                scrollback_bytes = excluded.scrollback_bytes
            "#,
        )
        .bind(record.id.to_string())
        .bind(record.name)
        .bind(created_str)
        .bind(ended_str)
        .bind(record.exit_code)
        .bind(record.cwd)
        .bind(record.command)
        .bind(record.scrollback_bytes as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to archive session: {}", e)))?;

        Ok(())
    }

    /// List archived sessions
    pub async fn list_sessions(&self, limit: usize, offset: usize) -> Result<Vec<SessionMetadata>> {
        let sessions = sqlx::query_as::<_, SessionMetadata>(
            r#"
            SELECT id, name, created_at, ended_at, exit_code, cwd, command, scrollback_bytes
            FROM sessions
            ORDER BY created_at DESC
            LIMIT ? OFFSET ?
            "#,
        )
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to list sessions: {}", e)))?;

        Ok(sessions)
    }

    /// Search session output
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
        let results = sqlx::query(
            r#"
            SELECT
                fts.session_id,
                s.name as session_name,
                s.created_at,
                snippet(session_output_fts, 1, '<mark>', '</mark>', '...', 32) as match_snippet,
                rank as rank
            FROM session_output_fts fts
            JOIN sessions s ON s.id = fts.session_id
            WHERE session_output_fts MATCH ?
            ORDER BY rank
            LIMIT ?
            "#,
        )
        .bind(query)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("search failed: {}", e)))?;

        let search_results = results
            .into_iter()
            .map(|row| SearchResult {
                session_id: row.get("session_id"),
                session_name: row.get("session_name"),
                created_at: row.get("created_at"),
                match_snippet: row.get("match_snippet"),
                rank: row.get("rank"),
            })
            .collect();

        Ok(search_results)
    }

    /// Index session output for full-text search
    pub async fn index_output(&self, session_id: Uuid, output: &str) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO session_output_fts (session_id, output_text)
            VALUES (?, ?)
            "#,
        )
        .bind(session_id.to_string())
        .bind(output)
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to index output: {}", e)))?;

        Ok(())
    }

    /// Get session by ID
    pub async fn get_session(&self, id: Uuid) -> Result<SessionMetadata> {
        let session = sqlx::query_as::<_, SessionMetadata>(
            r#"
            SELECT id, name, created_at, ended_at, exit_code, cwd, command, scrollback_bytes
            FROM sessions
            WHERE id = ?
            "#,
        )
        .bind(id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => ApiError::NotFound,
            _ => ApiError::Internal(format!("failed to get session: {}", e)),
        })?;

        Ok(session)
    }

    /// Clean up old sessions beyond retention period
    pub async fn cleanup_old_sessions(&self, retention_days: u32) -> Result<usize> {
        let cutoff = OffsetDateTime::now_utc() - time::Duration::days(retention_days as i64);
        let cutoff_str = cutoff
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {}", e)))?;

        let result = sqlx::query(
            r#"
            DELETE FROM sessions
            WHERE created_at < ?
            "#,
        )
        .bind(cutoff_str)
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("cleanup failed: {}", e)))?;

        Ok(result.rows_affected() as usize)
    }
}
