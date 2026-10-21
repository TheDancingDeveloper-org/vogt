//! Wire types for the parts of the Vogt API this slice reads.
//!
//! Every struct tolerates fields it does not know and fills ones the server
//! leaves out, so an older or newer engine still decodes.

use serde::{Deserialize, Serialize};

/// Where the app talks to, and with which bearer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auth {
    /// The API base, e.g. `https://vogt-dev.example`, with no trailing slash.
    pub base: String,
    /// The session bearer. Held in memory only; never written to storage.
    pub token: String,
}

/// The reply to `POST /api/auth/login`.
#[derive(Clone, Debug, Deserialize)]
pub struct LoginResult {
    /// Who signed in.
    pub actor: Actor,
    /// The session bearer.
    pub secret: String,
}

/// A signed-in actor.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Actor {
    /// The actor id.
    pub id: String,
    /// The name to show.
    pub display_name: String,
}

/// One row of `GET /api/sessions`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// The engine session id.
    pub id: String,
    /// The session name.
    pub name: String,
    /// `running`, `idle`, `awaiting-approval`, `exited` and so on.
    pub activity: String,
    /// The exit code once the session ended.
    pub exit_code: Option<i64>,
    /// Working directory.
    pub cwd: String,
    /// When the session was created (RFC 3339).
    pub created_at: String,
    /// When the current activity state began (RFC 3339).
    pub activity_changed_at: Option<String>,
}

/// `GET /api/sessions` returns either this or a bare array (older engines).
#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum SessionListBody {
    Wrapped { sessions: Vec<Session> },
    Bare(Vec<Session>),
}

impl SessionListBody {
    pub(crate) fn into_sessions(self) -> Vec<Session> {
        match self {
            Self::Wrapped { sessions } | Self::Bare(sessions) => sessions,
        }
    }
}

/// A work item as `work.get` returns it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkItem {
    /// Internal id (`wrk_...`).
    pub id: String,
    /// The reference people use (`WI-7`).
    #[serde(rename = "ref")]
    pub reference: String,
    /// feature, bug, chore or question.
    pub kind: String,
    /// The title.
    pub title: String,
    /// Markdown body.
    pub body: String,
    /// Workflow state.
    pub state: String,
    /// p0..p4.
    pub priority: String,
    /// xs..xl.
    pub effort: Option<String>,
    /// Owning project.
    pub project_slug: Option<String>,
    /// Label names.
    pub labels: Vec<String>,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

/// A comment on a work item.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Comment {
    /// Comment id.
    pub id: String,
    /// Markdown body.
    pub body: String,
    /// When it was written (RFC 3339).
    pub created_at: String,
}

/// The reply to `GET /api/vogt/work/get?mode=full`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkDetail {
    /// The item.
    pub item: WorkItem,
    /// Its comments.
    pub comments: Vec<Comment>,
}

/// The part of `GET /api/status` this app reads.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct OperationalStatus {
    pub(crate) storage: StorageStatus,
}

/// Where the engine keeps its files.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct StorageStatus {
    /// The root every `repo` and `path` parameter is relative to.
    pub(crate) workspace_root: String,
}

/// One changed file in `GET /api/git/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitEntry {
    /// Path relative to the repository.
    pub path: String,
    /// The index column of `git status --porcelain`.
    pub index: String,
    /// The worktree column.
    pub worktree: String,
    /// `untracked`, `modified`, `staged`, `conflicted`, `renamed` or `deleted`.
    pub kind: String,
}

impl GitEntry {
    /// The change is in the index, so its diff is index against HEAD.
    pub fn is_staged(&self) -> bool {
        self.kind == "staged"
    }
}

/// The reply to `GET /api/git/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitStatus {
    /// The repository, relative to the workspace root.
    pub repo: String,
    /// False when the directory is not in a Git repository.
    #[serde(default = "yes")]
    pub is_repo: bool,
    /// The current branch.
    pub branch: String,
    /// Commits ahead of upstream.
    pub ahead: u32,
    /// Commits behind upstream.
    pub behind: u32,
    /// The changed files.
    pub entries: Vec<GitEntry>,
}

fn yes() -> bool {
    true
}

/// The reply to `GET /api/git/diff`: the file at HEAD and as it is now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GitDiff {
    /// Path relative to the repository.
    pub path: String,
    /// The text at HEAD (empty for a new file).
    pub head: String,
    /// The text now: the index for a staged diff, else the worktree.
    pub current: String,
}

/// The `repo` parameter for a session working in `cwd`: its path relative to
/// the engine's workspace root, or `None` when `cwd` is outside it.
pub fn repo_for_cwd(root: &str, cwd: &str) -> Option<String> {
    let root = root.trim_end_matches('/');
    let rest = cwd.strip_prefix(root)?;
    if !(rest.is_empty() || rest.starts_with('/')) {
        return None; // `/work2` is not under `/work`
    }
    Some(rest.trim_matches('/').to_owned())
}

