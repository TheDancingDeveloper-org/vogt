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
    db_path: PathBuf,
    log_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryStorageStats {
    pub archived_session_count: u64,
    pub log_file_count: u64,
    pub log_bytes: u64,
    pub db_bytes: u64,
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
    /// How the session ended, when it has: `exited` (the child exited while
    /// the engine watched; `exit_code` is its code), `engine-shutdown` (the
    /// engine archived it while shutting down; `exit_code` NULL) or
    /// `engine-restart` (the engine found it unfinished at startup — killed by
    /// a restart it never saw; `ended_at` is the last time its log was
    /// written, `exit_code` NULL). NULL while the session is live, and on
    /// rows written before the column existed.
    #[sqlx(default)]
    pub end_reason: Option<String>,
    /// The session template it was started from, when it was (WI-962).
    #[sqlx(default)]
    pub template: Option<String>,
    /// `worker` or `oversight` (WI-957), as last set. NULL on rows written
    /// before the column existed.
    #[sqlx(default)]
    pub role: Option<String>,
    /// The agent conversation the session last ran (WI-962): the one the
    /// engine launched, or the one an agent typed into the shell reported.
    /// Kept after the conversation ends, so a lost session can be resumed.
    #[sqlx(default)]
    pub conversation_agent: Option<String>,
    #[sqlx(default)]
    pub conversation_id: Option<String>,
    /// The template a resume of `conversation_id` would start, worked out
    /// against the deployment's templates when the row is read: the row's
    /// own template when it runs that agent, else the one the agent's name
    /// resolves to. `None` when there is nothing to resume or no template
    /// runs that agent.
    #[sqlx(skip)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_template: Option<String>,
}

/// What a session is, as History shows it (WI-962): its template, its role
/// and the agent conversation it runs. Written with the provisional row and
/// updated when the role changes or an agent reports its conversation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    pub template: Option<String>,
    pub role: Option<&'static str>,
    pub conversation: Option<vogt_engine_contract::AgentConversation>,
}

/// The identity columns, added to an existing database in this order.
const IDENTITY_COLUMNS: &[&str] = &["template", "role", "conversation_agent", "conversation_id"];

/// The columns every metadata read selects.
const METADATA_COLUMNS: &str = "id, name, created_at, ended_at, exit_code, cwd, command, \
     scrollback_bytes, end_reason, template, role, conversation_agent, conversation_id";

