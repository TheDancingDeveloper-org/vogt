// Session history storage and retrieval using SQLite.
// Logs session metadata and optionally PTY output for replay and search.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::{FromRow, Row};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLogPreview {
    pub session_id: String,
    pub text: String,
    pub bytes: u64,
    pub total_bytes: u64,
    pub truncated: bool,
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

    /// Path for the raw PTY output log belonging to a session.
    pub fn log_path(&self, id: Uuid) -> PathBuf {
        self.log_dir.join(format!("{id}.log"))
    }

    /// Open a per-session raw output log for append.
    pub fn open_log_writer(&self, id: Uuid) -> Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path(id))
            .map_err(|e| ApiError::Internal(format!("failed to open session log: {e}")))
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
                name = excluded.name,
                ended_at = excluded.ended_at,
                exit_code = excluded.exit_code,
                cwd = excluded.cwd,
                command = excluded.command,
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
        let limit = limit.min(200);
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
        let Some(fts_query) = user_query_to_fts(query) else {
            return Ok(Vec::new());
        };
        let limit = limit.min(100);
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
        .bind(fts_query)
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
        self.replace_index_output(session_id, output).await
    }

    /// Replace indexed output for a session. Used by the archive lifecycle so
    /// retries do not accumulate duplicate FTS rows.
    pub async fn replace_index_output(&self, session_id: Uuid, output: &str) -> Result<()> {
        sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to clear indexed output: {}", e)))?;

        if output.trim().is_empty() {
            return Ok(());
        }

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

    /// Archive metadata and replace the searchable output in one public call.
    pub async fn archive_session_with_output(
        &self,
        record: ArchiveRecord,
        output: &str,
    ) -> Result<()> {
        let session_id = record.id;
        self.archive_session(record).await?;
        self.replace_index_output(session_id, output).await?;
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

    /// Read the tail of an archived raw session log for replay-oriented views.
    pub async fn read_log_preview(&self, id: Uuid, tail_bytes: u64) -> Result<SessionLogPreview> {
        let path = self.log_path(id);
        let mut file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ApiError::NotFound,
                _ => ApiError::Internal(format!("failed to open session log: {e}")),
            })?;
        let total_bytes = file
            .metadata()
            .await
            .map_err(|e| ApiError::Internal(format!("failed to stat session log: {e}")))?
            .len();

        let tail_bytes = tail_bytes.clamp(1, 256 * 1024);
        let bytes = total_bytes.min(tail_bytes);
        if total_bytes > bytes {
            file.seek(std::io::SeekFrom::Start(total_bytes - bytes))
                .await
                .map_err(|e| ApiError::Internal(format!("failed to seek session log: {e}")))?;
        }

        let mut buf = vec![0_u8; bytes as usize];
        if bytes > 0 {
            file.read_exact(&mut buf)
                .await
                .map_err(|e| ApiError::Internal(format!("failed to read session log: {e}")))?;
        }

        Ok(SessionLogPreview {
            session_id: id.to_string(),
            text: String::from_utf8_lossy(&buf).into_owned(),
            bytes,
            total_bytes,
            truncated: total_bytes > bytes,
        })
    }

    /// Clean up old sessions beyond retention period
    pub async fn cleanup_old_sessions(&self, retention_days: u32) -> Result<usize> {
        let cutoff = OffsetDateTime::now_utc() - time::Duration::days(retention_days as i64);
        let cutoff_str = cutoff
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {}", e)))?;

        let ids = sqlx::query("SELECT id FROM sessions WHERE created_at < ?")
            .bind(&cutoff_str)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("cleanup query failed: {}", e)))?;

        for row in ids {
            let id: String = row.get("id");
            sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
                .bind(&id)
                .execute(&self.pool)
                .await
                .map_err(|e| ApiError::Internal(format!("cleanup fts failed: {}", e)))?;
            if let Ok(uuid) = Uuid::parse_str(&id) {
                remove_log_file(self.log_path(uuid))?;
            }
        }

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

    /// Delete archived metadata, searchable output, and the raw log.
    pub async fn delete_session(&self, id: Uuid) -> Result<bool> {
        sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("delete fts failed: {}", e)))?;

        let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("delete failed: {}", e)))?;

        remove_log_file(self.log_path(id))?;
        Ok(result.rows_affected() > 0)
    }
}

fn user_query_to_fts(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(format!("\"{}\"", trimmed.replace('"', "\"\"")))
            }
        })
        .take(16)
        .collect();

    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

fn remove_log_file(path: PathBuf) -> Result<()> {
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::Internal(format!(
            "failed to remove session log {}: {e}",
            path.display()
        ))),
    }
}
