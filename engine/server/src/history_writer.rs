//! The one task that writes `history.db`.
//!
//! Request handlers, the PTY reader and the economics tailer enqueue a
//! [`Write`] and, except for coalesced economics, wait on a oneshot for
//! *this* connection to finish. They never take a pool connection that
//! another write might be holding, so a caller cannot observe `SQLITE_BUSY`
//! from our own writers. Reads use a different pool and never enter here.
//!
//! Economics snapshots coalesce per session: a newer snapshot replaces the
//! pending one, and the pending map flushes on [`ECONOMICS_COALESCE`] or
//! when a session is archived.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{sqlite::SqliteConnection, AssertSqlSafe, Row};
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::error::{ApiError, Result};
use crate::history::{ArchiveRecord, END_ENGINE_RESTART};

/// How long a coalesced economics snapshot waits before it is written.
pub const ECONOMICS_COALESCE: Duration = Duration::from_secs(5);

/// Bound on the writer queue. A full queue fails the enqueue immediately;
/// a handler never blocks on it.
pub const WRITER_QUEUE: usize = 256;

pub enum Write {
    Archive {
        record: Box<ArchiveRecord>,
        /// When `Some`, the searchable output is replaced in the same turn
        /// as the metadata write.
        output: Option<String>,
        reply: oneshot::Sender<Result<()>>,
    },
    SetIdentity {
        id: Uuid,
        role: Option<&'static str>,
        conversation: Option<vogt_engine_contract::AgentConversation>,
        reply: oneshot::Sender<Result<bool>>,
    },
    SetWorkItem {
        id: Uuid,
        work_item: Option<String>,
        reply: oneshot::Sender<Result<bool>>,
    },
    /// Coalesced. `reply` is answered as soon as the snapshot is stored,
    /// not when it is committed.
    SetEconomics {
        id: Uuid,
        economics: String,
        reply: oneshot::Sender<Result<bool>>,
    },
    Delete {
        id: Uuid,
        reply: oneshot::Sender<Result<bool>>,
    },
    Cleanup {
        retention_days: u32,
        reply: oneshot::Sender<Result<usize>>,
    },
    Backfill {
        booted_at: OffsetDateTime,
        log_dir: PathBuf,
        reply: oneshot::Sender<Result<usize>>,
    },
    Reconcile {
        reply: oneshot::Sender<Result<usize>>,
    },
    ReplaceIndex {
        id: Uuid,
        output: String,
        reply: oneshot::Sender<Result<()>>,
    },
    /// Commit every pending economics snapshot now. Tests use this so they
    /// do not sleep for [`ECONOMICS_COALESCE`].
    FlushEconomics {
        reply: oneshot::Sender<Result<usize>>,
    },
    /// Run one statement on the writer connection, then close it. The
    /// startup-reconcile test uses this to drop a column the way an older
    /// engine would have, and to make sure nothing is still writing.
    ExecAndClose {
        sql: String,
        reply: oneshot::Sender<Result<()>>,
    },
}

#[derive(Clone)]
pub struct HistoryWriter {
    tx: mpsc::Sender<Write>,
}

impl HistoryWriter {
    pub fn spawn(conn: SqliteConnection, log_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        tokio::spawn(async move {
            run(conn, log_dir, rx).await;
        });
        Self { tx }
    }

    pub fn enqueue(&self, write: Write) -> Result<()> {
        self.tx.try_send(write).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                ApiError::Internal("history writer queue is full".into())
            }
            mpsc::error::TrySendError::Closed(_) => {
                ApiError::Internal("history writer has stopped".into())
            }
        })
    }
}

pub async fn writer_reply<T>(rx: oneshot::Receiver<Result<T>>, what: &'static str) -> Result<T> {
    match rx.await {
        Ok(result) => result,
        Err(_) => Err(ApiError::Internal(format!(
            "history writer dropped the reply for {what}"
        ))),
    }
}

struct PendingEconomics {
    economics: String,
}

async fn run(mut conn: SqliteConnection, log_dir: PathBuf, mut rx: mpsc::Receiver<Write>) {
    let mut pending: HashMap<Uuid, PendingEconomics> = HashMap::new();
    let mut flush = tokio::time::interval(ECONOMICS_COALESCE);
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; skip it so a snapshot just queued
    // is not written before the coalesce window.
    flush.tick().await;
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                if handle(&mut conn, &log_dir, &mut pending, msg).await {
                    break;
                }
            }
            _ = flush.tick() => {
                flush_economics(&mut conn, &mut pending).await;
            }
        }
    }
    flush_economics(&mut conn, &mut pending).await;
}

