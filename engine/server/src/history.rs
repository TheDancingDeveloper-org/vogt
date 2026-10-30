// Session history storage and retrieval using SQLite.
// Logs session metadata and optionally PTY output for replay and search.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{AssertSqlSafe, FromRow, Row};
use time::OffsetDateTime;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::history_writer::{self, HistoryWriter, Write};

use crate::error::{ApiError, Result};

/// Session history database manager
pub struct SessionHistory {
    /// Read-only pool. Request paths that list or fetch a session use this
    /// and never the writer.
    pool: SqlitePool,
    writer: HistoryWriter,
    db_path: PathBuf,
    log_dir: PathBuf,
    /// Last on-disk footprint (db bytes, log file count, log bytes) and when
    /// it was taken. `/api/status` is polled; walking the log dir on every
    /// call is what it is not allowed to do (WI-2700).
    disk_stats: parking_lot::Mutex<Option<(std::time::Instant, (u64, u64, u64))>>,
}

/// How long `/api/status` may report a cached on-disk footprint.
pub const STORAGE_STATS_TTL: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// The work item the session served, as last labelled (WI-998); cleared
    /// when it was unbound. What a resume from History binds again.
    #[sqlx(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_item: Option<String>,
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
    pub work_item: Option<String>,
}

/// The identity columns, added to an existing database in this order.
const IDENTITY_COLUMNS: &[&str] = &[
    "template",
    "role",
    "conversation_agent",
    "conversation_id",
    "work_item",
    "economics",
];

/// The columns every metadata read selects. Spliced into queries with
/// `format!` behind `AssertSqlSafe`: a constant, never input.
const METADATA_COLUMNS: &str = "id, name, created_at, ended_at, exit_code, cwd, command, \
     scrollback_bytes, end_reason, template, role, conversation_agent, conversation_id, work_item";

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
    /// The economics snapshot as JSON (WI-1245). `None` leaves what the row
    /// holds, so a later archive without one does not erase it.
    pub economics: Option<String>,
}

impl SessionHistory {
    /// Initialize the session history database
    pub async fn new(state_dir: &Path) -> Result<Self> {
        let db_path = state_dir.join("history.db");
        let log_dir = state_dir.join("session-logs");

        std::fs::create_dir_all(&log_dir)
            .map_err(|e| ApiError::Internal(format!("failed to create log dir: {}", e)))?;

        // One writer connection. WAL + NORMAL means a commit does not wait
        // for a full fsync of the main database, and a reader on the other
        // pool never blocks this connection. busy_timeout covers a lock left
        // by a process that died mid-write; our own writers cannot contend
        // because there is only this connection.
        let writer_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Normal)
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await
            .map_err(|e| ApiError::Internal(format!("failed to connect to history db: {}", e)))?;
        let mut writer_conn = writer_pool
            .acquire()
            .await
            .map_err(|e| ApiError::Internal(format!("failed to acquire history writer: {e}")))?
            .detach();
        // Dropping the pool must not close the detached connection.
        writer_pool.close().await;

        Self::apply_schema(&mut writer_conn).await?;

        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .read_only(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .busy_timeout(Duration::from_secs(5)),
            )
            .await
            .map_err(|e| ApiError::Internal(format!("failed to open history read pool: {}", e)))?;

