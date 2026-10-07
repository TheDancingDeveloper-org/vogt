//! An opencode session's own conversation, read from opencode's store
//! (WI-930, WI-931).
//!
//! Claude Code takes the engine's id (`--session-id`), and Codex and Claude
//! Code write a JSONL transcript per conversation that the core reads. opencode
//! does neither: it mints its own id (`ses_…`) and keeps every session in one
//! SQLite database, `$XDG_DATA_HOME/opencode/opencode.db`. Without its id an
//! opencode session cannot be resumed, so it could not be hibernated or
//! survive a redeploy, and without a reader its replies were blank on the
//! oversight board.
//!
//! **Finding the id.** Every opencode session the engine starts with a brief
//! is told, in its first prompt, the path of that brief, and the path is named
//! for the engine session (`…/sessions/<engine-uuid>.md`). So the opencode
//! session whose first prompt names this engine session is this session's,
//! even when several start in the same directory in the same second (as
//! javascan's did on 2026-10-05). A session started with no brief falls back
//! to the newest top-level opencode session in its directory created since it
//! spawned that no other engine session has claimed. That is a guess, and the
//! log says so.
//!
//! **Reading.** The engine reads here, not the core: the same database also
//! holds opencode's own account and provider credentials, so it is not mounted
//! into the core the way transcript directories are. The engine opens it
//! read-only, reads one session's assistant text and nothing else, and the
//! core redacts it before it is shown.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode},
    ConnectOptions, Connection, Row, SqliteConnection,
};
use uuid::Uuid;

/// How long a fresh opencode session's id is looked for after spawn. The
/// first prompt is written within seconds; a TUI that never got that far has
/// nothing to resume.
pub const CAPTURE_FOR: Duration = Duration::from_secs(120);
/// How often the store is asked while looking.
pub const CAPTURE_EVERY: Duration = Duration::from_secs(1);
/// Most replies one read returns.
pub const MAX_REPLIES: usize = 20;
/// How many recent messages are scanned for assistant text.
const SCAN_MESSAGES: i64 = 200;
/// One reply is cut here, as the core cuts a transcript reply.
const MAX_REPLY_CHARS: usize = 20_000;

/// Where opencode keeps its database for a user whose home is `home`.
pub fn db_path(home: &Path) -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("opencode")
        .join("opencode.db")
}

async fn open(db: &Path) -> Option<SqliteConnection> {
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

/// How the id was found, for the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Basis {
    /// Its first prompt names the engine session: certain.
    Brief,
    /// The newest unclaimed session in the directory since spawn: a guess.
    Directory,
}

impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Basis::Brief => "brief",
            Basis::Directory => "directory",
        }
    }
}

/// The opencode session the engine session `engine_id` runs, when the store
/// has it yet. `since_ms` is the spawn time (epoch milliseconds); `claimed`
/// are opencode ids other engine sessions already hold.
pub async fn find_session(
    db: &Path,
    engine_id: Uuid,
    cwd: &str,
    since_ms: i64,
    claimed: &[String],
) -> Option<(String, Basis)> {
    let mut conn = open(db).await?;
    // A little slack: the TUI may have stamped the session a moment before
    // the engine finished recording its own spawn time.
    let since = since_ms - 5_000;
    let by_brief: Option<String> = sqlx::query(
        "SELECT p.session_id FROM part p JOIN session s ON s.id = p.session_id \
         WHERE s.time_created >= ?1 AND p.data LIKE ?2 \
         ORDER BY s.time_created LIMIT 1",
    )
    .bind(since)
    .bind(format!("%{engine_id}%"))
    .fetch_optional(&mut conn)
    .await
    .ok()
    .flatten()
    .and_then(|row| row.try_get::<String, _>(0).ok());
    if let Some(id) = by_brief {
        let _ = conn.close().await;
        return Some((id, Basis::Brief));
    }
    let rows = sqlx::query(
        "SELECT id FROM session WHERE directory = ?1 AND time_created >= ?2 \
         AND parent_id IS NULL ORDER BY time_created",
    )
    .bind(cwd)
    .bind(since)
    .fetch_all(&mut conn)
    .await
    .unwrap_or_default();
    let _ = conn.close().await;
    rows.into_iter()
        .filter_map(|row| row.try_get::<String, _>(0).ok())
        .find(|id| !claimed.contains(id))
        .map(|id| (id, Basis::Directory))
}

/// One assistant message's text.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// When opencode recorded the message (RFC 3339).
    pub at: Option<String>,
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
        let text = if text.chars().count() > MAX_REPLY_CHARS {
            text.chars().take(MAX_REPLY_CHARS).collect::<String>() + "…"
        } else {
            text
        };
        let at = time::OffsetDateTime::from_unix_timestamp_nanos(created as i128 * 1_000_000)
            .ok()
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            });
        replies.push(Reply { text, at });
    }
    let _ = conn.close().await;
    replies.reverse();
    Some(replies)
}

/// How long the list of models opencode can run is trusted.
const MODELS_FOR: Duration = Duration::from_secs(300);
/// How long `opencode models` may take before the check is skipped.
const MODELS_TIMEOUT: Duration = Duration::from_secs(15);

