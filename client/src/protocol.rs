//! Wire types mirroring the MyDevEnv2 server API.
//!
//! These are the *client* side of the contract defined in the server crate
//! (`server/src/{pty,files,git,events,activity}.rs`). Field names and serde
//! representations must match byte-for-byte, so the relevant server definition
//! is cited above each type. Only fields the client consumes are modelled;
//! `#[serde(default)]` covers anything the server may add later.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Activity (server/src/activity.rs) ────────────────────────────────────────

/// Per-session activity, `rename_all = "kebab-case"` on the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityState {
    Idle,
    Running,
    WaitingForInput,
    Errored,
}

impl ActivityState {
    /// Short glyph for tab-strip activity badges.
    pub fn badge(self) -> &'static str {
        match self {
            ActivityState::Idle => "○",
            ActivityState::Running => "●",
            ActivityState::WaitingForInput => "⏵",
            ActivityState::Errored => "✗",
        }
    }
}

// ── Sessions (server/src/pty.rs) ─────────────────────────────────────────────

/// Body for `POST /api/sessions`. `name` is required; everything else defaults
/// server-side (default shell, cwd, 80x24).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<(String, String)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cols: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<u16>,
}

/// Returned by `GET/POST /api/sessions` and inside `GET /api/sessions/{id}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: Uuid,
    pub name: String,
    pub activity: ActivityState,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub scrollback_bytes: u64,
    #[serde(default)]
    pub cwd: String,
    /// RFC3339 on the wire; kept as a string — the client only displays it.
    #[serde(default)]
    pub created_at: String,
}

// ── SSE events (server/src/events.rs) ────────────────────────────────────────

/// `GET /api/events` server-sent events. `tag = "type"`, kebab-case.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerEvent {
    SessionCreated {
        id: Uuid,
        name: String,
    },
    SessionRenamed {
        id: Uuid,
        name: String,
    },
    SessionKilled {
        id: Uuid,
        #[serde(default)]
        exit_code: Option<i32>,
    },
    Activity {
        id: Uuid,
        state: ActivityState,
    },
}

// ── Files (server/src/files.rs) ──────────────────────────────────────────────

/// One entry from `GET /api/dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
}

/// One node from `GET /api/tree` (recursive; `children` is None for files).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub children: Option<Vec<TreeNode>>,
}

/// `GET /api/files?path=...`. Text files populate `content`; binary files
/// populate `content_base64` and set `is_binary`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRead {
    pub path: String,
    pub size: u64,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub content_base64: Option<String>,
    #[serde(default)]
    pub is_binary: bool,
}

/// Body for `PUT /api/files`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WriteReq {
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    /// Base64-encoded raw bytes; lets the native client upload binary files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(default)]
    pub create_parents: bool,
}

/// One hit from `GET /api/search`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u64,
    pub text: String,
}

// ── Git (server/src/git.rs) ──────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StatusKind {
    Untracked,
    Modified,
    Staged,
    Conflicted,
    Renamed,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusEntry {
    pub path: String,
    pub index: String,
    pub worktree: String,
    pub kind: StatusKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitStatus {
    pub repo: String,
    #[serde(default = "default_true")]
    pub is_repo: bool,
    pub branch: String,
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub behind: u32,
    #[serde(default)]
    pub entries: Vec<StatusEntry>,
}

fn default_true() -> bool {
    true
}

/// `GET /api/git/diff` — HEAD vs working-tree (or staged) content of one file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffResp {
    pub path: String,
    pub current: String,
    pub head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchInfo {
    pub current: String,
    pub all: Vec<String>,
}

// ── WebSocket attach control frames (server/src/ws.rs) ───────────────────────

/// Client → server text control frames. Serialized as `{"type":"...",...}`,
/// `tag = "type"`, kebab-case — matching the server's `ClientControl` enum.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientControl {
    /// MUST be the first frame after upgrade (server drops the socket after 5s
    /// otherwise). Token is the API bearer.
    Auth {
        token: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Ping,
}

impl ClientControl {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("ClientControl serializes")
    }
}

/// Server → client text control frames during attach. Binary frames are raw PTY
/// output and are handled separately. The snapshot replay protocol is:
/// `snapshot-start` → N binary scrollback chunks → `snapshot-done` → live binary.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerControl {
    SnapshotStart {
        #[serde(default)]
        session_id: Option<Uuid>,
        #[serde(default)]
        scrollback_bytes: u64,
        #[serde(default)]
        scrollback_pos: u64,
    },
    SnapshotDone,
    /// Server fell behind; it will close the socket — the client should reattach.
    Lag {
        #[serde(default)]
        note: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_kebab_roundtrips() {
        let j = serde_json::to_string(&ActivityState::WaitingForInput).unwrap();
        assert_eq!(j, "\"waiting-for-input\"");
        let back: ActivityState = serde_json::from_str(&j).unwrap();
        assert_eq!(back, ActivityState::WaitingForInput);
    }

    #[test]
    fn auth_control_frame_shape() {
        let f = ClientControl::Auth {
            token: "secret".into(),
        };
        assert_eq!(f.to_json(), r#"{"type":"auth","token":"secret"}"#);
    }

    #[test]
    fn resize_control_frame_shape() {
        let f = ClientControl::Resize {
            cols: 120,
            rows: 40,
        };
        assert_eq!(f.to_json(), r#"{"type":"resize","cols":120,"rows":40}"#);
    }

    #[test]
    fn parses_server_snapshot_start() {
        let raw = r#"{"type":"snapshot-start","session_id":"00000000-0000-0000-0000-000000000000","scrollback_bytes":10,"scrollback_pos":42}"#;
        match serde_json::from_str::<ServerControl>(raw).unwrap() {
            ServerControl::SnapshotStart { scrollback_pos, .. } => assert_eq!(scrollback_pos, 42),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn parses_server_event_activity() {
        let raw =
            r#"{"type":"activity","id":"00000000-0000-0000-0000-000000000000","state":"running"}"#;
        match serde_json::from_str::<ServerEvent>(raw).unwrap() {
            ServerEvent::Activity { state, .. } => assert_eq!(state, ActivityState::Running),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn session_spec_omits_none_fields() {
        let spec = SessionSpec {
            name: "term".into(),
            ..Default::default()
        };
        assert_eq!(serde_json::to_string(&spec).unwrap(), r#"{"name":"term"}"#);
    }
}