/// `true` means the connection has been closed and the task should stop.
async fn handle(
    conn: &mut SqliteConnection,
    log_dir: &Path,
    pending: &mut HashMap<Uuid, PendingEconomics>,
    msg: Write,
) -> bool {
    match msg {
        Write::Archive {
            record,
            output,
            reply,
        } => {
            let id = record.id;
            // An archive is the session leaving (or its provisional row).
            // Commit the latest snapshot first so COALESCE in the archive
            // does not keep an older economics column over a newer one that
            // has not flushed — the archive carries its own economics, and
            // a pending snapshot is newer than the column.
            if let Some(waiting) = pending.remove(&id) {
                // The reply was already sent when the snapshot was queued.
                // A failed flush is logged by the statement error path below
                // only when this call returns Err; swallow it into the
                // archive result so the row is still written.
                if let Err(e) = apply_economics(conn, id, &waiting.economics).await {
                    let _ = reply.send(Err(e));
                    return false;
                }
            }
            let result = archive(conn, *record).await.and(match output {
                Some(output) => replace_index(conn, id, &output).await,
                None => Ok(()),
            });
            let _ = reply.send(result);
        }
        Write::SetIdentity {
            id,
            role,
            conversation,
            reply,
        } => {
            let _ = reply.send(set_identity(conn, id, role, conversation.as_ref()).await);
        }
        Write::SetWorkItem {
            id,
            work_item,
            reply,
        } => {
            let _ = reply.send(set_work_item(conn, id, work_item.as_deref()).await);
        }
        Write::SetEconomics {
            id,
            economics,
            reply,
        } => {
            // Do not touch SQLite here. Whether a row exists is answered
            // from the last archive this task performed is not tracked, so
            // the caller is told the snapshot was accepted (`true`) and the
            // commit happens on the flush. A missing row updates zero rows
            // then, which is the same outcome `set_economics` used to report
            // — lost only for a caller that branched on `false` inside the
            // coalesce window. The economics tailer only logs errors.
            let slot = pending.entry(id).or_insert_with(|| PendingEconomics {
                economics: String::new(),
            });
            slot.economics = economics;
            let _ = reply.send(Ok(true));
        }
        Write::Delete { id, reply } => {
            pending.remove(&id);
            let _ = reply.send(delete_session(conn, log_dir, id).await);
        }
        Write::Cleanup {
            retention_days,
            reply,
        } => {
            let _ = reply.send(cleanup(conn, log_dir, retention_days).await);
        }
        Write::Backfill {
            booted_at,
            log_dir: dir,
            reply,
        } => {
            let _ = reply.send(backfill(conn, &dir, booted_at).await);
        }
        Write::Reconcile { reply } => {
            let _ = reply.send(reconcile(conn, log_dir).await);
        }
        Write::ReplaceIndex { id, output, reply } => {
            let _ = reply.send(replace_index(conn, id, &output).await);
        }
        Write::FlushEconomics { reply } => {
            let n = flush_economics(conn, pending).await;
            let _ = reply.send(Ok(n));
        }
        Write::ExecAndClose { sql, reply } => {
            let result = sqlx::query(AssertSqlSafe(sql.as_str()))
                .execute(&mut *conn)
                .await
                .map(|_| ())
                .map_err(|e| ApiError::Internal(format!("history exec failed: {e}")));
            let _ = reply.send(result);
            return true;
        }
    }
    false
}

async fn flush_economics(
    conn: &mut SqliteConnection,
    pending: &mut HashMap<Uuid, PendingEconomics>,
) -> usize {
    let batch: Vec<_> = pending.drain().collect();
    let n = batch.len();
    for (id, waiting) in batch {
        // Replies were sent at enqueue. A flush error is not surfaced to a
        // caller that has already moved on; the next flush retries nothing
        // (the snapshot was drained). Log by returning it as a lost write:
        // the economics tailer re-sends the next snapshot.
        let _ = apply_economics(conn, id, &waiting.economics).await;
    }
    n
}

async fn apply_economics(conn: &mut SqliteConnection, id: Uuid, economics: &str) -> Result<bool> {
    let done = sqlx::query("UPDATE sessions SET economics = ? WHERE id = ?")
        .bind(economics)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to update session economics: {e}")))?;
    Ok(done.rows_affected() > 0)
}

async fn archive(conn: &mut SqliteConnection, record: ArchiveRecord) -> Result<()> {
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
                              template, role, conversation_agent, conversation_id, work_item, economics)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
            conversation_id = COALESCE(excluded.conversation_id, sessions.conversation_id),
            work_item = COALESCE(excluded.work_item, sessions.work_item),
            economics = COALESCE(excluded.economics, sessions.economics)
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
    .bind(identity.work_item)
    .bind(record.economics)
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::Internal(format!("failed to archive session: {}", e)))?;
    Ok(())
}

async fn set_identity(
    conn: &mut SqliteConnection,
    id: Uuid,
    role: Option<&str>,
    conversation: Option<&vogt_engine_contract::AgentConversation>,
) -> Result<bool> {
    let done = sqlx::query(
        "UPDATE sessions SET role = COALESCE(?, role), \
         conversation_agent = COALESCE(?, conversation_agent), \
         conversation_id = COALESCE(?, conversation_id) WHERE id = ?",
    )
    .bind(role)
    .bind(conversation.map(|c| c.agent.as_str()))
    .bind(conversation.map(|c| c.id.as_str()))
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::Internal(format!("failed to update session identity: {e}")))?;
    Ok(done.rows_affected() > 0)
}