        let history = Self {
            pool,
            writer: HistoryWriter::spawn(writer_conn, log_dir.clone()),
            db_path,
            log_dir,
            disk_stats: parking_lot::Mutex::new(None),
        };
        Ok(history)
    }

    /// Run one statement on the writer connection and then close it. The
    /// startup-reconcile test drops a column an older engine never had, and
    /// must be sure the writer has stopped before the next process opens
    /// the file.
    pub async fn exec_and_close(&self, sql: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::ExecAndClose {
                sql: sql.to_owned(),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "exec and close").await
    }

    /// Commit every coalesced economics snapshot now. Tests use this so they
    /// do not wait out the coalesce interval.
    pub async fn flush_economics(&self) -> Result<usize> {
        let (reply, rx) = oneshot::channel();
        self.writer.enqueue(Write::FlushEconomics { reply }).await?;
        history_writer::writer_reply(rx, "flush economics").await
    }

    /// Flush coalesced economics, drain the queue and stop the writer task.
    /// The graceful-shutdown path awaits this after `drain_to_history` so a
    /// redeploy loses none of the last writes. A failure to stop cleanly is
    /// logged, not returned: the process is exiting either way.
    pub async fn shutdown(&self) {
        let (reply, rx) = oneshot::channel();
        if let Err(e) = self.writer.enqueue(Write::Shutdown { reply }).await {
            tracing::warn!(error = %e, "history writer shutdown: enqueue failed");
            return;
        }
        if rx.await.is_err() {
            tracing::warn!("history writer shutdown: the writer stopped without replying");
        }
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

    async fn apply_schema(conn: &mut sqlx::SqliteConnection) -> Result<()> {
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
        .execute(&mut *conn)
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
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create fts table: {}", e)))?;

        // `end_reason` and the identity columns arrived after the table did;
        // add them to an existing database. SQLite has no ADD COLUMN IF NOT
        // EXISTS, so look first.
        for column in std::iter::once(&"end_reason").chain(IDENTITY_COLUMNS) {
            let present = sqlx::query("SELECT 1 FROM pragma_table_info('sessions') WHERE name = ?")
                .bind(column)
                .fetch_optional(&mut *conn)
                .await
                .map_err(|e| {
                    ApiError::Internal(format!("failed to inspect sessions table: {}", e))
                })?
                .is_some();
            if !present {
                // `column` comes from the compile-time list above, never input.
                sqlx::query(AssertSqlSafe(format!(
                    "ALTER TABLE sessions ADD COLUMN {column} TEXT"
                )))
                .execute(&mut *conn)
                .await
                .map_err(|e| ApiError::Internal(format!("failed to add {column}: {}", e)))?;
            }
        }

        // Index on created_at for date-range queries
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_sessions_created ON sessions(created_at)")
            .execute(&mut *conn)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to create index: {}", e)))?;

        Ok(())
    }

    /// Archive a session row (metadata only). The commit runs on the single
    /// writer connection; see [`history_writer`] for the conflict rules.
    pub async fn archive_session(&self, record: ArchiveRecord) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::Archive {
                record: Box::new(record),
                output: None,
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "archive").await
    }

    /// Update a session row's identity (WI-962). `None` leaves the column.
    pub async fn set_identity(
        &self,
        id: Uuid,
        role: Option<&'static str>,
        conversation: Option<&vogt_engine_contract::AgentConversation>,
    ) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::SetIdentity {
                id,
                role,
                conversation: conversation.cloned(),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "identity").await
    }

    /// Set or clear a session row's work item (WI-998).
    pub async fn set_work_item(&self, id: Uuid, work_item: Option<&str>) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::SetWorkItem {
                id,
                work_item: work_item.map(str::to_owned),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "work item").await
    }

    /// Queue a session's economics snapshot (WI-1245, WI-2703).
    ///
    /// The snapshot is coalesced on the writer: at most one `UPDATE` per
    /// session per [`history_writer::ECONOMICS_COALESCE`], and a newer
    /// snapshot replaces the pending one. This returns once the snapshot is
    /// queued, not once it is committed, so the economics tailer never waits
    /// on the history database. `true` means the snapshot was accepted. A
    /// row that does not exist yet updates nothing when the batch commits.
    pub async fn set_economics(&self, id: Uuid, economics: &str) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::SetEconomics {
                id,
                economics: economics.to_owned(),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "economics").await
    }

    /// List archived sessions
    pub async fn list_sessions(&self, limit: usize, offset: usize) -> Result<Vec<SessionMetadata>> {
        let limit = limit.min(200);
        let sessions = sqlx::query_as::<_, SessionMetadata>(AssertSqlSafe(format!(
            "SELECT {METADATA_COLUMNS} FROM sessions ORDER BY created_at DESC LIMIT ? OFFSET ?"
        )))
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
        let (db_bytes, log_file_count, log_bytes) = self.disk_stats().await?;
        Ok(HistoryStorageStats {
            archived_session_count,
            log_file_count,
            log_bytes,
            db_bytes,
        })
    }

    /// The db and log-dir footprint, rescanned off the runtime at most once
    /// per [`STORAGE_STATS_TTL`].
    async fn disk_stats(&self) -> Result<(u64, u64, u64)> {
        if let Some((at, stats)) = *self.disk_stats.lock() {
            if at.elapsed() < STORAGE_STATS_TTL {
                return Ok(stats);
            }
        }
        let db_path = self.db_path.clone();
        let log_dir = self.log_dir.clone();
        let stats = tokio::task::spawn_blocking(move || -> Result<(u64, u64, u64)> {
            let db_bytes = std::fs::metadata(&db_path)
                .map(|meta| meta.len())
                .unwrap_or(0);
            let (log_file_count, log_bytes) = summarize_regular_files(&log_dir)?;
            Ok((db_bytes, log_file_count as u64, log_bytes))
        })
        .await
        .map_err(|e| ApiError::Internal(format!("history storage scan panicked: {e}")))??;
        *self.disk_stats.lock() = Some((std::time::Instant::now(), stats));
        Ok(stats)
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

    /// Index session output for full-text search.
    pub async fn index_output(&self, session_id: Uuid, output: &str) -> Result<()> {
        self.replace_index_output(session_id, output).await
    }

    /// Replace indexed output for a session, on the writer connection.
    pub async fn replace_index_output(&self, session_id: Uuid, output: &str) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::ReplaceIndex {
                id: session_id,
                output: output.to_owned(),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "index").await
    }

    /// Archive metadata and replace the searchable output on the writer.
    pub async fn archive_session_with_output(
        &self,
        record: ArchiveRecord,
        output: &str,
    ) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::Archive {
                record: Box::new(record),
                output: Some(output.to_owned()),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "archive with output").await
    }

    /// One-shot startup pass: index raw session logs on disk that have no
    /// history row yet. See the writer for the row shape.
    pub async fn backfill_orphaned_logs(&self) -> Result<usize> {
        self.backfill_orphaned_logs_before(OffsetDateTime::now_utc())
            .await
    }

    /// [`Self::backfill_orphaned_logs`], closing out every recovered log last
    /// written before `booted_at`.
    pub async fn backfill_orphaned_logs_before(&self, booted_at: OffsetDateTime) -> Result<usize> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::Backfill {
                booted_at,
                log_dir: self.log_dir.clone(),
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "backfill").await
    }

    /// Close out rows a previous engine process left unfinished.
    pub async fn reconcile_unfinished(&self) -> Result<usize> {
        let (reply, rx) = oneshot::channel();
        self.writer.enqueue(Write::Reconcile { reply }).await?;
        history_writer::writer_reply(rx, "reconcile").await
    }

    /// Get session by ID
    pub async fn get_session(&self, id: Uuid) -> Result<SessionMetadata> {
        let session = sqlx::query_as::<_, SessionMetadata>(AssertSqlSafe(format!(
            "SELECT {METADATA_COLUMNS} FROM sessions WHERE id = ?"
        )))
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

    /// Clean up old sessions beyond the retention period, on the writer.
    pub async fn cleanup_old_sessions(&self, retention_days: u32) -> Result<usize> {
        let (reply, rx) = oneshot::channel();
        self.writer
            .enqueue(Write::Cleanup {
                retention_days,
                reply,
            })
            .await?;
        history_writer::writer_reply(rx, "cleanup").await
    }

    /// Delete archived metadata, searchable output, and the raw log.
    pub async fn delete_session(&self, id: Uuid) -> Result<bool> {
        let (reply, rx) = oneshot::channel();
        self.writer.enqueue(Write::Delete { id, reply }).await?;
        history_writer::writer_reply(rx, "delete").await
    }
}