/// One event on `GET /api/events`. Kinds this slice does not use decode to
/// [`ServerEvent::Other`] instead of failing.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerEvent {
    /// A session started.
    SessionCreated {
        /// Session id.
        id: String,
        /// Its name.
        #[serde(default)]
        name: String,
    },
    /// A session was renamed.
    SessionRenamed {
        /// Session id.
        id: String,
        /// The new name.
        name: String,
    },
    /// A session ended.
    SessionKilled {
        /// Session id.
        id: String,
        /// Its exit code, if it had one.
        #[serde(default)]
        exit_code: Option<i64>,
    },
    /// A session's activity changed.
    Activity {
        /// Session id.
        id: String,
        /// The new activity state.
        state: String,
        /// When it changed.
        #[serde(default)]
        activity_changed_at: Option<String>,
    },
    /// This connection fell behind and missed events.
    Lagged {
        /// How many it missed.
        #[serde(default)]
        skipped: u64,
    },
    /// Something changed in vogt-core; read it again to see what.
    VogtChanged {
        /// The core event kind, e.g. `work.updated`.
        #[serde(default)]
        kind: String,
        /// The entity kind, e.g. `work_item`.
        #[serde(default)]
        entity_kind: String,
        /// The entity id.
        #[serde(default)]
        entity_id: String,
    },
    /// Any other event.
    #[serde(other)]
    Other,
}

/// A failed API call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    /// The HTTP status, or `None` when the request never got a response.
    pub status: Option<u16>,
    /// What went wrong, for a person to read.
    pub message: String,
}

impl ApiError {
    /// True when the server refused the credential.
    pub fn is_unauthorized(&self) -> bool {
        self.status == Some(401)
    }

    /// An error that came back with an HTTP status. Prefers the server's own
    /// `{"error": ...}` message over the bare status.
    pub fn from_status(status: u16, body: &[u8]) -> Self {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ErrorField {
            Text(String),
            Object { message: String },
        }
        #[derive(Deserialize)]
        struct Body {
            error: ErrorField,
        }
        let message = match serde_json::from_slice::<Body>(body) {
            Ok(Body {
                error: ErrorField::Text(m) | ErrorField::Object { message: m },
            }) => m,
            Err(_) => format!("HTTP {status}"),
        };
        Self {
            status: Some(status),
            message,
        }
    }

    /// An error with no response at all (network, DNS, CORS).
    pub fn transport(message: impl Into<String>) -> Self {
        Self {
            status: None,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.status {
            Some(status) => write!(f, "{} ({status})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// Trim spaces and trailing slashes from a base URL.
pub fn normalise_base(base: &str) -> String {
    base.trim().trim_end_matches('/').to_owned()
}

/// Percent-encode a query value (RFC 3986 unreserved characters pass through).
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_decode_and_unknown_kinds_fall_through() {
        let created: ServerEvent = serde_json::from_str(r#"{"type":"session-created","id":"a","name":"n"}"#).unwrap();
        assert_eq!(
            created,
            ServerEvent::SessionCreated {
                id: "a".into(),
                name: "n".into()
            }
        );
        let activity: ServerEvent = serde_json::from_str(r#"{"type":"activity","id":"a","state":"idle"}"#).unwrap();
        assert!(matches!(activity, ServerEvent::Activity { state, .. } if state == "idle"));
        let other: ServerEvent = serde_json::from_str(r#"{"type":"session-resources","samples":[]}"#).unwrap();
        assert_eq!(other, ServerEvent::Other);
    }

    #[test]
    fn session_list_accepts_both_shapes() {
        let wrapped: SessionListBody =
            serde_json::from_str(r#"{"sessions":[{"id":"a","name":"x","activity":"idle"}],"hidden_count":0}"#).unwrap();
        assert_eq!(wrapped.into_sessions().len(), 1);
        let bare: SessionListBody = serde_json::from_str(r#"[{"id":"a"},{"id":"b"}]"#).unwrap();
        assert_eq!(bare.into_sessions().len(), 2);
    }

    #[test]
    fn error_message_prefers_server_text() {
        assert_eq!(
            ApiError::from_status(401, br#"{"error":"bad password"}"#).message,
            "bad password"
        );
        assert_eq!(
            ApiError::from_status(429, br#"{"error":{"message":"slow down"}}"#).message,
            "slow down"
        );
        assert_eq!(ApiError::from_status(502, b"<html>").message, "HTTP 502");
        assert!(ApiError::from_status(401, b"").is_unauthorized());
    }

    #[test]
    fn repo_is_cwd_relative_to_the_root() {
        assert_eq!(repo_for_cwd("/work", "/work/vogt").as_deref(), Some("vogt"));
        assert_eq!(repo_for_cwd("/work/", "/work/a/b/").as_deref(), Some("a/b"));
        assert_eq!(repo_for_cwd("/work", "/work").as_deref(), Some(""));
        assert_eq!(repo_for_cwd("/work", "/work2/x"), None);
        assert_eq!(repo_for_cwd("/work", "/home/x"), None);
    }

    #[test]
    fn git_status_decodes_and_defaults_to_a_repo() {
        let status: GitStatus = serde_json::from_str(
            r#"{"repo":"vogt","branch":"main","ahead":1,"behind":0,"entries":[{"path":"a.rs","index":"M","worktree":" ","kind":"staged"}]}"#,
        )
        .unwrap();
        assert!(status.is_repo);
        assert!(status.entries[0].is_staged());
        let plain: GitStatus = serde_json::from_str(r#"{"repo":"x","is_repo":false}"#).unwrap();
        assert!(!plain.is_repo && plain.entries.is_empty());
    }

    #[test]
    fn query_encoding_and_base() {
        assert_eq!(encode_query("WI-7"), "WI-7");
        assert_eq!(encode_query("a b&c"), "a%20b%26c");
        assert_eq!(normalise_base(" https://x/ "), "https://x");
    }
}
