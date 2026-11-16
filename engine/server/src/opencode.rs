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
//!
//! **Pods.** A pod's opencode keeps its store inside the pod. Its session
//! agent reads the assistant text out of it with the same reader
//! (`vogt_session_agent::opencode_store`) and posts only that to
//! [`REPLIES_ROUTE`]; the engine redacts it and keeps it beside the session's
//! hibernation record, where `/replies` reads it (WI-4291).

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use sqlx::{Connection, Row};
use uuid::Uuid;

use vogt_session_agent::opencode_store::{cut, open};
pub use vogt_session_agent::opencode_store::{
    db_path, last_replies, RepliesUpload, Reply, MAX_REPLIES, REPLIES_ROUTE,
};

/// How long a fresh opencode session's id is looked for after spawn. The
/// first prompt is written within seconds; a TUI that never got that far has
/// nothing to resume.
pub const CAPTURE_FOR: Duration = Duration::from_secs(120);
/// How often the store is asked while looking.
pub const CAPTURE_EVERY: Duration = Duration::from_secs(1);

/// The XDG root the engine gives one host opencode session when its own data
/// home is not writable (WI-4284): `data`, `state` and `cache` live under it.
fn session_xdg_root(state_dir: &Path, id: Uuid) -> PathBuf {
    state_dir.join("opencode-xdg").join(id.to_string())
}