/// `end_reason` values. See [`SessionMetadata::end_reason`].
pub const END_EXITED: &str = "exited";
pub const END_ENGINE_SHUTDOWN: &str = "engine-shutdown";
pub const END_ENGINE_RESTART: &str = "engine-restart";
/// The session was hibernated (WI-912): its process stopped, the session
/// did not end, and a wake continues this same row and log.
pub const END_HIBERNATED: &str = "hibernated";
/// The session was stopped on request (WI-913): killed through the engine's
/// kill route or vogt's `session.stop`, whatever its exit code.
pub const END_STOPPED: &str = "stopped";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_id: String,
    pub session_name: String,
    pub created_at: String,
    pub match_snippet: String,
    pub rank: f64,
    /// True when the hit came from a *live* session's scrollback (an on-demand
    /// bounded scan), rather than the archived FTS index. Archived rows default
    /// this to false, including when an older engine omits the field.
    #[serde(default)]
    pub live: bool,
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
    pub end_reason: Option<&'static str>,
    /// Set on the provisional row; `None` leaves what the row holds.
    pub identity: Option<SessionIdentity>,
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

        let history = Self {
            pool,
            db_path,
            log_dir,
        };
        history.init_schema().await?;

        Ok(history)
    }

    /// Directory where per-session scrollback logs are persisted for replay.
    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
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

        // `end_reason` and the identity columns arrived after the table did;
        // add them to an existing database. SQLite has no ADD COLUMN IF NOT
        // EXISTS, so look first.
        for column in std::iter::once(&"end_reason").chain(IDENTITY_COLUMNS) {
            let present = sqlx::query("SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?")
                .bind(column)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("failed to inspect sessions table: {}", e))
                })?
                .is_some();
            if !present {
                sqlx::query(&format!("ALTER TABLE sessions ADD COLUMN {column} TEXT"))
                    .execute(&self.pool)
                    .await
                    .map_err(|e| ApiError::Internal(format!("failed to add {column}: {}", e)))?;
            }
        }

        // Index on created_at for date-range queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_created ON sessions(created_at)")
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to create index: {}", e)))?;

        Ok(())
    }

    /// Archive a session row (metadata only).
    ///
    /// Used for three writes with different completeness: a provisional
    /// row at spawn (`ended_at`/`exit_code` NULL), the finalized row on exit,
    /// and the graceful-shutdown drain. The `ON CONFLICT` update is guarded so
    /// these can arrive in any order without a later, less-complete write
    /// erasing a completed one: `COALESCE` keeps an already-set `ended_at`,
    /// `exit_code`, `cwd`, or `command` when the incoming write has NULL there
    /// (a provisional or backfilled row), and `MAX` never shrinks the recorded
    /// scrollback. A real finalize (non-NULL values) still overwrites the
    /// provisional NULLs.
    pub async fn archive_session(&self, record: ArchiveRecord) -> Result<()> {
        let identity = record.identity.unwrap_or_default();
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
            INSERT INTO sessions (id, name, created_at, ended_at, exit_code, cwd, command, scrollback_bytes, end_reason,
                                  template, role, conversation_agent, conversation_id)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                ended_at = COALESCE(excluded.ended_at, sessions.ended_at),
                exit_code = COALESCE(excluded.exit_code, sessions.exit_code),
                end_reason = CASE
                    WHEN excluded.exit_code IS NOT NULL THEN excluded.end_reason
                    ELSE COALESCE(sessions.end_reason, excluded.end_reason)
                END,
                cwd = COALESCE(excluded.cwd, sessions.cwd),
                command = COALESCE(excluded.command, sessions.command),
                scrollback_bytes = MAX(excluded.scrollback_bytes, sessions.scrollback_bytes),
                template = COALESCE(excluded.template, sessions.template),
                role = COALESCE(excluded.role, sessions.role),
                conversation_agent = COALESCE(excluded.conversation_agent, sessions.conversation_agent),
                conversation_id = COALESCE(excluded.conversation_id, sessions.conversation_id)
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
        .bind(record.end_reason)
        .bind(identity.template)
        .bind(identity.role)
        .bind(identity.conversation.as_ref().map(|c| c.agent.clone()))
        .bind(identity.conversation.map(|c| c.id))
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to archive session: {}", e)))?;

        Ok(())
    }

    /// Update a session row's identity (WI-962): its role when given, and its
    /// conversation when given. A conversation that ends is not cleared —
    /// the row keeps the last one, which is what a resume needs. A row that
    /// does not exist yet (history off, or the provisional write still in
    /// flight) is left alone.
    pub async fn set_identity(
        &self,
        id: Uuid,
        role: Option<&'static str>,
        conversation: Option<&vogt_engine_contract::AgentConversation>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE sessions SET role = COALESCE(?, role), \
             conversation_agent = COALESCE(?, conversation_agent), \
             conversation_id = COALESCE(?, conversation_id) WHERE id = ?",
        )
        .bind(role)
        .bind(conversation.map(|c| c.agent.as_str()))
        .bind(conversation.map(|c| c.id.as_str()))
        .bind(id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to update session identity: {e}")))?;
        Ok(())
    }

    /// List archived sessions
    pub async fn list_sessions(&self, limit: usize, offset: usize) -> Result<Vec<SessionMetadata>> {
        let limit = limit.min(200);
        let sessions = sqlx::query_as::<_, SessionMetadata>(&format!(
            "SELECT {METADATA_COLUMNS} FROM sessions ORDER BY created_at DESC LIMIT ? OFFSET ?"
        ))
        .bind(limit as i64)
        .bind(offset as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to list sessions: {}", e)))?;

        Ok(sessions)
    }

    pub async fn count_sessions(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM sessions")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to count sessions: {}", e)))?;
        Ok(row.get::<i64, _>("count") as u64)
    }

    pub async fn storage_stats(&self) -> Result<HistoryStorageStats> {
        let archived_session_count = self.count_sessions().await?;
        let db_bytes = std::fs::metadata(&self.db_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let (log_file_count, log_bytes) = summarize_regular_files(&self.log_dir)?;
        Ok(HistoryStorageStats {
            archived_session_count,
            log_file_count: log_file_count as u64,
            log_bytes,
            db_bytes,
        })
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
                -- Return plain text. The PWA owns the narrowly scoped
                -- highlighting at its text sink; terminal output is untrusted.
                snippet(session_output_fts, 1, '', '', '...', 32) as match_snippet,
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
                live: false,
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

    /// One-shot startup pass: index raw session logs on disk that have no
    /// history row yet.
    ///
    /// The raw transcript at `<state_dir>/session-logs/<uuid>.log` persists
    /// across restarts, but before this the index row was only written when a
    /// child exited while the engine was alive. A hard redeploy (SIGKILL of a
    /// long-lived agent shell) therefore left the transcript on disk with no
    /// row, invisible in the History tab. This recovers those: for every
    /// `*.log` whose UUID is not already a row, it inserts a metadata row with
    /// `ended_at`/`exit_code` NULL (the outcome is genuinely unknown) and
    /// indexes the stripped output so search reaches it. Rows that already
    /// exist are left untouched.
    pub async fn backfill_orphaned_logs(&self) -> Result<usize> {
        self.backfill_orphaned_logs_before(OffsetDateTime::now_utc())
            .await
    }

    /// [`Self::backfill_orphaned_logs`], closing out every recovered log last
    /// written before `booted_at`: such a log belongs to a session of an
    /// earlier process, which cannot still be running, so its row gets
    /// `ended_at` = the log's mtime and `end_reason = engine-restart` (exit
    /// code unknown, NULL). A log written since boot may be a live session of
    /// this process whose provisional row has not landed yet; it stays open.
    pub async fn backfill_orphaned_logs_before(&self, booted_at: OffsetDateTime) -> Result<usize> {
        let rows = sqlx::query("SELECT id FROM sessions")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("backfill: list ids failed: {e}")))?;
        let known: std::collections::HashSet<String> =
            rows.into_iter().map(|r| r.get::<String, _>("id")).collect();

        let entries = match std::fs::read_dir(&self.log_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "backfill: read log dir failed: {e}"
                )))
            }
        };

        let mut recovered = 0usize;
        for entry in entries {
            let entry = entry
                .map_err(|e| ApiError::Internal(format!("backfill: dir entry failed: {e}")))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("log") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(id) = Uuid::parse_str(stem) else {
                continue;
            };
            if known.contains(&id.to_string()) {
                continue;
            }
            let meta = match std::fs::metadata(&path) {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            let size = meta.len();
            let modified = meta.modified().ok().map(OffsetDateTime::from);
            let ended_str = modified.filter(|t| *t < booted_at).and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            });
            let end_reason = ended_str.as_ref().map(|_| END_ENGINE_RESTART);
            let created_str = modified
                .and_then(|t| {
                    t.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .unwrap_or_else(|| {
                    OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default()
                });

            // `DO NOTHING`: another writer that raced us to this id keeps its
            // (more complete) row. The outcome is unknown, so exit_code/ended_at
            // stay NULL, which the `unfinished` history filter now surfaces.
            // (`ended_at` is the log's mtime when that predates this boot.)
            sqlx::query(
                r#"
                INSERT INTO sessions (id, name, created_at, ended_at, exit_code, cwd, command, scrollback_bytes, end_reason)
                VALUES (?, ?, ?, ?, NULL, NULL, NULL, ?, ?)
                ON CONFLICT(id) DO NOTHING
                "#,
            )
            .bind(id.to_string())
            .bind(format!("recovered {}", &stem[..stem.len().min(8)]))
            .bind(&created_str)
            .bind(&ended_str)
            .bind(size as i64)
            .bind(end_reason)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("backfill: insert failed: {e}")))?;

            if let Ok(bytes) = std::fs::read(&path) {
                let visible = crate::activity::strip_ansi(&bytes);
                let text = String::from_utf8_lossy(&visible);
                if let Err(e) = self.replace_index_output(id, text.as_ref()).await {
                    tracing::warn!(session = %id, error = %e, "backfill: failed to index recovered log");
                }
            }
            recovered += 1;
        }
        Ok(recovered)
    }

    /// Close out rows a previous engine process left unfinished.
    ///
    /// Sessions live in this process's memory, so at startup none of the
    /// rows with a NULL `ended_at` can still be running: each was a session
    /// the previous process lost — SIGKILLed by a redeploy before the drain
    /// could archive it, or crashed. Without this they read as live in the
    /// History tab forever. Each gets `ended_at` = the last time its raw log
    /// was written (the best evidence of when it stopped; now if there is no
    /// log), `end_reason = engine-restart`, and `exit_code` stays NULL because
    /// the code is genuinely unknown.
    ///
    /// Must run before this process creates any session, since a new
    /// session's provisional row also has a NULL `ended_at`.
    pub async fn reconcile_unfinished(&self) -> Result<usize> {
        let rows = sqlx::query("SELECT id FROM sessions WHERE ended_at IS NULL")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("reconcile: list failed: {e}")))?;
        let now = OffsetDateTime::now_utc();
        let mut closed = 0usize;
        for row in rows {
            let id: String = row.get("id");
            let ended = Uuid::parse_str(&id)
                .ok()
                .and_then(|uuid| std::fs::metadata(self.log_path(uuid)).ok())
                .and_then(|meta| meta.modified().ok())
                .map(OffsetDateTime::from)
                .unwrap_or(now);
            let ended = ended
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|e| ApiError::Internal(format!("time format error: {e}")))?;
            sqlx::query(
                "UPDATE sessions SET ended_at = ?, end_reason = COALESCE(end_reason, ?) \
                 WHERE id = ? AND ended_at IS NULL",
            )
            .bind(&ended)
            .bind(END_ENGINE_RESTART)
            .bind(&id)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("reconcile: update failed: {e}")))?;
            closed += 1;
        }
        Ok(closed)
    }

    /// Get session by ID
    pub async fn get_session(&self, id: Uuid) -> Result<SessionMetadata> {
        let session = sqlx::query_as::<_, SessionMetadata>(&format!(
            "SELECT {METADATA_COLUMNS} FROM sessions WHERE id = ?"
        ))
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
    ///
    /// With `strip_ansi`, the escape sequences a terminal consumes without
    /// printing are removed from `text` (the same stripper the archive path
    /// uses), so an agent reading over MCP gets plain text rather than a wall
    /// of `\x1b[` noise. The byte counters still describe the raw tail window
    /// that was read from disk — `text` is the rendered view of it.
    pub async fn read_log_preview(
        &self,
        id: Uuid,
        tail_bytes: u64,
        strip_ansi: bool,
    ) -> Result<SessionLogPreview> {
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

        let text = if strip_ansi {
            String::from_utf8_lossy(&crate::activity::strip_ansi(&buf)).into_owned()
        } else {
            String::from_utf8_lossy(&buf).into_owned()
        };

        Ok(SessionLogPreview {
            session_id: id.to_string(),
            text,
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

/// Lowercased alphanumeric/underscore tokens (max 16), the same tokenisation
/// the FTS query builder uses, for substring-matching live-session scrollback.
/// Empty when the query carries no usable term.
pub fn query_tokens(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter_map(|part| {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_lowercase())
            }
        })
        .take(16)
        .collect()
}

/// Build a live-search hit from a live session's raw scrollback tail, when
/// every query token appears (case-insensitively) in the ANSI-stripped text.
/// Mirrors the archived path's AND-of-terms semantics and its plain-text
/// snippet contract — terminal output is untrusted, so the snippet is never
/// marked up; the PWA owns highlighting at its text sink.
pub fn live_match(
    session_id: &str,
    session_name: &str,
    created_at: &str,
    raw_tail: &[u8],
    tokens: &[String],
) -> Option<SearchResult> {
    if tokens.is_empty() {
        return None;
    }
    let stripped = crate::activity::strip_ansi(raw_tail);
    let text = String::from_utf8_lossy(&stripped);
    let haystack = text.to_lowercase();
    if !tokens.iter().all(|t| haystack.contains(t.as_str())) {
        return None;
    }
    Some(SearchResult {
        session_id: session_id.to_string(),
        session_name: session_name.to_string(),
        created_at: created_at.to_string(),
        match_snippet: live_snippet(&text, &tokens[0]),
        // Archived hits carry FTS `rank` (ascending = best); live hits have no
        // FTS score. 0.0 keeps them ahead of nothing in particular; the `live`
        // flag, not the rank, is what a consumer keys on.
        rank: 0.0,
        live: true,
    })
}

/// A readable one-line snippet for a live hit: the first scrollback line that
/// contains the leading token (case-insensitively), trimmed and length-capped;
/// failing that, the first non-empty line. Line-oriented rather than a byte
/// window, so it stays on character boundaries and reads cleanly.
fn live_snippet(text: &str, first_token: &str) -> String {
    const MAX_CHARS: usize = 160;
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && trimmed.to_lowercase().contains(first_token) {
            return truncate_chars(trimmed, MAX_CHARS);
        }
    }
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| truncate_chars(line, MAX_CHARS))
        .unwrap_or_default()
}

