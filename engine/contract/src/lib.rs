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
    /// The brief the session's agent should start from. The engine writes it
    /// to a prompt file under its own `state_dir` and hands the child the
    /// path; callers that have no brief omit the field entirely. It exists
    /// because the caller (vogt-core) is a separate process and cannot write
    /// a file the engine owns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Which model the agent CLI in `command` should run, and how hard it
    /// should think (FR-T11). The engine turns these into that CLI's own
    /// flags or environment; a command it has no mapping for is refused by
    /// name rather than started without them, because a session that
    /// silently ran the default model is the failure this field exists
    /// against — it works, it answers, and it is not what was asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
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
    /// Explicit command the session was created with, if any (None for
    /// default-shell sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub created_at: String,
    /// Wall-clock instant when the current activity state began. This keeps
    /// live attention occurrence keys stable across reads.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub activity_changed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub summary: SessionSummary,
    #[serde(default)]
    pub scrollback_pos: u64,
    #[serde(default)]
    pub scrollback_base64: String,
}

/// A pending assistant effect awaiting the single on-screen approval gate.
///
/// This is deliberately shared by every client surface. It is ephemeral and
/// carries the exact effector payload that the engine holds in memory; it is
/// not an approval ledger or a history record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingAction {
    /// Exact text, and whether the engine will append Enter, for one PTY.
    SendInput(SendInputAction),
    /// Exact registry arguments, pretty printed for human review.
    VogtWrite(VogtWriteAction),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendInputAction {
    pub id: Uuid,
    pub session_id: Uuid,
    pub session_name: String,
    pub text: String,
    pub submit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VogtWriteAction {
    pub id: Uuid,
    pub operation: String,
    pub target: String,
    pub reason: String,
    pub payload: String,
}

impl PendingAction {
    pub fn id(&self) -> Uuid {
        match self {
            Self::SendInput(action) => action.id,
            Self::VogtWrite(action) => action.id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantTranscriptEntry {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_trace: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantReply {
    pub reply: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_action: Option<PendingAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_trace: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantHistory {
    pub transcript: Vec<AssistantTranscriptEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_action: Option<PendingAction>,
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
    /// Something changed in vogt-core.
    ///
    /// Republished onto this stream by the front door so that a client with
    /// one event source has both halves of the product on it (FR-U10). The
    /// payload is deliberately thin — kind, what changed, and the core's own
    /// sequence number — because a client that wants the change reads it
    /// from Vogt; this says only that there is something to read, which is
    /// what stops a board from polling.
    VogtChanged {
        /// The core's event kind, verbatim — e.g. `work.transitioned`,
        /// `drift.raised`. Spelled here as `services/drift_service.py` spells
        /// it, because this comment is what someone writing a filter reads:
        /// it said `drift.opened`, which the core has never emitted, and a
        /// filter built on that would match nothing while looking right.
        kind: String,
        /// What the change was about, as the core names it.
        entity_kind: String,
        entity_id: String,
        /// The core's sequence number, so a client can tell order and gaps.
        seq: i64,
    },
    /// An agent-task run held at a prompt boundary on a declared approval gate
    /// (#289). The PTY is paused at the question; a client renders the options
    /// and a phone push invites an answer. The gate is not resolved until a
    /// person (or the audited `--auto-approve` bypass) picks an option, or it
    /// fails closed to `blocked` — reported as `task.gate.answered` either way.
    ///
    /// The tag is spelled explicitly rather than left to the container's
    /// kebab-case rule so it reads `task.gate.opened`, the dotted name the PWA
    /// and phone filter on.
    #[serde(rename = "task.gate.opened")]
    TaskGateOpened {
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        gate_id: Uuid,
        question: String,
        /// The option labels, in the order they were declared.
        options: Vec<String>,
    },
    /// A gate reached a terminal state (#289). `outcome` is `approved` when a
    /// person or the audited bypass chose an option, and `blocked` when the
    /// gate was interrupted, timed out, or its session died — the fail-closed
    /// half of the guarantee, where `interrupted != approved`. `actor` names
    /// who resolved it (`auto-approve` for the bypass, a reason string for a
    /// fail-closed block).
    #[serde(rename = "task.gate.answered")]
    TaskGateAnswered {
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        gate_id: Uuid,
        /// The chosen option's label, present only for an `approved` outcome.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option: Option<String>,
        /// `approved` or `blocked`.
        outcome: String,
        actor: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Mid-run steering was delivered to an agent-task run's PTY at a prompt
    /// boundary (#289). `interrupt` records whether the CLI's cancel was sent
    /// first. `actor` and `reason` are the audit trail — who steered and why.
    #[serde(rename = "task.steered")]
    TaskSteered {
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        actor: String,
        interrupt: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// An agent-task run reached a terminal state and its conclusion was
    /// recorded (#291). `outcome` is the typed verdict — `succeeded`, `failed`,
    /// `partially-succeeded`, `skipped`, or `blocked` (a run stopped at a #289
    /// gate). The remaining fields are the durable conclusion a client renders
    /// without re-reading the whole run: how long it ran, what it exited with,
    /// how many schema re-prompts it took, the final sha of the bound branch
    /// and what it changed there, and the parsed cost when the CLI reported
    /// usage. Additive to the stream — a client that does not know this event
    /// still reads `session.killed` for the same run.
    ///
    /// Spelled `task.run.concluded` explicitly, the dotted name clients filter
    /// on, matching the other `task.*` events above.
    #[serde(rename = "task.run.concluded")]
    TaskRunConcluded {
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        /// `succeeded` | `failed` | `partially-succeeded` | `skipped` | `blocked`.
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: u64,
        /// Schema re-prompts spent before the findings validated (or the run
        /// was given up on); 0 when no `output_schema` was set.
        #[serde(default)]
        retries: u32,
        /// The bound branch the run worked on, when its workspace is a repo.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        /// Tip sha of the bound branch at the moment the run finished.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        final_sha: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files_changed: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        insertions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletions: Option<u64>,
        /// Parsed cost in USD when the CLI reported usage, else absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
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
    /// On-disk modification time, milliseconds since the Unix epoch. Lets a
    /// client detect that a file changed underneath it since it last read.
    #[serde(default)]
    pub mtime: u64,
    /// SHA-256 of the file's bytes, hex-encoded. The robust, content-based
    /// half of optimistic concurrency: pass it back as `WriteReq::if_match`.
    #[serde(default)]
    pub hash: String,
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
    /// Optimistic-concurrency guard: the SHA-256 hex the client last read for
    /// this file. When present and the on-disk hash no longer matches, the
    /// write is refused with 409 Conflict rather than clobbering newer content.
    /// Absent (`None`) preserves the original last-writer-wins behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_match: Option<String>,
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
    /// SHA-256 hex of the bytes just written — the client adopts this as its
    /// new `if_match` baseline without needing to re-read the file.
    #[serde(default)]
    pub hash: String,
    /// On-disk mtime after the write, milliseconds since the Unix epoch.
    #[serde(default)]
    pub mtime: u64,
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
    fn gate_and_steer_events_carry_dotted_type_tags() {
        // The PWA and phone filter on the dotted names; the container's
        // kebab-case rule would otherwise spell them `task-gate-opened`.
        let opened = ServerEvent::TaskGateOpened {
            task_id: Uuid::nil(),
            run_id: Uuid::nil(),
            session_id: Uuid::nil(),
            gate_id: Uuid::nil(),
            question: "Deploy to prod?".into(),
            options: vec!["Approve".into(), "Hold".into()],
        };
        let json = serde_json::to_value(&opened).unwrap();
        assert_eq!(json["type"], "task.gate.opened");
        assert_eq!(json["question"], "Deploy to prod?");
        assert_eq!(json["options"][0], "Approve");

        let answered = ServerEvent::TaskGateAnswered {
            task_id: Uuid::nil(),
            run_id: Uuid::nil(),
            session_id: Uuid::nil(),
            gate_id: Uuid::nil(),
            option: None,
            outcome: "blocked".into(),
            actor: "timed out".into(),
            reason: Some("no answer within deadline".into()),
        };
        let json = serde_json::to_value(&answered).unwrap();
        assert_eq!(json["type"], "task.gate.answered");
        assert_eq!(json["outcome"], "blocked");
        // A blocked outcome names no option.
        assert!(json.get("option").is_none());

        let steered = ServerEvent::TaskSteered {
            task_id: Uuid::nil(),
            run_id: Uuid::nil(),
            session_id: Uuid::nil(),
            actor: "operator".into(),
            interrupt: true,
            reason: None,
        };
        assert_eq!(
            serde_json::to_value(&steered).unwrap()["type"],
            "task.steered"
        );
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

    #[test]
    fn session_spec_reads_a_prompt_from_the_wire() {
        // vogt-core codes against this exact field name.
        let spec: SessionSpec =
            serde_json::from_str(r#"{"name":"term","prompt":"Fix the flaky test."}"#).unwrap();
        assert_eq!(spec.prompt.as_deref(), Some("Fix the flaky test."));
    }

    #[test]
    fn session_spec_reads_model_selection_from_the_wire() {
        // vogt-core is a separate Python process. This pins the actual JSON
        // field names at that language boundary, rather than only testing a
        // Rust value built in this crate.
        let spec: SessionSpec = serde_json::from_str(
            r#"{"name":"term","command":["codex"],"model":"gpt-5.6","effort":"medium"}"#,
        )
        .unwrap();
        assert_eq!(spec.model.as_deref(), Some("gpt-5.6"));
        assert_eq!(spec.effort.as_deref(), Some("medium"));
    }
}
