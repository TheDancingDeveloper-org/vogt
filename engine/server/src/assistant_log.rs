//! Durable, attributable log of every assistant interaction (FR-T14).
//!
//! The assistant's live conversation is a single in-memory `Conversation`
//! (`assistant.rs`), capped and lost on restart. That is enough to *drive* a
//! turn and wrong for the question FR-T14 asks: what did or did not happen, who
//! drove it, and does it survive the process. FR-T3 audits the *write* an
//! assistant caused; this records the conversation that did or did not cause
//! one — every refusal, every expired card, every question a person asked and
//! acted on themselves, none of which reach the core's audit log at all.
//!
//! ## Where it lives, and why
//!
//! A small SQLite file under the engine's `state_dir`, beside `history.db` and
//! the push subscriptions — the pattern the engine already uses for its own
//! durable state, and the one that answers FR-E9: the log is the *engine's*,
//! not the core's, so an absent or unreachable vogt-core costs Vogt features
//! and never the record of the conversation. The engine boots, records and
//! serves this log with no core configured at all.
//!
//! ## What it does not store
//!
//! No audio (FR-T12's rule, restated by FR-T14): text and structure only. The
//! recognised utterance, its repaired form where FR-T13's pass changed it, the
//! composed request, the reply, every tool call and result, and every
//! pending-action outcome are enough to reconstruct a voice interaction without
//! retaining a recording of somebody's voice.
//!
//! ## Untrusted content stays untrusted
//!
//! Tool results are logged exactly as the loop fed them to the model, which
//! means the ones carrying external content are already wrapped in their
//! `<vogt-data>` / `<terminal-output>` / `<tool-error>` delimiters (FR-T4).
//! Stored verbatim, they read back to a person or a model still delimited as
//! data — the rule the requirement asks to hold "including when the log is
//! later read back to a model" holds because nothing here unwraps them.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use time::OffsetDateTime;

use crate::error::{ApiError, Result};

/// One thing that happened in a turn, in the direction it happened.
///
/// The kind is the discriminator stored in its own column so the log can be
/// filtered and counted without parsing every payload; the direction makes the
/// "both directions" property (FR-T14) a fact a reader can select on rather
/// than infer.
#[derive(Debug, Clone)]
pub enum LogEvent {
    /// A recognised voice utterance. `repaired` is set only when FR-T13's
    /// repair pass changed the raw form before it became the request.
    Utterance {
        raw: String,
        repaired: Option<String>,
    },
    /// The composed request the user sent (voice or typed).
    Request { text: String },
    /// The model's final spoken/typed reply for the turn.
    Reply { text: String },
    /// A tool the model asked to run, with the arguments it proposed.
    ToolCall { name: String, arguments: Value },
    /// A tool result fed back to the model. `content` is stored exactly as the
    /// loop produced it, so external content keeps its FR-T4 delimiters.
    ToolResult { name: String, content: String },
    /// An FR-T2 pending action and its outcome. Logged once when proposed
    /// (`outcome = "proposed"`) and again when it resolves
    /// (`approved` / `denied` / `expired`), so a card that never resolves
    /// because the process restarted is still visible as having been offered.
    PendingAction {
        action_id: String,
        /// `send_input` or `vogt_write`.
        action: String,
        /// One-line human summary of what it touches.
        target: String,
        /// The registry operation, for a Vogt write.
        operation: Option<String>,
        outcome: String,
    },
    /// The backend was unavailable or malformed for this turn. Not the model's
    /// words, so it is recorded as a system event rather than a reply.
    BackendError { message: String },
}