/// Truncate to at most `max` characters (not bytes), appending an ellipsis when
/// anything was dropped.
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("...");
    out
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

fn summarize_regular_files(path: &Path) -> Result<(usize, u64)> {
    let mut count = 0usize;
    let mut bytes = 0u64;
    match std::fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                let meta = entry.metadata()?;
                if meta.is_file() {
                    count += 1;
                    bytes = bytes.saturating_add(meta.len());
                }
            }
            Ok((count, bytes))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((0, 0)),
        Err(e) => Err(ApiError::Internal(format!(
            "failed to read history log dir {}: {e}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_tokens_lowercases_splits_and_keeps_underscores() {
        assert_eq!(
            query_tokens("Hello, World_1!"),
            vec!["hello".to_string(), "world_1".to_string()]
        );
    }

    #[test]
    fn query_tokens_are_empty_for_a_query_with_no_usable_term() {
        assert!(query_tokens("   ").is_empty());
        assert!(query_tokens("!!! ---").is_empty());
    }

    #[test]
    fn query_tokens_cap_at_sixteen() {
        let many = (0..30)
            .map(|i| format!("w{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(query_tokens(&many).len(), 16);
    }

    #[test]
    fn live_match_requires_every_token_and_strips_ansi() {
        // Two tokens, both present across the ANSI-coloured tail.
        let tail = b"\x1b[2K\x1b[1Ghello there\n\x1b[31mworld needle\x1b[0m\n";
        let tokens = query_tokens("needle world");
        let hit = live_match("sid", "sname", "2026-01-01T00:00:00Z", tail, &tokens)
            .expect("all tokens present -> a hit");
        assert!(hit.live);
        assert_eq!(hit.session_id, "sid");
        assert_eq!(hit.session_name, "sname");
        assert_eq!(hit.rank, 0.0);
        // Snippet is the matching line, ANSI removed, plain text (untrusted
        // output preserved as data — never marked up).
        assert_eq!(hit.match_snippet, "world needle");
        assert!(!hit.match_snippet.contains('\u{1b}'));
    }

    #[test]
    fn live_match_returns_none_when_a_token_is_absent() {
        let tail = b"only has the word alpha\n";
        let tokens = query_tokens("alpha beta");
        assert!(live_match("s", "n", "t", tail, &tokens).is_none());
    }

    #[test]
    fn live_match_returns_none_for_empty_tokens() {
        assert!(live_match("s", "n", "t", b"anything", &[]).is_none());
    }

    #[test]
    fn truncate_chars_appends_ellipsis_only_when_dropping() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert_eq!(truncate_chars("abcdef", 3), "abc...");
    }

    /// A history database from before WI-962 gains the identity columns, its
    /// rows read with them empty, and a session's identity is kept: role and
    /// conversation updates land, and an update without a conversation does
    /// not erase the last one.
    #[tokio::test]
    async fn an_old_history_gains_the_identity_columns_and_keeps_the_last_conversation() {
        let dir = tempfile::tempdir().unwrap();
        {
            let pool = SqlitePoolOptions::new()
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(dir.path().join("history.db"))
                        .create_if_missing(true),
                )
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE sessions (id TEXT PRIMARY KEY, name TEXT NOT NULL, \
                 created_at TEXT NOT NULL, ended_at TEXT, exit_code INTEGER, cwd TEXT, \
                 command TEXT, scrollback_bytes INTEGER DEFAULT 0, end_reason TEXT)",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO sessions (id, name, created_at, command) \
                 VALUES ('00000000-0000-4000-8000-000000000001', 'old', '2026-10-01T00:00:00Z', 'bash')",
            )
            .execute(&pool)
            .await
            .unwrap();
        }
        let history = SessionHistory::new(dir.path()).await.unwrap();
        let old = history
            .get_session(Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap())
            .await
            .unwrap();
        assert_eq!(old.command.as_deref(), Some("bash"));
        assert_eq!(old.conversation_id, None);
        assert_eq!(old.role, None);

        let id = Uuid::new_v4();
        history
            .archive_session(ArchiveRecord {
                id,
                name: "Oversight".into(),
                created_at: OffsetDateTime::now_utc(),
                ended_at: None,
                exit_code: None,
                cwd: None,
                command: Some("bash".into()),
                scrollback_bytes: 0,
                end_reason: None,
                identity: Some(SessionIdentity {
                    template: None,
                    role: Some("worker"),
                    conversation: None,
                }),
            })
            .await
            .unwrap();
        let conversation = vogt_engine_contract::AgentConversation {
            agent: "claude".into(),
            id: "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f".into(),
        };
        history
            .set_identity(id, None, Some(&conversation))
            .await
            .unwrap();
        history
            .set_identity(id, Some("oversight"), None)
            .await
            .unwrap();
        let row = history.get_session(id).await.unwrap();
        assert_eq!(row.role.as_deref(), Some("oversight"));
        assert_eq!(row.conversation_agent.as_deref(), Some("claude"));
        assert_eq!(
            row.conversation_id.as_deref(),
            Some(conversation.id.as_str())
        );
        // A later finalize carries no identity and keeps it.
        history
            .archive_session(ArchiveRecord {
                id,
                name: "Oversight".into(),
                created_at: OffsetDateTime::now_utc(),
                ended_at: Some(OffsetDateTime::now_utc()),
                exit_code: Some(0),
                cwd: None,
                command: None,
                scrollback_bytes: 10,
                end_reason: Some(END_EXITED),
                identity: None,
            })
            .await
            .unwrap();
        let row = history.get_session(id).await.unwrap();
        assert_eq!(
            row.conversation_id.as_deref(),
            Some(conversation.id.as_str())
        );
        assert_eq!(row.role.as_deref(), Some("oversight"));
    }
}
