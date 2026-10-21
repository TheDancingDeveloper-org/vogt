//! Wire types for the parts of the Vogt API the GUI reads.
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

/// Zero-based cursor position on a [`SessionScreen`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenCursor {
    /// Row from the top.
    pub row: u16,
    /// Column from the left.
    pub col: u16,
}

/// The reply to `GET /api/sessions/{id}/screen`: the rendered screen as text.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionScreen {
    /// Session id.
    pub id: String,
    /// Width in columns.
    pub cols: u16,
    /// Height in rows.
    pub rows: u16,
    /// The visible rows, top to bottom, trailing spaces trimmed.
    pub lines: Vec<String>,
    /// Where the cursor is.
    pub cursor: ScreenCursor,
    /// The window title the program set, if any.
    pub title: Option<String>,
    /// The activity state, as on [`Session::activity`].
    pub activity: String,
    /// False once the program has exited.
    pub alive: bool,
}

/// A text frame on the `/api/sessions/{id}/attach` WebSocket. Binary frames
/// between them are raw PTY output. Kinds this client does not use decode to
/// [`AttachControl::Other`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AttachControl {
    /// A snapshot follows as binary frames, then [`AttachControl::SnapshotDone`].
    SnapshotStart {
        /// The absolute output position at the end of the snapshot.
        #[serde(default)]
        scrollback_pos: u64,
        /// Discard the screen first; false means the payload is a resume delta.
        #[serde(default = "yes")]
        reset: bool,
        /// The PTY size the payload is drawn at.
        #[serde(default)]
        cols: Option<u16>,
        /// The PTY size the payload is drawn at.
        #[serde(default)]
        rows: Option<u16>,
    },
    /// The snapshot is complete; live output follows.
    SnapshotDone,
    /// The PTY was resized by some client.
    Resize {
        /// Columns.
        cols: u16,
        /// Rows.
        rows: u16,
    },
    /// The answer to a ping.
    Pong {
        /// The ping's id.
        #[serde(default)]
        id: u64,
        /// The server's output position.
        #[serde(default)]
        pos: u64,
    },
    /// This client fell too far behind; the server closes and a fresh attach
    /// is needed.
    Lag,
    /// The session is hibernated: the snapshot was its last screen and
    /// nothing live follows.
    Hibernated,
    /// Any other frame (`input-refused` and future kinds).
    #[serde(other)]
    Other,
}

fn yes() -> bool {
    true
}

/// The WebSocket URL for attaching to session `id` on `base`.
pub fn attach_url(base: &str, id: &str) -> String {
    let base = normalise_base(base);
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base
    };
    format!("{ws}/api/sessions/{}/attach", encode_query(id))
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
    fn attach_frames_decode() {
        let start: AttachControl = serde_json::from_str(
            r#"{"type":"snapshot-start","session_id":"x","scrollback_bytes":3,"scrollback_pos":9,"reset":false,"cols":80,"rows":24}"#,
        )
        .unwrap();
        assert_eq!(
            start,
            AttachControl::SnapshotStart {
                scrollback_pos: 9,
                reset: false,
                cols: Some(80),
                rows: Some(24)
            }
        );
        let old: AttachControl = serde_json::from_str(r#"{"type":"snapshot-start"}"#).unwrap();
        assert!(matches!(old, AttachControl::SnapshotStart { reset: true, .. }));
        let lag: AttachControl = serde_json::from_str(r#"{"type":"lag","note":"slow"}"#).unwrap();
        assert_eq!(lag, AttachControl::Lag);
        let other: AttachControl = serde_json::from_str(r#"{"type":"input-refused","reason":"r"}"#).unwrap();
        assert_eq!(other, AttachControl::Other);
    }

    #[test]
    fn attach_url_switches_scheme() {
        assert_eq!(
            attach_url("https://v.example/", "a-1"),
            "wss://v.example/api/sessions/a-1/attach"
        );
        assert_eq!(
            attach_url("http://127.0.0.1:8910", "b"),
            "ws://127.0.0.1:8910/api/sessions/b/attach"
        );
    }

    #[test]
    fn query_encoding_and_base() {
        assert_eq!(encode_query("WI-7"), "WI-7");
        assert_eq!(encode_query("a b&c"), "a%20b%26c");
        assert_eq!(normalise_base(" https://x/ "), "https://x");
    }
}