pub(crate) fn user_query_to_fts(query: &str) -> Option<String> {
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

pub(crate) fn remove_log_file(path: PathBuf) -> Result<()> {
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
                    work_item: Some("WI-7".into()),
                }),
                economics: None,
            })
            .await
            .unwrap();
        let conversation = vogt_engine_contract::AgentConversation {
            agent: "claude".into(),
            id: "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f".into(),
        };
        assert!(history
            .set_identity(id, None, Some(&conversation))
            .await
            .unwrap());
        assert!(history
            .set_identity(id, Some("oversight"), None)
            .await
            .unwrap());
        assert!(
            !history
                .set_identity(Uuid::new_v4(), Some("worker"), None)
                .await
                .unwrap(),
            "no row yet is a miss, not a silent success"
        );
        let row = history.get_session(id).await.unwrap();
        assert_eq!(row.role.as_deref(), Some("oversight"));
        assert_eq!(row.conversation_agent.as_deref(), Some("claude"));
        assert_eq!(
            row.work_item.as_deref(),
            Some("WI-7"),
            "set_identity keeps it"
        );
        // A rebind replaces the work item; an unbind clears it (WI-998).
        assert!(history.set_work_item(id, Some("WI-8")).await.unwrap());
        assert_eq!(
            history.get_session(id).await.unwrap().work_item.as_deref(),
            Some("WI-8")
        );
        assert!(history.set_work_item(id, None).await.unwrap());
        assert!(!history.set_work_item(Uuid::new_v4(), None).await.unwrap());
        assert_eq!(history.get_session(id).await.unwrap().work_item, None);
        assert!(history.set_work_item(id, Some("WI-8")).await.unwrap());
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
                economics: None,
            })
            .await
            .unwrap();
        let row = history.get_session(id).await.unwrap();
        assert_eq!(
            row.conversation_id.as_deref(),
            Some(conversation.id.as_str())
        );
        assert_eq!(row.role.as_deref(), Some("oversight"));
        assert_eq!(
            row.work_item.as_deref(),
            Some("WI-8"),
            "a finalize keeps it"
        );
    }

    async fn read_economics(history: &SessionHistory, id: Uuid) -> Option<String> {
        // The list/get shape does not carry economics (WI-1245 stores it for
        // the tailer, not the session list). Read the column directly.
        let row = sqlx::query("SELECT economics FROM sessions WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&history.pool)
            .await
            .unwrap();
        row.get("economics")
    }

    fn archive_row(id: Uuid, name: &str) -> ArchiveRecord {
        ArchiveRecord {
            id,
            name: name.into(),
            created_at: OffsetDateTime::now_utc(),
            ended_at: None,
            exit_code: None,
            cwd: None,
            command: Some("bash".into()),
            scrollback_bytes: 0,
            end_reason: None,
            identity: None,
            economics: None,
        }
    }

    /// Concurrent archives, economics and work-item writes share one writer
    /// connection, so a caller never sees SQLITE_BUSY from our own writers.
    #[tokio::test]
    async fn concurrent_writes_do_not_return_sqlite_busy() {
        let dir = tempfile::tempdir().unwrap();
        let history = std::sync::Arc::new(SessionHistory::new(dir.path()).await.unwrap());
        let mut joins = Vec::new();
        for n in 0..24 {
            let history = history.clone();
            joins.push(tokio::spawn(async move {
                let id = Uuid::new_v4();
                history
                    .archive_session(archive_row(id, &format!("s{n}")))
                    .await?;
                history.set_economics(id, r#"{"turns":1}"#).await?;
                history.set_work_item(id, Some("WI-2703")).await?;
                history.set_economics(id, r#"{"turns":2}"#).await?;
                Ok::<(), ApiError>(())
            }));
        }
        for join in joins {
            let result = join.await.unwrap();
            if let Err(ref e) = result {
                let err = e.to_string();
                assert!(
                    !err.to_lowercase().contains("locked") && !err.contains("SQLITE_BUSY"),
                    "a caller saw a lock error: {err}"
                );
            }
            result.unwrap();
        }
    }

    /// Two economics snapshots inside the coalesce window become one write,
    /// and a read before the flush does not show the uncommitted snapshot.
    #[tokio::test]
    async fn economics_snapshots_coalesce_until_flushed() {
        let dir = tempfile::tempdir().unwrap();
        let history = SessionHistory::new(dir.path()).await.unwrap();
        let id = Uuid::new_v4();
        history
            .archive_session(archive_row(id, "eco"))
            .await
            .unwrap();
        assert!(history.set_economics(id, r#"{"turns":1}"#).await.unwrap());
        assert!(history.set_economics(id, r#"{"turns":9}"#).await.unwrap());
        // Nothing has been committed yet: the row still has no snapshot.
        assert_eq!(read_economics(&history, id).await, None);
        let wrote = history.flush_economics().await.unwrap();
        assert_eq!(wrote, 1, "the two snapshots are one write");
        assert_eq!(
            read_economics(&history, id).await.as_deref(),
            Some(r#"{"turns":9}"#)
        );
    }

    /// A burst of writes does not stall a list: the list uses the read pool
    /// and never waits on the writer.
    #[tokio::test]
    async fn list_sessions_stays_fast_under_write_load() {
        let dir = tempfile::tempdir().unwrap();
        let history = std::sync::Arc::new(SessionHistory::new(dir.path()).await.unwrap());
        for n in 0..8 {
            history
                .archive_session(archive_row(Uuid::new_v4(), &format!("seed{n}")))
                .await
                .unwrap();
        }
        let writer = history.clone();
        let burst = tokio::spawn(async move {
            for n in 0..40 {
                writer
                    .archive_session(archive_row(Uuid::new_v4(), &format!("burst{n}")))
                    .await
                    .unwrap();
                writer
                    .set_economics(Uuid::new_v4(), r#"{"turns":1}"#)
                    .await
                    .unwrap();
            }
        });
        let started = std::time::Instant::now();
        let listed = history.list_sessions(50, 0).await.unwrap();
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "list took {elapsed:?} under write load"
        );
        assert!(!listed.is_empty());
        burst.await.unwrap();
    }

    /// A shutdown flushes coalesced economics, drains the queue and stops
    /// the writer — the graceful-shutdown path loses no pending snapshot.
    #[tokio::test]
    async fn shutdown_flushes_pending_economics() {
        let dir = tempfile::tempdir().unwrap();
        let history = SessionHistory::new(dir.path()).await.unwrap();
        let id = Uuid::new_v4();
        history
            .archive_session(archive_row(id, "shut"))
            .await
            .unwrap();
        assert!(history.set_economics(id, r#"{"turns":3}"#).await.unwrap());
        history.shutdown().await;
        assert_eq!(
            read_economics(&history, id).await.as_deref(),
            Some(r#"{"turns":3}"#),
            "the shutdown flushed the coalesced snapshot"
        );
    }

    /// An economics flush that fails before an archive is logged and does
    /// not stop the archive: the row is still written.
    #[tokio::test]
    async fn a_failed_economics_write_still_archives() {
        let dir = tempfile::tempdir().unwrap();
        let history = SessionHistory::new(dir.path()).await.unwrap();
        let id = Uuid::new_v4();
        history
            .archive_session(archive_row(id, "econ-fail"))
            .await
            .unwrap();
        assert!(history.set_economics(id, r#"{"turns":1}"#).await.unwrap());
        // Fail every economics UPDATE while leaving INSERT alone. A trigger
        // is the deterministic way to fail exactly that one statement; a
        // dropped column or closed database would fail the archive too.
        // Create it on a writable connection: the session-history read pool
        // is read-only.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(dir.path().join("history.db"))
                    .journal_mode(SqliteJournalMode::Wal),
            )
            .await
            .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_econ BEFORE UPDATE OF economics ON sessions \
             BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .execute(&pool)
        .await
        .unwrap();
        // Delete the provisional row so the next archive is an INSERT, not
        // the upsert — the upsert also touches economics and would fail.
        history.delete_session(id).await.unwrap();
        let mut record = archive_row(id, "econ-fail");
        record.ended_at = Some(OffsetDateTime::now_utc());
        history
            .archive_session(record)
            .await
            .expect("the archive still lands when the coalesced economics write fails");
        let row = history.get_session(id).await.unwrap();
        assert_eq!(row.name, "econ-fail");
        assert!(row.ended_at.is_some(), "the archive row was written");
    }
}
