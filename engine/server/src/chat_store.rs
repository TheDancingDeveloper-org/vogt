//! Durable record of quick chats (WI-1097): what each chat is and everything
//! said in it.
//!
//! A small SQLite file under `state_dir`, beside `history.db` and the
//! assistant log, for the same reason those live there: it is the engine's
//! own state and needs no core. Unlike session history it has **no retention
//! sweep** — a chat is meant to be found again a week or a year later, so
//! nothing here deletes a row. Archiving only hides a chat from the default
//! list.
//!
//! The agent CLI keeps its own transcript too (Klaudia's JSONL under
//! `~/.klaudia/sessions`), and that is what `--resume` continues from. This
//! is the readable, searchable copy Vogt shows: text, tool calls and their
//! (truncated) results, approvals and errors, with a full-text index over all
//! of it.

use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row;
use uuid::Uuid;
use vogt_engine_contract::{ChatEntry, ChatState, ChatSummary};

use crate::error::{ApiError, Result};

/// A chat's stored record. The live parts of a [`ChatSummary`] (`state`,
/// `live`) are the runtime's and are filled in by it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatRecord {
    pub id: Uuid,
    pub title: String,
    pub driver: String,
    pub model: Option<String>,
    pub creator: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived: bool,
    pub work_item: Option<String>,
    pub promoted_session: Option<Uuid>,
    /// Whether the driver has been launched for this chat at least once, so
    /// its conversation exists and the next launch resumes it.
    pub started: bool,
    pub message_count: u64,
    pub preview: Option<String>,
}

impl ChatRecord {
    pub fn summary(&self, state: ChatState, live: bool) -> ChatSummary {
        ChatSummary {
            id: self.id,
            title: self.title.clone(),
            driver: self.driver.clone(),
            model: self.model.clone(),
            creator: self.creator.clone(),
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            archived: self.archived,
            work_item: self.work_item.clone(),
            promoted_session: self.promoted_session,
            state,
            live,
            message_count: self.message_count,
            preview: self.preview.clone(),
        }
    }
}

/// How a list is filtered.
#[derive(Debug, Clone, Default)]
pub struct ChatQuery {
    /// Full-text terms, matched against titles and every entry.
    pub q: Option<String>,
    /// `Some(true)` archived only, `Some(false)` unarchived only, `None` both.
    pub archived: Option<bool>,
    pub limit: usize,
}

pub struct ChatStore {
    pool: SqlitePool,
    db_path: PathBuf,
}

/// The longest preview kept for the list.
const PREVIEW_CHARS: usize = 160;

