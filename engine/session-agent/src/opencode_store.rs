//! opencode's own store, read for its assistant replies (WI-931, WI-4291).
//!
//! opencode keeps every session in one SQLite database,
//! `$XDG_DATA_HOME/opencode/opencode.db`, which also holds its account and
//! provider credentials. On a host the engine reads it; in a pod the database
//! never leaves the pod, so the session agent reads the session's assistant
//! text out of it here and ships only that, as a [`RepliesUpload`], to the
//! engine's [`REPLIES_ROUTE`]. Nothing else in the store is read: no tool
//! inputs or outputs, no user prompts, no account rows.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
    ConnectOptions, Connection, Row, SqliteConnection,
};

/// Where a pod's session agent posts its opencode replies. It sits beside the
/// other pod uploads and takes the pod's stream credential.
pub const REPLIES_ROUTE: &str = "/api/agent-auth/store/bundle/opencode-replies";
/// Most replies one read returns, and one upload carries.
pub const MAX_REPLIES: usize = 20;
/// One reply is cut here, as the core cuts a transcript reply.
pub const MAX_REPLY_CHARS: usize = 20_000;
/// How many recent messages are scanned for assistant text.
const SCAN_MESSAGES: i64 = 200;

/// Where opencode keeps its database for a user whose home is `home`.
pub fn db_path(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("opencode")
        .join("opencode.db")
}

/// The store, opened read-only, when it exists.
pub async fn open(db: &Path) -> Option<SqliteConnection> {
    if !db.is_file() {
        return None;
    }
    SqliteConnectOptions::new()
        .filename(db)
        .read_only(true)
        // Never change the journal mode of somebody else's database.
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(2))
        .disable_statement_logging()
        .connect()
        .await
        .ok()
}

/// One assistant message's text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub text: String,
    /// When opencode recorded the message (RFC 3339).
    #[serde(default)]
    pub at: Option<String>,
}

/// What a pod sends the engine: its opencode conversation and that
/// conversation's last assistant replies, oldest first. These two fields are
/// the whole message; the engine refuses any other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepliesUpload {
    pub conversation_id: String,
    pub replies: Vec<Reply>,
}

/// The last `n` assistant replies of opencode session `id`, oldest first.
pub async fn last_replies(db: &Path, id: &str, n: usize) -> Option<Vec<Reply>> {
    let mut conn = open(db).await?;
    let messages = sqlx::query(
        "SELECT id, time_created, data FROM message WHERE session_id = ?1 \
         ORDER BY time_created DESC LIMIT ?2",
    )
    .bind(id)
    .bind(SCAN_MESSAGES)
    .fetch_all(&mut conn)
    .await
    .ok()?;
    let mut replies = Vec::new();
    for row in messages {
        if replies.len() >= n.min(MAX_REPLIES) {
            break;
        }
        let data: String = row.try_get(2).unwrap_or_default();
        let role = serde_json::from_str::<serde_json::Value>(&data)
            .ok()
            .and_then(|v| v.get("role").and_then(|r| r.as_str()).map(str::to_string));
        if role.as_deref() != Some("assistant") {
            continue;
        }
        let message_id: String = row.try_get(0).unwrap_or_default();
        let created: i64 = row.try_get(1).unwrap_or_default();
        let parts =
            sqlx::query("SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created")
                .bind(&message_id)
                .fetch_all(&mut conn)
                .await
                .unwrap_or_default();
        let text: String = parts
            .iter()
            .filter_map(|p| p.try_get::<String, _>(0).ok())
            .filter_map(|d| serde_json::from_str::<serde_json::Value>(&d).ok())
            .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|v| v.get("text").and_then(|t| t.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        replies.push(Reply {
            text: cut(text),
            at: rfc3339_millis(created),
        });
    }
    let _ = conn.close().await;
    replies.reverse();
    Some(replies)
}

/// `text` cut at [`MAX_REPLY_CHARS`].
pub fn cut(text: String) -> String {
    if text.chars().count() > MAX_REPLY_CHARS {
        text.chars().take(MAX_REPLY_CHARS).collect::<String>() + "…"
    } else {
        text
    }
}

fn rfc3339_millis(ms: i64) -> Option<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
}