impl LogEvent {
    fn kind(&self) -> &'static str {
        match self {
            LogEvent::Utterance { .. } => "utterance",
            LogEvent::Request { .. } => "request",
            LogEvent::Reply { .. } => "reply",
            LogEvent::ToolCall { .. } => "tool_call",
            LogEvent::ToolResult { .. } => "tool_result",
            LogEvent::PendingAction { .. } => "pending_action",
            LogEvent::BackendError { .. } => "backend_error",
        }
    }

    /// `user` (into the assistant), `assistant` (out of it), `tool` (the
    /// effector/read boundary) or `system` (the engine itself).
    fn direction(&self) -> &'static str {
        match self {
            LogEvent::Utterance { .. } | LogEvent::Request { .. } => "user",
            LogEvent::Reply { .. } => "assistant",
            LogEvent::ToolCall { .. }
            | LogEvent::ToolResult { .. }
            | LogEvent::PendingAction { .. } => "tool",
            LogEvent::BackendError { .. } => "system",
        }
    }

    fn payload(&self) -> Value {
        match self {
            LogEvent::Utterance { raw, repaired } => match repaired {
                Some(repaired) => json!({ "raw": raw, "repaired": repaired }),
                None => json!({ "raw": raw }),
            },
            LogEvent::Request { text } => json!({ "text": text }),
            LogEvent::Reply { text } => json!({ "text": text }),
            LogEvent::ToolCall { name, arguments } => {
                json!({ "name": name, "arguments": arguments })
            }
            LogEvent::ToolResult { name, content } => json!({ "name": name, "content": content }),
            LogEvent::PendingAction {
                action_id,
                action,
                target,
                operation,
                outcome,
            } => {
                let mut value = json!({
                    "action_id": action_id,
                    "action": action,
                    "target": target,
                    "outcome": outcome,
                });
                if let Some(operation) = operation {
                    value["operation"] = json!(operation);
                }
                value
            }
            LogEvent::BackendError { message } => json!({ "message": message }),
        }
    }
}

/// One row read back from the log — the discriminator columns beside the
/// kind-specific payload the client renders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggedEntry {
    pub seq: i64,
    pub at: String,
    pub actor: String,
    pub kind: String,
    pub direction: String,
    pub payload: Value,
}

/// How much of the log to read, and whose.
#[derive(Debug, Clone)]
pub struct ListQuery {
    pub limit: usize,
    pub offset: usize,
    /// Restrict to one actor's interactions.
    pub actor: Option<String>,
}

impl Default for ListQuery {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
            actor: None,
        }
    }
}

/// The durable interaction log. One append-only table, engine-local.
pub struct AssistantLog {
    pool: SqlitePool,
    db_path: PathBuf,
}