impl ChatStore {
    pub async fn new(state_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(state_dir)
            .map_err(|e| ApiError::Internal(format!("failed to create chat store dir: {e}")))?;
        let db_path = state_dir.join("chats.db");
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to open chats db: {e}")))?;
        let store = Self { pool, db_path };
        store.init_schema().await?;
        Ok(store)
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    async fn init_schema(&self) -> Result<()> {
        for statement in [
            r#"CREATE TABLE IF NOT EXISTS chats (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                driver TEXT NOT NULL,
                model TEXT,
                creator TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                archived INTEGER NOT NULL DEFAULT 0,
                work_item TEXT,
                promoted_session TEXT,
                started INTEGER NOT NULL DEFAULT 0,
                message_count INTEGER NOT NULL DEFAULT 0,
                preview TEXT
            )"#,
            "CREATE INDEX IF NOT EXISTS idx_chats_updated ON chats(updated_at)",
            r#"CREATE TABLE IF NOT EXISTS chat_entries (
                chat_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                at TEXT NOT NULL,
                kind TEXT NOT NULL,
                text TEXT NOT NULL,
                tool_name TEXT,
                tool_use_id TEXT,
                is_error INTEGER NOT NULL DEFAULT 0,
                retryable INTEGER NOT NULL DEFAULT 0,
                by_actor TEXT,
                PRIMARY KEY (chat_id, seq)
            )"#,
            // Titles and entries in one index; `chat_id` is carried, not
            // searched, so a match names its chat.
            r#"CREATE VIRTUAL TABLE IF NOT EXISTS chat_fts USING fts5(
                chat_id UNINDEXED,
                text
            )"#,
        ] {
            sqlx::query(statement)
                .execute(&self.pool)
                .await
                .map_err(|e| ApiError::Internal(format!("chats db schema: {e}")))?;
        }
        Ok(())
    }

    pub async fn insert_chat(&self, record: &ChatRecord) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO chats (id, title, driver, model, creator, created_at, updated_at,
                archived, work_item, promoted_session, started, message_count, preview)
               VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(record.id.to_string())
        .bind(&record.title)
        .bind(&record.driver)
        .bind(&record.model)
        .bind(&record.creator)
        .bind(&record.created_at)
        .bind(&record.updated_at)
        .bind(record.archived)
        .bind(&record.work_item)
        .bind(record.promoted_session.map(|s| s.to_string()))
        .bind(record.started)
        .bind(record.message_count as i64)
        .bind(&record.preview)
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("insert chat: {e}")))?;
        self.index(record.id, &record.title).await
    }

    /// Write every mutable field of `record` back.
    pub async fn update_chat(&self, record: &ChatRecord) -> Result<()> {
        sqlx::query(
            r#"UPDATE chats SET title = ?, model = ?, updated_at = ?, archived = ?,
                work_item = ?, promoted_session = ?, started = ?, message_count = ?, preview = ?
               WHERE id = ?"#,
        )
        .bind(&record.title)
        .bind(&record.model)
        .bind(&record.updated_at)
        .bind(record.archived)
        .bind(&record.work_item)
        .bind(record.promoted_session.map(|s| s.to_string()))
        .bind(record.started)
        .bind(record.message_count as i64)
        .bind(&record.preview)
        .bind(record.id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("update chat: {e}")))?;
        Ok(())
    }

    pub async fn get_chat(&self, id: Uuid) -> Result<Option<ChatRecord>> {
        let row = sqlx::query("SELECT * FROM chats WHERE id = ?")
            .bind(id.to_string())
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("read chat: {e}")))?;
        row.map(|r| record_from_row(&r)).transpose()
    }

    pub async fn list_chats(&self, query: &ChatQuery) -> Result<Vec<ChatRecord>> {
        let limit = query.limit.clamp(1, 500) as i64;
        let archived = query.archived.map(i64::from);
        let rows = match query
            .q
            .as_deref()
            .and_then(crate::history::user_query_to_fts)
        {
            Some(fts) => {
                sqlx::query(
                    r#"SELECT c.* FROM chats c
                       WHERE c.id IN (SELECT chat_id FROM chat_fts WHERE chat_fts MATCH ?)
                         AND (? IS NULL OR c.archived = ?)
                       ORDER BY c.updated_at DESC LIMIT ?"#,
                )
                .bind(fts)
                .bind(archived)
                .bind(archived)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    r#"SELECT * FROM chats WHERE (? IS NULL OR archived = ?)
                       ORDER BY updated_at DESC LIMIT ?"#,
                )
                .bind(archived)
                .bind(archived)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
        }
        .map_err(|e| ApiError::Internal(format!("list chats: {e}")))?;
        rows.iter().map(record_from_row).collect()
    }

    /// Append one entry, numbered after the chat's last, and index its text.
    /// Returns it with its `seq`.
    pub async fn append_entry(&self, chat: Uuid, mut entry: ChatEntry) -> Result<ChatEntry> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ApiError::Internal(format!("append entry: {e}")))?;
        let next: i64 = sqlx::query(
            "SELECT COALESCE(MAX(seq), 0) + 1 AS next FROM chat_entries WHERE chat_id = ?",
        )
        .bind(chat.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("append entry: {e}")))?
        .get("next");
        entry.seq = next;
        sqlx::query(
            r#"INSERT INTO chat_entries (chat_id, seq, at, kind, text, tool_name, tool_use_id,
                is_error, retryable, by_actor) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
        )
        .bind(chat.to_string())
        .bind(entry.seq)
        .bind(&entry.at)
        .bind(&entry.kind)
        .bind(&entry.text)
        .bind(&entry.tool_name)
        .bind(&entry.tool_use_id)
        .bind(entry.is_error)
        .bind(entry.retryable)
        .bind(&entry.by)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("append entry: {e}")))?;
        // What people and the agent said is searchable; tool inputs and
        // results are not, so a file the agent read is not one search away.
        if matches!(entry.kind.as_str(), "user" | "assistant") && !entry.text.trim().is_empty() {
            sqlx::query("INSERT INTO chat_fts (chat_id, text) VALUES (?, ?)")
                .bind(chat.to_string())
                .bind(&entry.text)
                .execute(&mut *tx)
                .await
                .map_err(|e| ApiError::Internal(format!("index entry: {e}")))?;
        }
        tx.commit()
            .await
            .map_err(|e| ApiError::Internal(format!("append entry: {e}")))?;
        Ok(entry)
    }

    /// The chat's entries after `after_seq`, oldest first, at most `limit`.
    pub async fn entries(
        &self,
        chat: Uuid,
        after_seq: i64,
        limit: usize,
    ) -> Result<Vec<ChatEntry>> {
        let rows = sqlx::query(
            r#"SELECT * FROM chat_entries WHERE chat_id = ? AND seq > ?
               ORDER BY seq ASC LIMIT ?"#,
        )
        .bind(chat.to_string())
        .bind(after_seq)
        .bind(limit.clamp(1, 10_000) as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ApiError::Internal(format!("read entries: {e}")))?;
        Ok(rows
            .iter()
            .map(|r| ChatEntry {
                seq: r.get("seq"),
                at: r.get("at"),
                kind: r.get("kind"),
                text: r.get("text"),
                tool_name: r.get("tool_name"),
                tool_use_id: r.get("tool_use_id"),
                is_error: r.get::<i64, _>("is_error") != 0,
                retryable: r.get::<i64, _>("retryable") != 0,
                by: r.get("by_actor"),
            })
            .collect())
    }

    /// The newest `limit` entries, oldest first.
    pub async fn tail(&self, chat: Uuid, limit: usize) -> Result<Vec<ChatEntry>> {
        let last: i64 =
            sqlx::query("SELECT COALESCE(MAX(seq), 0) AS last FROM chat_entries WHERE chat_id = ?")
                .bind(chat.to_string())
                .fetch_one(&self.pool)
                .await
                .map_err(|e| ApiError::Internal(format!("read entries: {e}")))?
                .get("last");
        self.entries(chat, (last - limit as i64).max(0), limit)
            .await
    }

    async fn index(&self, chat: Uuid, text: &str) -> Result<()> {
        sqlx::query("INSERT INTO chat_fts (chat_id, text) VALUES (?, ?)")
            .bind(chat.to_string())
            .bind(text)
            .execute(&self.pool)
            .await
            .map_err(|e| ApiError::Internal(format!("index chat: {e}")))?;
        Ok(())
    }

    /// Index a new title, so a renamed chat is found by it.
    pub async fn index_title(&self, chat: Uuid, title: &str) -> Result<()> {
        self.index(chat, title).await
    }
}