/// The opencode session a pod runs. A pod runs one opencode TUI, so this is
/// the session whose first prompt names the engine session (`engine_id`, the
/// brief's file name) when there is one, else the newest top-level session.
pub async fn pod_conversation(db: &Path, engine_id: &str) -> Option<String> {
    let mut conn = open(db).await?;
    let mut found: Option<String> = None;
    if !engine_id.is_empty() {
        found = sqlx::query(
            "SELECT p.session_id FROM part p JOIN session s ON s.id = p.session_id \
             WHERE s.parent_id IS NULL AND p.data LIKE ?1 \
             ORDER BY s.time_created DESC LIMIT 1",
        )
        .bind(format!("%{engine_id}%"))
        .fetch_optional(&mut conn)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get::<String, _>(0).ok());
    }
    if found.is_none() {
        found = sqlx::query(
            "SELECT id FROM session WHERE parent_id IS NULL \
             ORDER BY time_created DESC LIMIT 1",
        )
        .fetch_optional(&mut conn)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get::<String, _>(0).ok());
    }
    let _ = conn.close().await;
    found
}

/// The upload a pod sends for engine session `engine_id`, or `None` while
/// opencode has no session or no store yet.
pub async fn replies_upload(db: &Path, engine_id: &str) -> Option<RepliesUpload> {
    let conversation_id = pod_conversation(db, engine_id).await?;
    let replies = last_replies(db, &conversation_id, MAX_REPLIES).await?;
    Some(RepliesUpload {
        conversation_id,
        replies,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A store with opencode's tables, plus the account table that holds its
    /// credentials, so a test can show those never leave.
    pub(crate) async fn store(dir: &Path) -> PathBuf {
        let db = dir.join("opencode.db");
        let mut conn = SqliteConnectOptions::new()
            .filename(&db)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .connect()
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT NOT NULL, \
             parent_id TEXT, time_created INTEGER NOT NULL)",
            "CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT NOT NULL, \
             time_created INTEGER NOT NULL, data TEXT NOT NULL)",
            "CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT NOT NULL, \
             session_id TEXT NOT NULL, time_created INTEGER NOT NULL, data TEXT NOT NULL)",
            "CREATE TABLE account (id TEXT PRIMARY KEY, data TEXT NOT NULL)",
        ] {
            sqlx::query(sql).execute(&mut conn).await.unwrap();
        }
        db
    }

    pub(crate) async fn exec(db: &Path, sql: &'static str, binds: &[&str]) {
        let mut conn = SqliteConnectOptions::new()
            .filename(db)
            .connect()
            .await
            .unwrap();
        let mut q = sqlx::query(sql);
        for b in binds {
            q = q.bind(*b);
        }
        q.execute(&mut conn).await.unwrap();
    }

    pub(crate) const PROVIDER_KEY: &str = "sk-ant-oat01-PODSECRETPODSECRETPODSECRET";
    pub(crate) const TOOL_SECRET: &str = "GITHUB_TOKEN=ghp_TOOLPAYLOADTOOLPAYLOAD0123456789";
    pub(crate) const USER_PROMPT: &str = "user prompt that stays in the pod";

    /// A pod store: one top-level session started by engine session
    /// `engine_id`, a subagent child, a user prompt, a tool call carrying a
    /// secret in its input and output, an assistant reply, and an account
    /// row holding a provider key.
    pub(crate) async fn pod_store(dir: &Path, engine_id: &str, reply: &str) -> PathBuf {
        let db = store(dir).await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_pod', '/work', NULL, 1000)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_child', '/work', 'ses_pod', 5000)",
            &[],
        )
        .await;
        let account = format!(r#"{{"provider":"anthropic","key":"{PROVIDER_KEY}"}}"#);
        exec(&db, "INSERT INTO account VALUES ('acc', ?1)", &[&account]).await;
        for (mid, at, role) in [
            ("m1", "1000", "user"),
            ("m2", "2000", "assistant"),
            ("m3", "3000", "assistant"),
        ] {
            exec(
                &db,
                "INSERT INTO message VALUES (?1, 'ses_pod', CAST(?2 AS INTEGER), ?3)",
                &[mid, at, &format!(r#"{{"role":"{role}"}}"#)],
            )
            .await;
        }
        let brief = format!(
            r#"{{"type":"text","text":"Vogt started this session with a brief in /s/sessions/{engine_id}.md. {USER_PROMPT}"}}"#
        );
        let tool = serde_json::json!({
            "type": "tool",
            "tool": "bash",
            "state": {"input": {"command": format!("echo {TOOL_SECRET}")}, "output": TOOL_SECRET},
        })
        .to_string();
        let text = serde_json::json!({"type": "text", "text": reply}).to_string();
        for (pid, mid, data) in [
            ("p1", "m1", brief.as_str()),
            ("p2", "m2", tool.as_str()),
            ("p3", "m3", text.as_str()),
        ] {
            exec(
                &db,
                "INSERT INTO part VALUES (?1, ?2, 'ses_pod', 1, ?3)",
                &[pid, mid, data],
            )
            .await;
        }
        db
    }

    #[tokio::test]
    async fn last_replies_are_the_assistant_text_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path()).await;
        for (mid, at, role) in [
            ("m1", "1000", "user"),
            ("m2", "2000", "assistant"),
            ("m3", "3000", "assistant"),
            ("m4", "4000", "assistant"),
        ] {
            exec(
                &db,
                "INSERT INTO message VALUES (?1, 'ses_1', CAST(?2 AS INTEGER), ?3)",
                &[mid, at, &format!("{{\"role\":\"{role}\"}}")],
            )
            .await;
        }
        let parts: [(&str, &str, &str); 5] = [
            ("p1", "m1", r#"{"type":"text","text":"do the thing"}"#),
            ("p2", "m2", r#"{"type":"text","text":"first reply"}"#),
            ("p3", "m3", r#"{"type":"tool","tool":"bash"}"#),
            ("p4", "m4", r#"{"type":"text","text":"last reply"}"#),
            ("p5", "m4", r#"{"type":"text","text":"second paragraph"}"#),
        ];
        for (pid, mid, data) in parts {
            exec(
                &db,
                "INSERT INTO part VALUES (?1, ?2, 'ses_1', 1, ?3)",
                &[pid, mid, data],
            )
            .await;
        }
        let replies = last_replies(&db, "ses_1", 5).await.unwrap();
        let texts: Vec<&str> = replies.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, ["first reply", "last reply\nsecond paragraph"]);
        assert!(replies[1]
            .at
            .as_deref()
            .unwrap()
            .starts_with("1970-01-01T00:00:04"));
        assert_eq!(last_replies(&db, "ses_1", 1).await.unwrap().len(), 1);
        assert!(last_replies(&tmp.path().join("absent.db"), "x", 1)
            .await
            .is_none());
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(
            rfc3339_millis(1_763_262_000_123).as_deref(),
            Some("2025-11-16T03:00:00.123Z")
        );
        assert_eq!(rfc3339_millis(0).as_deref(), Some("1970-01-01T00:00:00Z"));
    }

    /// WI-4291: a pod's upload names its own opencode session and carries
    /// that session's assistant text, and nothing else from the store.
    #[tokio::test]
    async fn a_pod_upload_is_the_assistant_text_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let engine_id = "6f1c1a52-1111-4222-8333-944455556666";
        let db = pod_store(tmp.path(), engine_id, "PONG from the pod opencode").await;
        let upload = replies_upload(&db, engine_id).await.expect("an upload");
        assert_eq!(upload.conversation_id, "ses_pod");
        assert_eq!(
            upload
                .replies
                .iter()
                .map(|r| r.text.as_str())
                .collect::<Vec<_>>(),
            ["PONG from the pod opencode"]
        );
        let wire = serde_json::to_value(&upload).unwrap();
        let mut keys: Vec<&String> = wire.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["conversation_id", "replies"]);
        for reply in wire["replies"].as_array().unwrap() {
            let mut keys: Vec<&String> = reply.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, ["at", "text"]);
        }
        let text = wire.to_string();
        for leaked in [PROVIDER_KEY, TOOL_SECRET, "ghp_", USER_PROMPT, "\"tool\""] {
            assert!(!text.contains(leaked), "{leaked} crossed: {text}");
        }
    }

    /// The wire type is closed: a message with any other field, such as a
    /// credential, is not an upload.
    #[test]
    fn an_upload_with_another_field_is_refused() {
        for extra in [
            serde_json::json!({"conversation_id": "ses_pod", "replies": [], "credentials": "x"}),
            serde_json::json!({"conversation_id": "ses_pod", "replies": [{"text": "hi", "env": {"K": "v"}}]}),
        ] {
            assert!(serde_json::from_value::<RepliesUpload>(extra).is_err());
        }
    }

    #[tokio::test]
    async fn with_no_brief_the_pod_conversation_is_the_newest_top_level_session() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path()).await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_old', '/w', NULL, 1)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_new', '/w', NULL, 2)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_kid', '/w', 'ses_new', 3)",
            &[],
        )
        .await;
        assert_eq!(
            pod_conversation(&db, "no-such-engine").await.as_deref(),
            Some("ses_new")
        );
        assert_eq!(
            pod_conversation(&tmp.path().join("none.db"), "x").await,
            None
        );
    }
}