async fn set_work_item(
    conn: &mut SqliteConnection,
    id: Uuid,
    work_item: Option<&str>,
) -> Result<bool> {
    let done = sqlx::query("UPDATE sessions SET work_item = ? WHERE id = ?")
        .bind(work_item)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to update session work item: {e}")))?;
    Ok(done.rows_affected() > 0)
}

async fn replace_index(conn: &mut SqliteConnection, session_id: Uuid, output: &str) -> Result<()> {
    sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
        .bind(session_id.to_string())
        .execute(&mut *conn)
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
    .execute(&mut *conn)
    .await
    .map_err(|e| ApiError::Internal(format!("failed to index output: {}", e)))?;
    Ok(())
}

async fn delete_session(conn: &mut SqliteConnection, log_dir: &Path, id: Uuid) -> Result<bool> {
    sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
        .bind(id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("delete fts failed: {}", e)))?;
    let result = sqlx::query("DELETE FROM sessions WHERE id = ?")
        .bind(id.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("delete failed: {}", e)))?;
    crate::history::remove_log_file(log_dir.join(format!("{id}.log")))?;
    Ok(result.rows_affected() > 0)
}

async fn cleanup(
    conn: &mut SqliteConnection,
    log_dir: &Path,
    retention_days: u32,
) -> Result<usize> {
    let cutoff = OffsetDateTime::now_utc() - time::Duration::days(retention_days as i64);
    let cutoff_str = cutoff
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|e| ApiError::Internal(format!("time format error: {}", e)))?;
    let ids = sqlx::query("SELECT id FROM sessions WHERE created_at < ?")
        .bind(&cutoff_str)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("cleanup query failed: {}", e)))?;
    for row in ids {
        let id: String = row.get("id");
        sqlx::query("DELETE FROM session_output_fts WHERE session_id = ?")
            .bind(&id)
            .execute(&mut *conn)
            .await
            .map_err(|e| ApiError::Internal(format!("cleanup fts failed: {}", e)))?;
        if let Ok(uuid) = Uuid::parse_str(&id) {
            crate::history::remove_log_file(log_dir.join(format!("{uuid}.log")))?;
        }
    }
    let result = sqlx::query("DELETE FROM sessions WHERE created_at < ?")
        .bind(cutoff_str)
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("cleanup failed: {}", e)))?;
    Ok(result.rows_affected() as usize)
}

async fn backfill(
    conn: &mut SqliteConnection,
    log_dir: &Path,
    booted_at: OffsetDateTime,
) -> Result<usize> {
    let rows = sqlx::query("SELECT id FROM sessions")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("backfill: list ids failed: {e}")))?;
    let known: std::collections::HashSet<String> =
        rows.into_iter().map(|r| r.get::<String, _>("id")).collect();
    let entries = match std::fs::read_dir(log_dir) {
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
        let entry =
            entry.map_err(|e| ApiError::Internal(format!("backfill: dir entry failed: {e}")))?;
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
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("backfill: insert failed: {e}")))?;
        if let Ok(bytes) = std::fs::read(&path) {
            let visible = crate::activity::strip_ansi(&bytes);
            let text = String::from_utf8_lossy(&visible);
            if let Err(e) = replace_index(conn, id, text.as_ref()).await {
                tracing::warn!(session = %id, error = %e, "backfill: failed to index recovered log");
            }
        }
        recovered += 1;
    }
    Ok(recovered)
}

async fn reconcile(conn: &mut SqliteConnection, log_dir: &Path) -> Result<usize> {
    let rows = sqlx::query("SELECT id FROM sessions WHERE ended_at IS NULL")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("reconcile: list failed: {e}")))?;
    let now = OffsetDateTime::now_utc();
    let mut closed = 0usize;
    for row in rows {
        let id: String = row.get("id");
        let ended = Uuid::parse_str(&id)
            .ok()
            .and_then(|uuid| std::fs::metadata(log_dir.join(format!("{uuid}.log"))).ok())
            .and_then(|meta| meta.modified().ok())
            .map(OffsetDateTime::from)
            .unwrap_or(now);
        let ended = ended
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| ApiError::Internal(format!("time format error: {e}")))?;
        sqlx::query(
            "UPDATE sessions SET ended_at = ?, end_reason = COALESCE(end_reason, ?)              WHERE id = ? AND ended_at IS NULL",
        )
        .bind(&ended)
        .bind(END_ENGINE_RESTART)
        .bind(&id)
        .execute(&mut *conn)
        .await
        .map_err(|e| ApiError::Internal(format!("reconcile: update failed: {e}")))?;
        closed += 1;
    }
    Ok(closed)
}