/// A preview line: the first `PREVIEW_CHARS` characters on one line.
pub fn preview(text: &str) -> Option<String> {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return None;
    }
    Some(match flat.char_indices().nth(PREVIEW_CHARS) {
        Some((cut, _)) => format!("{}…", &flat[..cut]),
        None => flat,
    })
}

fn record_from_row(r: &sqlx::sqlite::SqliteRow) -> Result<ChatRecord> {
    let id: String = r.get("id");
    let promoted: Option<String> = r.get("promoted_session");
    Ok(ChatRecord {
        id: Uuid::parse_str(&id).map_err(|e| ApiError::Internal(format!("chat id {id}: {e}")))?,
        title: r.get("title"),
        driver: r.get("driver"),
        model: r.get("model"),
        creator: r.get("creator"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        archived: r.get::<i64, _>("archived") != 0,
        work_item: r.get("work_item"),
        promoted_session: promoted.and_then(|s| Uuid::parse_str(&s).ok()),
        started: r.get::<i64, _>("started") != 0,
        message_count: r.get::<i64, _>("message_count").max(0) as u64,
        preview: r.get("preview"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: Uuid, title: &str, at: &str) -> ChatRecord {
        ChatRecord {
            id,
            title: title.into(),
            driver: "klaudia".into(),
            model: None,
            creator: "human:ada".into(),
            created_at: at.into(),
            updated_at: at.into(),
            archived: false,
            work_item: None,
            promoted_session: None,
            started: false,
            message_count: 0,
            preview: None,
        }
    }

    fn entry(kind: &str, text: &str) -> ChatEntry {
        ChatEntry {
            seq: 0,
            at: "2026-10-08T00:00:00Z".into(),
            kind: kind.into(),
            text: text.into(),
            tool_name: None,
            tool_use_id: None,
            is_error: false,
            retryable: false,
            by: None,
        }
    }

    #[tokio::test]
    async fn chats_and_entries_survive_a_reopen_and_are_found_by_what_was_said() {
        let dir = tempfile::tempdir().unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        {
            let store = ChatStore::new(dir.path()).await.unwrap();
            store
                .insert_chat(&record(a, "AWS in Sydney", "2026-10-01T00:00:00Z"))
                .await
                .unwrap();
            store
                .insert_chat(&record(b, "Unrelated", "2026-10-02T00:00:00Z"))
                .await
                .unwrap();
            let first = store
                .append_entry(a, entry("user", "is bedrock in ap-southeast-2?"))
                .await
                .unwrap();
            let second = store
                .append_entry(a, entry("assistant", "Yes, it is."))
                .await
                .unwrap();
            assert_eq!((first.seq, second.seq), (1, 2));
        }
        let store = ChatStore::new(dir.path()).await.unwrap();
        let all = store
            .list_chats(&ChatQuery {
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(
            all.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![b, a],
            "newest first"
        );
        let found = store
            .list_chats(&ChatQuery {
                q: Some("bedrock".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(found.iter().map(|c| c.id).collect::<Vec<_>>(), vec![a]);
        let by_title = store
            .list_chats(&ChatQuery {
                q: Some("sydney".into()),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(by_title.len(), 1);
        let entries = store.entries(a, 0, 100).await.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(store.tail(a, 1).await.unwrap()[0].text, "Yes, it is.");
    }

    #[tokio::test]
    async fn archiving_hides_a_chat_from_the_unarchived_list_and_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = ChatStore::new(dir.path()).await.unwrap();
        let id = Uuid::new_v4();
        let mut chat = record(id, "Old question", "2026-10-01T00:00:00Z");
        store.insert_chat(&chat).await.unwrap();
        chat.archived = true;
        store.update_chat(&chat).await.unwrap();
        let open = store
            .list_chats(&ChatQuery {
                archived: Some(false),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(open.is_empty());
        let archived = store
            .list_chats(&ChatQuery {
                archived: Some(true),
                limit: 10,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(archived.len(), 1);
        assert!(store.get_chat(id).await.unwrap().unwrap().archived);
    }

    #[test]
    fn a_preview_is_one_short_line() {
        assert_eq!(preview("  a\n b  ").as_deref(), Some("a b"));
        assert_eq!(preview("   "), None);
        let long = "x".repeat(400);
        assert_eq!(preview(&long).unwrap().chars().count(), PREVIEW_CHARS + 1);
    }
}
