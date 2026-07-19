use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivityState {
    Idle,
    Running,
    WaitingForInput,
    Errored,
}

impl ActivityState {
    pub fn badge(self) -> &'static str {
        match self {
            ActivityState::Idle => "○",
            ActivityState::Running => "●",
            ActivityState::WaitingForInput => "⏵",
            ActivityState::Errored => "✗",
        }
    }
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrollback_bytes: Option<usize>,
}

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
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub summary: SessionSummary,
    #[serde(default)]
    pub scrollback_pos: u64,
    #[serde(default)]
    pub scrollback_base64: String,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    #[serde(default)]
    pub children: Option<Vec<TreeNode>>,
}

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WriteReq {
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_base64: Option<String>,
    #[serde(default)]
    pub create_parents: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub path: String,
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSearchResult {
    pub path: String,
    pub name: String,
}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ClientControl {
    Auth {
        token: String,
        /// Absolute PTY output position already rendered by the client.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_from: Option<u64>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerControl {
    SnapshotStart {
        #[serde(default)]
        session_id: Option<Uuid>,
        #[serde(default)]
        scrollback_bytes: u64,
        #[serde(default)]
        scrollback_pos: u64,
        /// Whether the client must discard its current terminal buffer before
        /// applying this snapshot. False means the payload is a resume delta.
        #[serde(default = "default_true")]
        reset: bool,
    },
    SnapshotDone,
    Lag {
        #[serde(default)]
        note: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkResponse {
    pub ok: bool,
}

impl OkResponse {
    pub const fn new(ok: bool) -> Self {
        Self { ok }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteFileResponse {
    pub ok: bool,
    pub bytes: usize,
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
            resume_from: None,
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
            ServerControl::SnapshotStart {
                scrollback_pos,
                reset,
                ..
            } => {
                assert_eq!(scrollback_pos, 42);
                assert!(reset);
            }
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