impl AssistantLog {
    /// Open (creating if absent) the log database under `state_dir`.
    pub async fn new(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| ApiError::Internal(format!("failed to create assistant log dir: {e}")))?;
        let db_path = state_dir.join("assistant-log.db");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| {
                ApiError::Internal(format!("failed to connect to assistant log db: {e}"))
            })?;
        let log = Self { pool, db_path };
        log.init_schema().await?;
        Ok(log)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    async fn init_schema(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS entries (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                at TEXT NOT NULL,
                actor TEXT NOT NULL,
                kind TEXT NOT NULL,
                direction TEXT NOT NULL,
                payload TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to create entries table: {e}")))?;

        sqlx::query("CREATE INDEX IF NOT EXISTS idx_entries_at ON entries(at)")
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to create at index: {e}")))?;
        sqlx::query("CREATE INDEX IF NOT EXISTS idx_entries_actor ON entries(actor)")
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to create actor index: {e}")))?;

        Ok(())
    }

    /// Append one event, attributed to `actor`. Durable on return.
    pub async fn record(&self, actor: &str, event: LogEvent) -> Result<()> {
        let at = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {e}")))?;
        let payload = serde_json::to_string(&event.payload())
            .map_err(|e| ApiError::Internal(format!("failed to encode log payload: {e}")))?;
        sqlx::query(
            r#"
            INSERT INTO entries (at, actor, kind, direction, payload)
            VALUES (?, ?, ?, ?, ?)
            "#,
        )
        .bind(at)
        .bind(actor)
        .bind(event.kind())
        .bind(event.direction())
        .bind(payload)
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to record log entry: {e}")))?;
        Ok(())
    }

    /// Read entries newest-first, optionally for one actor.
    pub async fn list(&self, query: ListQuery) -> Result<Vec<LoggedEntry>> {
        let limit = query.limit.clamp(1, 500) as i64;
        let offset = query.offset as i64;
        let rows = match query.actor.as_deref() {
            Some(actor) => {
                sqlx::query(
                    r#"
                SELECT seq, at, actor, kind, direction, payload
                FROM entries
                WHERE actor = ?
                ORDER BY seq DESC
                LIMIT ? OFFSET ?
                "#,
                )
                .bind(actor)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"
                SELECT seq, at, actor, kind, direction, payload
                FROM entries
                ORDER BY seq DESC
                LIMIT ? OFFSET ?
                "#,
                )
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| ApiError::Internal(format!("failed to read assistant log: {e}")))?;

        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let payload_raw: String = row.get("payload");
            let payload: Value = serde_json::from_str(&payload_raw).unwrap_or(Value::Null);
            entries.push(LoggedEntry {
                seq: row.get("seq"),
                at: row.get("at"),
                actor: row.get("actor"),
                kind: row.get("kind"),
                direction: row.get("direction"),
                payload,
            });
        }
        Ok(entries)
    }

    pub async fn count(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COUNT(*) AS count FROM entries")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to count log entries: {e}")))?;
        Ok(row.get::<i64, _>("count") as u64)
    }

    /// Delete entries older than the retention horizon. Enforced on a schedule
    /// (see `app::spawn_assistant_log_retention_sweeper`) so the horizon is a
    /// configured maximum rather than whatever the last caller passed.
    pub async fn cleanup(&self, retention_days: u32) -> Result<usize> {
        let cutoff = OffsetDateTime::now_utc() - time::Duration::days(retention_days as i64);
        let cutoff_str = cutoff
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {e}")))?;
        let result = sqlx::query("DELETE FROM entries WHERE at < ?")
            .bind(cutoff_str)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("assistant log cleanup failed: {e}")))?;
        Ok(result.rows_affected() as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn temp_log() -> (tempfile::TempDir, AssistantLog) {
        let dir = tempfile::tempdir().unwrap();
        let log = AssistantLog::new(dir.path()).await.unwrap();
        (dir, log)
    }

    #[tokio::test]
    async fn records_and_reads_back_newest_first() {
        let (_dir, log) = temp_log().await;
        log.record(
            "phone",
            LogEvent::Request {
                text: "what's the top bug".into(),
            },
        )
        .await
        .unwrap();
        log.record(
            "phone",
            LogEvent::Reply {
                text: "WI-7".into(),
            },
        )
        .await
        .unwrap();

        let entries = log.list(ListQuery::default()).await.unwrap();
        assert_eq!(entries.len(), 2);
        // Newest first.
        assert_eq!(entries[0].kind, "reply");
        assert_eq!(entries[1].kind, "request");
        assert_eq!(entries[0].actor, "phone");
        assert_eq!(entries[1].payload["text"], json!("what's the top bug"));
    }

    #[tokio::test]
    async fn a_repaired_utterance_carries_both_forms() {
        let (_dir, log) = temp_log().await;
        log.record(
            "phone",
            LogEvent::Utterance {
                raw: "close whiskey seven".into(),
                repaired: Some("close WI-7".into()),
            },
        )
        .await
        .unwrap();
        let entries = log.list(ListQuery::default()).await.unwrap();
        assert_eq!(entries[0].payload["raw"], json!("close whiskey seven"));
        assert_eq!(entries[0].payload["repaired"], json!("close WI-7"));
    }

    #[tokio::test]
    async fn survives_a_reopen_of_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        {
            let log = AssistantLog::new(dir.path()).await.unwrap();
            log.record(
                "primary",
                LogEvent::Request {
                    text: "before restart".into(),
                },
            )
            .await
            .unwrap();
        }
        // A fresh process, same state dir.
        let reopened = AssistantLog::new(dir.path()).await.unwrap();
        let entries = reopened.list(ListQuery::default()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].payload["text"], json!("before restart"));
    }

    #[tokio::test]
    async fn filters_by_actor() {
        let (_dir, log) = temp_log().await;
        log.record("phone", LogEvent::Reply { text: "a".into() })
            .await
            .unwrap();
        log.record("desktop", LogEvent::Reply { text: "b".into() })
            .await
            .unwrap();
        let only_phone = log
            .list(ListQuery {
                actor: Some("phone".into()),
                ..ListQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(only_phone.len(), 1);
        assert_eq!(only_phone[0].actor, "phone");
    }
}