static MODELS: std::sync::Mutex<Option<(std::time::Instant, Vec<String>)>> =
    std::sync::Mutex::new(None);

/// The `provider/model` ids opencode can run here, from `opencode models`,
/// cached for five minutes. `None` when they cannot be listed, in which case
/// nothing is refused on their account.
pub fn known_models() -> Option<Vec<String>> {
    if let Some((at, models)) = MODELS.lock().ok()?.as_ref() {
        if at.elapsed() < MODELS_FOR {
            return Some(models.clone());
        }
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            std::process::Command::new("opencode")
                .arg("models")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output(),
        );
    });
    let output = rx.recv_timeout(MODELS_TIMEOUT).ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    let models: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| l.contains('/') && !l.contains(' '))
        .map(str::to_string)
        .collect();
    if models.is_empty() {
        return None;
    }
    *MODELS.lock().ok()? = Some((std::time::Instant::now(), models.clone()));
    Some(models)
}

/// Why opencode could not run `model`, given the models it can (WI-935): an
/// unknown model otherwise fails inside the TUI, after launch, with an
/// opaque error. The refusal names the near matches.
pub fn check_model(model: &str, known: &[String]) -> std::result::Result<(), String> {
    if known.iter().any(|m| m == model) {
        return Ok(());
    }
    let provider = model.split('/').next().unwrap_or_default();
    let tail = model
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut near: Vec<&str> = known
        .iter()
        .filter(|m| {
            m.starts_with(&format!("{provider}/"))
                || (!tail.is_empty() && m.to_ascii_lowercase().contains(&tail))
        })
        .map(String::as_str)
        .collect();
    near.truncate(12);
    Err(if near.is_empty() {
        format!(
            "opencode cannot run model {model:?}: it is not among the {} models its \
             configured providers offer (`opencode models` lists them)",
            known.len()
        )
    } else {
        format!(
            "opencode cannot run model {model:?}; models it can run that look close: {}",
            near.join(", ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store shaped like opencode 1.18's, with only the columns read here.
    async fn store(dir: &Path) -> PathBuf {
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
        ] {
            sqlx::query(sql).execute(&mut conn).await.unwrap();
        }
        db
    }

    async fn exec(db: &Path, sql: &'static str, binds: &[&str]) {
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

    #[tokio::test]
    async fn the_session_whose_first_prompt_names_the_engine_session_is_found() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path()).await;
        let mine = Uuid::new_v4();
        let other = Uuid::new_v4();
        // Three opencode sessions in one directory in the same second, the
        // javascan shape: only the brief tells them apart.
        for (sid, engine) in [("ses_a", other), ("ses_b", mine), ("ses_c", other)] {
            exec(
                &db,
                "INSERT INTO session VALUES (?1, '/w/javascan', NULL, 1000)",
                &[sid],
            )
            .await;
            let text = format!(
                "{{\"type\":\"text\",\"text\":\"Vogt started this session with a brief in /s/sessions/{engine}.md\"}}"
            );
            exec(
                &db,
                "INSERT INTO part VALUES (?1 || '-p', ?1 || '-m', ?1, 1001, ?2)",
                &[sid, &text],
            )
            .await;
        }
        let found = find_session(&db, mine, "/w/javascan", 1000, &[]).await;
        assert_eq!(found, Some(("ses_b".to_string(), Basis::Brief)));
    }

    #[tokio::test]
    async fn with_no_brief_the_newest_unclaimed_session_in_the_directory_is_a_guess() {
        let tmp = tempfile::tempdir().unwrap();
        let db = store(tmp.path()).await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_old', '/w/p', NULL, 1)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_x', '/w/p', NULL, 50000)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_y', '/w/p', NULL, 50001)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_child', '/w/p', 'ses_y', 50002)",
            &[],
        )
        .await;
        exec(
            &db,
            "INSERT INTO session VALUES ('ses_z', '/w/elsewhere', NULL, 50003)",
            &[],
        )
        .await;
        let found = find_session(&db, Uuid::new_v4(), "/w/p", 50000, &["ses_x".into()]).await;
        assert_eq!(found, Some(("ses_y".to_string(), Basis::Directory)));
        assert_eq!(
            find_session(&db, Uuid::new_v4(), "/w/none", 50000, &[]).await,
            None
        );
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
    fn an_unknown_opencode_model_is_refused_with_its_near_matches() {
        let known: Vec<String> = [
            "theclawbay/grok-4.7",
            "theclawbay/gpt-5.6",
            "openrouter/x/grok-4.7",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(check_model("theclawbay/grok-4.7", &known), Ok(()));
        let err = check_model("theclawbay/grok-4.8", &known).unwrap_err();
        assert!(
            err.contains("theclawbay/grok-4.7") && err.contains("theclawbay/gpt-5.6"),
            "{err}"
        );
        let err = check_model("nowhere/grok-4.7", &known).unwrap_err();
        assert!(err.contains("openrouter/x/grok-4.7"), "{err}");
        let err = check_model("nowhere/unheard-of", &known).unwrap_err();
        assert!(err.contains("not among the 3 models"), "{err}");
    }
}
