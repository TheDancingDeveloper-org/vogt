use super::*;

// -- parameter models ---------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StartSessionParams {
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub name: Option<String>,
    pub template: Option<String>,
    pub placement: Option<String>,
    pub task: Option<String>,
    pub reason: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub resume: Option<String>,
    pub permission_mode: String,
    pub autopilot: Option<bool>,
    pub role: String,
    /// The dispatch purpose (WI-1323). Unset, the start takes no profile.
    pub purpose: Option<String>,
    /// The work item a review is of (Python `review_of`). A review's brief
    /// checks the item's diff rather than carrying the work on.
    pub review_of: Option<String>,
    /// Open on the work item this names, resumed from its latest hand-off
    /// (Python `from_handoff`). Sugar for `work_item`; refused when the item
    /// has no hand-off comment.
    pub from_handoff: Option<String>,
    /// The limits the start asks for. Unset, the default rule applies.
    pub budget: Option<SessionBudget>,
    /// Cut a worktree for the session. Unset, the profile decides.
    pub worktree: Option<bool>,
    /// Draw a repo map into the brief. Unset, the profile decides.
    pub repo_map: Option<bool>,
    /// The MCP set the agent starts with (WI-2833): `worker` or `full`. Unset,
    /// the profile decides; a value that names `full` is sent, since an unset
    /// value lets the engine pick its own agent default.
    pub mcp_set: Option<String>,
    /// Never compact this session on a cold wake (WI-2037).
    pub cold_wake_opt_out: bool,
    /// The change kinds an oversight session is woken for (WI-1521), sent to
    /// the engine as `wake_on`. Empty types nothing.
    pub wake_on: Vec<String>,
    /// Profile fields the caller sent as an explicit null. Python counts these
    /// in `model_fields_set`: they are overrides, and the profile leaves them.
    pub explicit_null: Vec<&'static str>,
    /// Why this start may break the dispatch rules (WI-1329). Accepted only
    /// from an oversight session; audited as `dispatch.override`.
    pub override_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StopSessionParams {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ListSessionsParams {
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub include_stopped: bool,
    pub limit: i64,
    pub offset: i64,
    pub order: String,
    /// `digest` (default) or `full`. Digest mode pages after the live-state
    /// filter and returns rows under `digests`.
    pub mode: String,
    /// Keep only these activities. `None` uses the digest live set; an empty
    /// list keeps every state.
    pub states: Option<Vec<String>>,
    pub stall_after_minutes: i64,
    /// Ask the engine for rows it hides from the default list (WI-1289).
    pub include_hidden: bool,
    /// Summary fields copied onto each digest row as `extra`.
    pub fields: Option<Vec<String>>,
    /// Keep only sessions this one started (WI-4725): the starter's engine
    /// session id or its `ses_…` id. Unlinked rows have no starter and never
    /// match.
    pub started_by_session: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SweepSessionsParams {
    pub project: Option<String>,
    pub screen_lines: i64,
    pub stall_after_minutes: i64,
    /// Roles left out of the sweep (WI-1525).
    pub exclude_roles: Vec<String>,
    /// Keep only sessions this session started.
    pub supervised_by: Option<String>,
    /// Keep only sessions that changed after this instant.
    pub since: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LogTailParams {
    pub id: String,
    pub tail_bytes: i64,
    pub strip_ansi: bool,
}

#[derive(Debug, Clone)]
pub struct InputParams {
    pub id: String,
    pub text: Option<String>,
    pub keys: Option<Vec<String>>,
    pub submit: bool,
    pub reason: String,
    pub confirm: bool,
    pub wake_timeout_s: i64,
}

#[derive(Debug, Clone)]
pub struct ScreenParams {
    pub id: String,
    pub scrollback_lines: i64,
}

#[derive(Debug, Clone)]
pub struct WaitParams {
    pub id: String,
    pub until: String,
    pub timeout_s: i64,
    /// True restores the full screen; false returns the digest row plus an
    /// 8-line screen tail.
    pub screen: bool,
}

#[derive(Debug, Clone)]
pub struct WakeParams {
    pub id: String,
    pub reason: String,
    /// Wake even though the session was superseded, or another live session
    /// holds the same work item in the same directory.
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct KeepAwakeParams {
    pub id: String,
    pub keep_awake: bool,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct SetRoleParams {
    pub id: String,
    pub role: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct AnswerParams {
    pub id: String,
    pub option: Option<i64>,
    pub label: Option<String>,
    pub expect_question: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct HibernateParams {
    pub id: String,
    pub reason: String,
    pub allow_shell: bool,
    /// Change the session's cold-wake opt-out (WI-2037); unset keeps it.
    pub cold_wake_opt_out: Option<bool>,
    /// The session taking over this one's work, in either id form.
    pub superseded_by: Option<String>,
}

// -- result models, field order matching application/models.py ---------------

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionApprovalOption {
    pub number: i64,
    pub label: String,
    pub selected: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionApproval {
    pub question: String,
    pub command_excerpt: String,
    pub kind: String,
    pub options: Vec<SessionApprovalOption>,
    pub deadline_seconds: Option<i64>,
    pub deadline_at: Option<String>,
    /// Absent when the engine did not say when it noticed, which serialises as
    /// null rather than an empty string.
    pub detected_at: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionBlocked {
    /// What the session is blocked on, in its words. Python's `SessionBlocked`
    /// names this `blocker`, so the wire does too.
    pub blocker: String,
    pub items: Vec<String>,
    pub since: Option<String>,
    /// Who can resolve it: `person`, `overseer` or `either` (WI-996).
    pub needs: &'static str,
    /// What kind of boundary it is (WI-996).
    pub boundary: &'static str,
    /// The content key `session:boundary:hash(reason)` (WI-996); null from an
    /// older engine.
    pub key: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResources {
    pub rss_bytes: i64,
    pub cpu_pct: f64,
    pub processes: i64,
    pub sampled_at: String,
    pub over_threshold: bool,
    pub memory_current: Option<i64>,
    pub memory_max: Option<i64>,
    pub memory_peak: Option<i64>,
    pub oom_kill: Option<i64>,
    pub observed_at: Option<String>,
    pub memory_warning: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionHibernation {
    pub at: String,
    pub trigger: String,
    pub reason: Option<String>,
    pub resumable: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionRuntime {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub model_basis: Option<String>,
    pub effort: Option<String>,
    pub effort_basis: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    #[serde(default = "linked_default")]
    pub linked: bool,
    pub id: String,
    pub engine_session_id: String,
    pub project: Option<String>,
    pub work_item: Option<String>,
    pub work_item_title: Option<String>,
    pub work_item_state: Option<String>,
    pub actor: Option<String>,
    pub cwd: String,
    pub template: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reason: Option<String>,
    /// The dispatch purpose the start named (WI-1323), when it named one.
    pub purpose: Option<String>,
    /// The engine id of the session that started this one, when a session did.
    pub started_by_session: Option<String>,
    /// Whether the starter was a person, or a session in the oversight or
    /// worker role at the time.
    pub started_by_kind: Option<String>,
    pub started_at: Option<Moment>,
    pub stopped_at: Option<Moment>,
    // The engine's own facts about the process, copied straight across. They
    // sit here because pydantic emits fields in declaration order and Python
    // declares them right after `stopped_at`.
    pub name: Option<String>,
    pub command: Option<String>,
    pub exit_code: Option<i64>,
    pub activity_changed_at: Option<String>,
    pub conversation_agent: Option<String>,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<SessionApproval>,
    pub blocked: Option<SessionBlocked>,
    /// What the agent's own terminal program reports about itself over OSC 7501 (state, kind, progress, msg, title, app); null until it reports, and after it clears or exits. Untrusted: anything in the PTY can emit it, so it never replaces `activity`.
    #[serde(default)]
    pub reported_status: Option<serde_json::Map<String, serde_json::Value>>,
    pub last_reply_excerpt: Option<String>,
    pub hibernation: Option<SessionHibernation>,
    pub keep_awake: Option<bool>,
    pub autopilot: Option<bool>,
    pub autopilot_nudges: Option<i64>,
    pub role: Option<String>,
    pub stopped_by: Option<String>,
    pub stop_reason: Option<String>,
    pub permission_mode: Option<String>,
    pub posture_lost_on_resume: bool,
    /// Live from the engine: the named Claude Code account (WI-2026). Null is the default account.
    pub account: Option<String>,
    pub running: Option<SessionRuntime>,
    pub resources: Option<SessionResources>,
    /// Live from the engine: the limits this session was started with (WI-2026).
    pub budget: Option<SessionBudget>,
    pub conversation_id: Option<String>,
    /// Digest-only: when autopilot declared itself done. Not on the wire summary.
    #[serde(skip)]
    pub done_at: Option<String>,
    /// Digest-only: the latest session report, as one line.
    #[serde(skip)]
    pub latest_report: Option<String>,
    /// Digest-only: the engine economics object, when present.
    #[serde(skip)]
    pub economics: Option<serde_json::Map<String, serde_json::Value>>,
}

fn linked_default() -> bool {
    true
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionResult {
    pub session: SessionSummary,
    /// On `session.start`: the purpose an `implement` start ran as instead,
    /// because its item failed review `dispatch.escalate_after_failed_reviews`
    /// times (WI-1329).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escalated_to: Option<String>,
}

/// One digest row. Field order matches `views::SessionDigest` / Python
/// `SessionDigest`. Local so the service does not name a private model module.
#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct SessionDigestRow {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) work_item: Option<String>,
    pub(super) activity: Option<String>,
    pub(super) attention: Option<String>,
    pub(super) ready: Option<bool>,
    pub(super) blocked_reason: Option<String>,
    pub(super) blocked_detail: Option<String>,
    pub(super) turn_started_at: Option<String>,
    pub(super) done_at: Option<String>,
    pub(super) last_reply_excerpt: Option<String>,
    pub(super) report: Option<String>,
    pub(super) template: Option<String>,
    pub(super) brief_file: Option<String>,
    pub(super) question: Option<String>,
    pub(super) question_kind: Option<String>,
    pub(super) options: Option<Vec<String>>,
    pub(super) budget: Option<String>,
    pub(super) supervisor: Option<String>,
    pub(super) extra: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, serde::Serialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionSummary>,
    /// Digest rows (design C1). `mode` defaults to `digest`, so the ordinary
    /// list returns these and leaves `sessions` empty; `mode=full` does the
    /// reverse.
    #[serde(default)]
    pub digests: Vec<SessionDigestRow>,
    /// Why the engine could not be asked, or null when it answered.
    pub engine: Option<String>,
    /// How many sessions match the filters, before `limit`/`offset`.
    #[serde(default)]
    pub total: i64,
    /// More sessions match than this page returns.
    #[serde(default)]
    pub truncated: bool,
    /// How many sessions the engine left out of its default list (WI-1289).
    /// 0 from an engine that does not report it, and when no engine answered.
    #[serde(default)]
    pub hidden_count: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepRow {
    pub attention: String,
    pub attention_reason: String,
    pub session: SessionSummary,
    /// Empty when the sweep asked for no screen lines: left out, not `[]`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub screen_tail: Vec<String>,
    pub ready: Option<bool>,
    /// On an `approval` row: an overseer's recorded pick for the dialog it
    /// shows, `{option, label, by, at, escalation_key}` (WI-996 E0b). Null
    /// otherwise.
    pub recommendation: Option<Value>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSweepResult {
    pub rows: Vec<SessionSweepRow>,
    pub counts: serde_json::Map<String, Value>,
    pub swept_at: Moment,
    pub engine: Option<String>,
}

/// What `log_tail` says when no session engine is configured at all.
/// Screen lines a wait keeps when it was not asked for the whole screen.
pub(super) const WAIT_TAIL_LINES: usize = 8;

pub(super) const NO_ENGINE: &str = "no session engine is configured (VOGT_ENGINE_URL is unset)";

#[derive(Debug, Clone, serde::Serialize)]
pub struct LogTailResult {
    pub session_id: Option<String>,
    pub text: String,
    pub bytes: i64,
    pub total_bytes: i64,
    pub truncated: bool,
    pub engine: Option<String>,
}

impl LogTailResult {
    /// No output to show: an unknown id or history that is off.
    pub(super) fn empty() -> Self {
        Self {
            session_id: None,
            text: String::new(),
            bytes: 0,
            total_bytes: 0,
            truncated: false,
            engine: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionScreenResult {
    pub id: String,
    pub engine_session_id: String,
    pub cols: i64,
    pub rows: i64,
    pub lines: Vec<String>,
    pub cursor: Option<SessionCursor>,
    pub title: Option<String>,
    pub activity: Option<String>,
    pub alive: Option<bool>,
    pub ready: Option<bool>,
    pub scrollback: Vec<String>,
    pub turn_started_at: Option<String>,
    pub last_output_at: Option<String>,
    pub approval: Option<SessionApproval>,
    pub blocked: Option<SessionBlocked>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionCursor {
    pub row: i64,
    pub col: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionWaitResult {
    pub id: String,
    pub engine_session_id: String,
    pub outcome: String,
    pub matched: bool,
    pub waited_ms: i64,
    pub screen: Option<SessionScreenResult>,
    pub digest: Option<SessionDigestRow>,
    pub screen_tail: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInputResult {
    pub id: String,
    pub engine_session_id: String,
    pub linked: bool,
    pub bytes: i64,
    pub keys: Vec<String>,
    pub submitted: bool,
    pub delivery: String,
    pub delivery_evidence: String,
    pub woke: bool,
    pub digest: Option<SessionDigestRow>,
}