/// Whether opencode could create its data directory under `home` (or
/// `$XDG_DATA_HOME`) as this process.
fn data_home_writable(home: &Path) -> bool {
    let dir = db_path(home)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".vogt-write-probe-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// Create `dir` and every missing or existing level between `base` (exclusive)
/// and it as mode 0700. The data directory holds opencode's conversation store
/// and `auth.json`, so no level may be readable by another user; a directory
/// left by an older launch is tightened too.
fn private_dirs(base: &Path, dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let rel = dir.strip_prefix(base).unwrap_or(dir);
    std::fs::create_dir_all(base)?;
    let mut cur = base.to_path_buf();
    for part in rel.components() {
        cur.push(part);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&cur)
            .or_else(|e| {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(e)
                }
            })?;
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The environment that points a host opencode session at its own data,
/// state and cache directories, or nothing when the engine's own are
/// writable. A session that already has its directory (a wake) keeps it, so
/// the conversation it resumes is found where it was written. The engine
/// container's `HOME` is not writable on the estate, so without this opencode
/// died at launch with `EACCES: mkdir ~/.local/share/opencode`.
pub fn session_xdg_env(state_dir: &Path, id: Uuid, home: &Path) -> Vec<(String, String)> {
    let root = session_xdg_root(state_dir, id);
    if !root.join("data").is_dir() && data_home_writable(home) {
        return Vec::new();
    }
    let mut env = Vec::new();
    for (var, sub) in [
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("XDG_CACHE_HOME", "cache"),
    ] {
        let dir = root.join(sub);
        if private_dirs(state_dir, &dir).is_err() {
            return Vec::new();
        }
        env.push((var.to_string(), dir.display().to_string()));
    }
    env
}

/// Where the store of engine session `id` is: its own directory when the
/// engine gave it one, else the engine's [`db_path`].
pub fn db_path_for(state_dir: &Path, id: Uuid, home: &Path) -> PathBuf {
    let own = session_xdg_root(state_dir, id).join("data");
    if own.is_dir() {
        return own.join("opencode").join("opencode.db");
    }
    db_path(home)
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

/// The most a pod's reply upload may be: [`MAX_REPLIES`] replies of
/// [`vogt_session_agent::opencode_store::MAX_REPLY_CHARS`] characters, with
/// room for JSON escaping.
const MAX_UPLOAD_BYTES: usize = 2 * 1024 * 1024;
/// Uploads one pod may make a minute. Its agent posts when the store changes,
/// at most about once a second.
const UPLOADS_PER_MINUTE: u32 = 120;

/// The replies a pod's agent sent, made fit to keep: the last
/// [`MAX_REPLIES`], each cut and redacted. `None` when the conversation id
/// is not one.
pub fn sanitize_upload(upload: RepliesUpload) -> Option<RepliesUpload> {
    if !crate::agent_cli::is_conversation_id(&upload.conversation_id) {
        return None;
    }
    let skip = upload.replies.len().saturating_sub(MAX_REPLIES);
    let replies = upload
        .replies
        .into_iter()
        .skip(skip)
        .map(|reply| Reply {
            text: crate::redaction::redact(&cut(reply.text)),
            at: reply.at.filter(|at| at.len() <= 64),
        })
        .collect();
    Some(RepliesUpload {
        conversation_id: upload.conversation_id,
        replies,
    })
}

/// Keep a pod session's replies, replacing the last ones.
pub fn store_pod_replies(
    state_dir: &Path,
    id: Uuid,
    upload: &RepliesUpload,
) -> std::io::Result<()> {
    let json = serde_json::to_vec(upload).map_err(std::io::Error::other)?;
    crate::hibernation::write_replies(state_dir, id, &json)
}

/// A pod session's kept replies, when its agent has sent any.
pub fn pod_replies(state_dir: &Path, id: Uuid) -> Option<RepliesUpload> {
    let bytes = crate::hibernation::read_replies(state_dir, id)?;
    serde_json::from_slice(&bytes).ok()
}

/// `POST /api/agent-auth/store/bundle/opencode-replies` — a pod opencode's
/// last assistant replies, from its session agent (WI-4291). Only a pod's
/// stream credential opens it, and it names the session. The body is a
/// [`RepliesUpload`] and nothing else: a message with any other field is
/// refused, so no credential, environment or tool payload can ride along.
/// The text is redacted before it is kept, and the conversation is recorded
/// on the session so `session.last_reply` asks for it.
pub async fn upload_pod_replies(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::app::AppState>>,
    request: axum::extract::Request<axum::body::Body>,
) -> axum::response::Response {
    use axum::{http::header, response::IntoResponse};
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    // A pod's stream credential only: a host opencode's store is read in
    // place, so a host session has nothing to send here.
    let session = match presented {
        Some(token) => state.agent_auth.session_for_token(token.as_bytes()).await,
        None => None,
    };
    let Some(id) = session.filter(|id| state.sessions.get(*id).is_ok()) else {
        return crate::error::ApiError::Unauthorized.into_response();
    };
    if let Some(retry_after) = state
        .auth
        .check_mutation_rate_limit(&format!("opencode-replies:{id}"), UPLOADS_PER_MINUTE)
    {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            [(
                header::RETRY_AFTER,
                retry_after.as_secs().max(1).to_string(),
            )],
            axum::Json(serde_json::json!({"error": "opencode replies rate limit exceeded"})),
        )
            .into_response();
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_UPLOAD_BYTES).await else {
        return crate::error::ApiError::BadRequest("opencode replies are too large".into())
            .into_response();
    };
    let Some(upload) = serde_json::from_slice::<RepliesUpload>(&body)
        .ok()
        .and_then(sanitize_upload)
    else {
        return crate::error::ApiError::BadRequest("invalid opencode replies".into())
            .into_response();
    };
    if let Err(e) = store_pod_replies(state.sessions.state_dir(), id, &upload) {
        tracing::warn!(session = %id, error = %e, "could not keep the pod's opencode replies");
        return crate::error::ApiError::Internal("could not keep the replies".into())
            .into_response();
    }
    let _ = state.sessions.report_conversation(
        id,
        vogt_engine_contract::ConversationReport {
            agent: "opencode".into(),
            id: upload.conversation_id.clone(),
            ended: false,
        },
    );
    tracing::info!(target: "vogt::audit", session_id = %id, replies = upload.replies.len(),
        "pod opencode replies kept");
    axum::Json(serde_json::json!({"ok": true, "replies": upload.replies.len()})).into_response()
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
    use sqlx::{
        sqlite::{SqliteConnectOptions, SqliteJournalMode},
        ConnectOptions,
    };

    #[test]
    fn an_unwritable_home_gives_the_session_its_own_xdg_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o500)).unwrap();
        let id = Uuid::new_v4();
        // (Skipped as root, which a read-only directory does not stop, and
        // when the environment already names a data home.)
        if std::env::var_os("XDG_DATA_HOME").is_some()
            || std::fs::create_dir(home.join("probe")).is_ok()
        {
            return;
        }
        let env = session_xdg_env(&state, id, &home);
        let data = env.iter().find(|(k, _)| k == "XDG_DATA_HOME").unwrap();
        assert!(Path::new(&data.1).is_dir());
        assert!(data.1.starts_with(state.to_str().unwrap()));
        assert!(env.iter().any(|(k, _)| k == "XDG_STATE_HOME"));
        assert!(env.iter().any(|(k, _)| k == "XDG_CACHE_HOME"));
        // The reader looks where the session wrote, and a wake keeps it.
        assert_eq!(
            db_path_for(&state, id, &home),
            Path::new(&data.1).join("opencode").join("opencode.db")
        );
        assert_eq!(session_xdg_env(&state, id, &home), env);
    }

    #[test]
    fn the_session_xdg_dirs_are_private_and_an_old_loose_dir_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let id = Uuid::new_v4();
        let root = session_xdg_root(&state, id);
        // A directory an earlier launch left with default permissions.
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(root.join("data"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        // A read-only home forces the per-session dirs; the data dir exists, so
        // a wake reuses it even when the home is writable.
        let env = session_xdg_env(&state, id, tmp.path());
        assert_eq!(env.len(), 3);
        for dir in [
            root.parent().unwrap().to_path_buf(),
            root.clone(),
            root.join("data"),
            root.join("state"),
            root.join("cache"),
        ] {
            assert_eq!(mode(&dir), 0o700, "{}", dir.display());
        }
    }

    #[test]
    fn a_writable_home_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        assert!(session_xdg_env(&tmp.path().join("state"), id, tmp.path()).is_empty());
        assert_eq!(
            db_path_for(&tmp.path().join("state"), id, tmp.path()),
            db_path(tmp.path())
        );
    }

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
