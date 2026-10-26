//! Session, agent-CLI, activity and decision operations' models.
//!
//! Ported from `src/vogt/application/models.py`; each schema is checked against
//! Python's live registry by `tests/test_rust_schemas.py`.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::views::{
    AgentActivityEvent, AgentActivitySessionSummary, AgentCliRow, BindSessionWorkResultEngineLabel,
    DecideGrantParamsDecision, DecisionOptionModel, DecisionView, HistoryListParamsMode,
    HistoryOutputMatch, HistorySessionRow, ListDecisionsParamsState, ListGrantsParamsState,
    ListSessionsParamsMode, ListSessionsParamsOrder, RequestGrantParamsKind,
    RequestGrantParamsUses, SessionApprovalOption, SessionBlocked, SessionBudgetParams,
    SessionChange, SessionDigest, SessionGrantView, SessionInputParamsKeysItem,
    SessionInputResultKeysItem, SessionReply, SessionScreenResult, SessionSummary, SessionSweepRow,
    SessionWaitAnyParamsCursor, SessionWaitParamsUntil, SetSessionRoleParamsRole,
    StartSessionParamsPermissionMode, StartSessionParamsPurpose, StartSessionParamsRole,
    SweepSessionsParamsMode,
};
use super::{date_time, optional_date_time, optional_string};

/// Open a terminal for a work item, or for a project.
///
/// At most one of `work_item` and `project`. A session always belongs to a
/// project — the work item's own, when one is given — because the working
/// directory comes from the project registry and nowhere else.
///
/// ** Giving **neither** is allowed only when the deployment has
/// configured a scratch project, and resolves to it. It exists for the spoken
/// request with no subject — "research the best risotto in Wollongong" — which
/// has no work item and no repository but still needs a registered tree to
/// open in. The scratch project is a project like any other: registered, with
/// a root path somebody chose. What is *not* allowed is inventing a directory,
/// which is the failure the registry-owned `cwd` rule exists against.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StartSessionParams {
    /// Work item ref, e.g. WI-7.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Session template to run, by name or by tag — the engine expands it against the deployment's templates. Use this to run an agent rather than a plain shell: `claude` (or `codex`, `opencode`, `klaudia`) starts that agent under the deployment's protected wrapper, so a request to *do* something in a session names one here. Omitted means a plain shell, which does nothing until typed into.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub template: Option<String>,
    /// What the session's agent should do, in the user's words. Folded into the brief as its Task section; an agent template (Claude Code, Codex, OpenCode, Klaudia) is started with a first prompt telling it to read the brief and carry that task out, so 'start a session on X and check its containers' opens an agent already checking them. A plain shell only gets the brief's path in VOGT_ENGINE_AGENT_TASK_PROMPT_FILE, so pair a task with `template` when it is something to carry out. Omitted on an agent start, the task is taken from `reason` (a leading `Task:` dropped); a `review` purpose gets a default review task.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub task: Option<String>,
    /// Files the task touches, each `path` or `path:start-end`. Rendered under Touch points; a line range is a hint, not a slice of the file.
    #[serde(default)]
    pub files: Vec<String>,
    /// Symbols the task touches. Rendered under Touch points.
    #[serde(default)]
    pub symbols: Vec<String>,
    /// What finished looks like, one line each. Rendered under Touch points.
    #[serde(default)]
    pub done: Vec<String>,
    /// Checks that must pass before the task is done. Rendered under Touch points.
    #[serde(default)]
    pub gates: Vec<String>,
    /// Include a two-level outline of the touch-point directories in the brief. Omitted means include one when touch points name files, and not otherwise. The outline is capped; it is a map, not a listing.
    #[serde(default)]
    pub repo_map: Option<bool>,
    /// The agent's permission posture. `default`: Claude Code's own mode (auto) with the deployment's driven-session policy — routine safe work proceeds, prod-mutating, destructive, shared-resource and secret-exposing actions are denied. `accept_edits`: file edits auto-accepted, everything else asks. `bypass`: no permission checks at all, for one trusted task — only a person may grant it (an agent is refused), and it is audited. Claude Code, Codex, opencode and Klaudia templates. Codex maps the same three postures: `default` is `-a on-request -s danger-full-access`; `accept_edits` adds `approvals_reviewer="auto_review"`; `bypass` is `--dangerously-bypass-approvals-and-sandbox` and stays people-only. A named Codex requirements file that is missing, empty or unreadable refuses the Codex launch (400) and does not fall through to bypass. Klaudia takes `default` and `bypass` only (its `acceptEdits` is an alias for its default autonomous mode, so `accept_edits` is refused).
    #[serde(default = "default_start_session_params_permission_mode")]
    pub permission_mode: StartSessionParamsPermissionMode,
    /// Run the session under a named Claude Code subscription account (a name from the engine's ENGINE_AGENT_CLAUDE_ACCOUNTS). Omitted uses the engine's default account. Claude Code templates only; an unknown name is refused, never started on the default login.
    #[serde(default)]
    #[schemars(schema_with = "optional_start_session_params_account")]
    pub account: Option<String>,
    /// Continue a previous conversation of the template's agent CLI instead of starting a new one: its own conversation id, mapped to `claude --resume <id>`, `codex resume <id>`, `opencode --session <id>` or `klaudia --resume <id>`. A Claude Code (or Klaudia) session Vogt started fresh has the engine session id (`engine_session_id`) as its conversation id, so a session lost to a restart can be resumed by that. The engine starts a resumed conversation in the directory its transcript records (when inside the workspace), so one begun under a worktree or a parent folder resumes from any project. Requires `template`; letters, digits and . _ - only, never a leading dash.
    #[serde(default)]
    #[schemars(schema_with = "optional_start_session_params_resume")]
    pub resume: Option<String>,
    /// Session name. Derived from the subject if omitted.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub name: Option<String>,
    /// Model id for the agent CLI this template runs, e.g. 'gpt-5.6' or 'claude-sonnet-4-5'. Omitted means the CLI's own default. Requires `template`: a plain shell has no model. A template that cannot be told which model to use refuses rather than ignoring this.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub model: Option<String>,
    /// Reasoning effort for the agent CLI, e.g. 'low' / 'medium' / 'high'. Omitted means the CLI's own default. Requires `template`: a plain shell has no effort to set, so do not volunteer one for a terminal that runs no agent.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub effort: Option<String>,
    /// Keep the agent working unattended: its brief tells it to carry on when its next step needs no person, stopping only when it is blocked on a person (reported with session_report_blocked) or no unblocked work is left (then it prints `AUTOPILOT: DONE`). The engine also re-prompts it whenever it stops at its prompt with work left, and its idle policy leaves it awake. Omitted means on for an agent template started with a `task`, off otherwise; `false` turns it off.
    #[serde(default)]
    pub autopilot: Option<bool>,
    /// `oversight` nominates the session to supervise others (drive rollouts, check on and revive worker sessions): it is pinned awake, so it comes back by itself after a redeploy, and the GUI lists it first. `worker` (default) is every other session. Only a person starts an oversight session; an agent caller is refused. Change it later with session.set_role.
    #[serde(default = "default_start_session_params_role")]
    pub role: StartSessionParamsRole,
    /// Start a fresh session from a work item's hand-off comment: the work item ref (e.g. WI-7). Refused when that item has no hand-off comment, and refused when `work_item` is also given and names a different item. The brief carries the latest hand-off.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub from_handoff: Option<String>,
    /// Start a reviewer on this work item (e.g. WI-7) instead of a worker. The brief checks the diff named by the item's latest hand-off against the item's own acceptance criteria, and it does not carry the work on. Refused without a work item — a review is of an item — and when `work_item` or `from_handoff` names a different one.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub review_of: Option<String>,
    /// Limits the engine enforces on this session (C7): a soft warning as one is approached, then hibernation (trigger `budget`) when one is reached. Omitted means the default for an agent template started with a `task` as a worker — 120 turns and 90 minutes of wall clock, no context limit — and no budget otherwise. An empty object sets no limits, which is how a start declines the default.
    #[serde(default)]
    pub budget: Option<SessionBudgetParams>,
    /// Start the session in a git worktree of its own rather than in the project's shared checkout (WI-1298). The engine cuts the worktree from the project's integration branch — `agent_policy.base_branch` when the project states one, otherwise `main` — onto a new branch `<work item>-<session>` (or `<project>-<session>` when no work item is given), and the session's working directory is that worktree. The shared checkout is never written to, so a dirty tree there cannot leak into the session and the session cannot disturb it. Requires a project, or a work item that belongs to one. Unset, the purpose's dispatch profile decides (WI-1323), and with no purpose the session opens in the project's checkout, as `false` does. Cannot be combined with `cwd` (WI-1671). Recorded on the audit row.
    #[serde(default)]
    pub worktree: Option<bool>,
    /// Open the session in this existing directory instead of the project's registered checkout (WI-1671). Use it when a worktree already exists and `worktree=true` cannot cut a new one — a private origin the engine cannot fetch, for example. The path must exist, must sit under the workspace's `Active/` directory (the parent of the project's checkout), must not be inside that checkout (a subdirectory shares its git toplevel; a linked worktree nested under it does not), and must be a git worktree of the same repository (the same `commondir`). Anything else is refused, and so is combining it with an explicit `worktree=true`. A purpose profile that would have set `worktree` does not, when `cwd` is given. Recorded as the session's cwd.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub cwd: Option<String>,
    /// Never compact this session on a cold wake (WI-2037): every wake resumes its full transcript, whatever the deployment's cold_wake_compaction says. Changeable later at session_hibernate.
    #[serde(default)]
    #[schemars(extend("default" = false))]
    pub cold_wake_opt_out: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
    /// What the session is for (WI-1323, design §2.4): implement, implement-careful, review, mechanical, investigate or design. The deployment's dispatch profile for it fills template, model, effort, budget, worktree, repo_map and autopilot wherever this start leaves them unset; a value given here wins and is audited as an override. Recorded on the session.
    #[serde(default)]
    pub purpose: Option<StartSessionParamsPurpose>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionResult {
    pub session: SessionSummary,
    #[serde(default)]
    pub digest: Option<SessionDigest>,
    /// On `session.start` for a project with a landing rule: the rule the worker runs under and any task lines it overrode.
    #[serde(default)]
    pub effective_policy: Option<EffectivePolicy>,
}

/// The landing rule a started session runs under (WI-1832).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectivePolicy {
    pub base_branch: String,
    pub lands_by: String,
    pub merge_by: String,
    pub merge_mode: String,
    pub deploy_by: String,
    /// Where the task contradicted this policy, one line per axis (PR vs push only, merge, base branch, deploy). The policy wins and the brief says so; empty when the task agreed or set none.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub overrides: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSessionsParams {
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// `started` (default): newest Vogt links first, then unlinked sessions. `rss`: heaviest process tree first, to find the session holding the memory. Either order is applied to the joined set before the page is cut.
    #[serde(default = "default_list_sessions_params_order")]
    pub order: ListSessionsParamsOrder,
    /// Work item ref.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// Include sessions Vogt has already stopped.
    #[serde(default)]
    pub include_stopped: bool,
    /// `digest` (default) or `full` (every SessionSummary field).
    #[serde(default = "default_list_sessions_params_mode")]
    pub mode: ListSessionsParamsMode,
    /// Extra SessionSummary attribute names on each digest row. A comma-separated string is accepted as well as a list.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub fields: Vec<String>,
    /// Keep only sessions this one started: the starter's engine session id, or its Vogt `ses_…` id. Unlinked engine sessions (no starter) never match.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub started_by_session: Option<String>,
    /// Keep only these activities. Default excludes hibernated, stopped and exited; pass an empty list for every state.
    #[serde(default)]
    pub states: Option<Vec<String>>,
    /// A running session that has printed nothing for this long is classified `stalled` on a digest row.
    #[serde(default = "default_list_sessions_params_stall_after_minutes")]
    #[schemars(range(min = 1, max = 1440))]
    pub stall_after_minutes: i64,
    /// Also return sessions the engine hides from its default list (WI-1289): aged out, finished or idle-and-not-alive, with no recent person input. Off by default. `hidden_count` on the result counts them either way, and session.get still resolves one.
    #[serde(default)]
    pub include_hidden: bool,
    #[serde(default = "default_list_sessions_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionListResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub digests: Vec<SessionDigest>,
    /// What the engine said, when it could not be asked. The links are still returned: Vogt's record of what it started does not depend on the engine being up.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
    /// How many sessions match the filters, before `limit`/`offset`. `truncated` is set when this page is not all of them. When more declared links match than `session.list` reads (500), `total` is one past the number actually read and `truncated` stays true on every page, including an empty one past the end.
    #[serde(default)]
    pub total: i64,
    /// More sessions match than this page returns.
    #[serde(default)]
    pub truncated: bool,
    /// How many sessions the engine left out of its default list (WI-1289), counted before this operation's own filters and paging. 0 from an engine that does not report it. Those sessions are in `sessions` only when `include_hidden` was set.
    #[serde(default)]
    pub hidden_count: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionInputParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Text to type, sent verbatim (at most 64 KiB of UTF-8). Sent first, before any keys.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub text: Option<String>,
    /// Named keys to press after the text, in order: enter, esc, tab, up, down, left, right, ctrl-c, ctrl-d, backspace. Each is its own write, so an Esc is not read as Alt+<next key>.
    #[serde(default)]
    pub keys: Option<Vec<SessionInputParamsKeysItem>>,
    /// Press Enter last, after the text and keys. The default when text is given: the line is sent, not left sitting in the input. A call that names only keys does not press Enter unless this is set. `draft` or an explicit `false` types without pressing Enter.
    #[serde(default = "default_session_input_params_submit")]
    pub submit: bool,
    /// Type but do not press Enter: the text (and any keys) sit in the input. Forces `submit` off. Use it to compose a line without sending it.
    #[serde(default)]
    pub draft: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
    /// When Enter is pressed, watch the session for up to ~2 s and say whether the input was delivered (a turn started) or queued behind a running turn. False returns at once (`unconfirmed`).
    #[serde(default = "default_session_input_params_confirm")]
    pub confirm: bool,
    /// When the session is hibernated, it is woken first and the input waits until it is ready, for at most this many seconds.
    #[serde(default = "default_session_input_params_wake_timeout_s")]
    #[schemars(range(min = 1, max = 600))]
    pub wake_timeout_s: i64,
}
/// What was sent. The text itself is never echoed or audited.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionInputResult {
    /// The id the caller named, as given.
    pub id: String,
    pub engine_session_id: String,
    /// True when the session is one Vogt started (has a ses_ id).
    pub linked: bool,
    /// UTF-8 bytes of `text` written.
    pub bytes: i64,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub keys: Vec<SessionInputResultKeysItem>,
    /// Enter was pressed — by `submit` or as one of the `keys`.
    #[serde(default)]
    pub submitted: bool,
    /// typed (no Enter: the text sits in the input) / delivered (a turn started) / queued (behind a running turn; once it ends Vogt checks the input box and presses Enter if the text is still there) / stuck (at rest with the text still in the input box: the submit did not land; the sweep shows it as input pending) / wedged (Codex echoed the text after a model-API error but started no turn; restart with session_hibernate then session_wake, which keeps the worktree and conversation) / unconfirmed (sent; nothing observable yet). A refused input is an error, not a value.
    #[serde(default = "default_session_input_result_delivery")]
    pub delivery: String,
    /// What `delivery` was judged from.
    #[serde(default)]
    pub delivery_evidence: String,
    /// True when the session was hibernated and was woken first.
    #[serde(default)]
    pub woke: bool,
    #[serde(default)]
    pub digest: Option<SessionDigest>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepSessionsParams {
    /// Only sessions Vogt linked to this project (slug).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// `digest` (default) is one line per session; `full` is the SessionSummary a GUI renders, including command.
    #[serde(default = "default_sweep_sessions_params_mode")]
    pub mode: SweepSessionsParamsMode,
    /// Extra SessionSummary attribute names copied onto each digest row.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub fields: Vec<String>,
    /// Keep only these activities (or attention classes). Default excludes hibernated, stopped and exited; pass an empty list for every state.
    #[serde(default)]
    pub states: Option<Vec<String>>,
    /// Keep only these attention classes. `needs_you` means approval, blocked and waiting.
    #[serde(default)]
    pub attention: Option<Vec<String>>,
    /// How many rows to return. Counts cover every session.
    #[serde(default = "default_sweep_sessions_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    /// Non-blank lines from the bottom of each session's screen. Zero (the default) asks for none; a positive value is honoured.
    #[serde(default)]
    #[schemars(range(min = 0, max = 40))]
    pub screen_lines: i64,
    /// A running session that has printed nothing for this long is reported `stalled`.
    #[serde(default = "default_sweep_sessions_params_stall_after_minutes")]
    #[schemars(range(min = 1, max = 1440))]
    pub stall_after_minutes: i64,
}
/// Every live and hibernated session at once, most urgent first.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionSweepResult {
    pub rows: Vec<SessionSweepRow>,
    /// Rows per attention class, plus `needs_you` and `total`.
    pub counts: std::collections::BTreeMap<String, i64>,
    #[schemars(schema_with = "date_time")]
    pub swept_at: String,
    /// Why the engine could not be asked, when it could not.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HibernateSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Hibernate a session that has no agent conversation to resume (a shell). It wakes as a fresh process in the same directory.
    #[serde(default)]
    pub allow_shell: bool,
    /// The session taking over this one's work (ses_… or the engine UUID). Recorded on the hibernation, and a later wake is refused unless forced — so the work stays with one session.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub superseded_by: Option<String>,
    /// Arm a watch before hibernating: the session is woken, and told the outcome, when this trigger fires (see watch_create).
    #[serde(default)]
    pub wake_on: Option<super::caught_up::WatchTriggerModel>,
    /// How long the watch stays armed: default 24 h, at most 14 d. Past it the watch reads `expired` and wakes nobody.
    #[serde(default = "default_hibernate_wake_on_ttl")]
    #[schemars(range(min = 60, max = 1_209_600), extend("default" = 86400))]
    pub wake_on_ttl_seconds: i64,
    /// Never compact this session on a cold wake (WI-2037): a wake always resumes its full transcript, even when the deployment's cold_wake_compaction is on and the prompt cache has expired. Unset keeps the session's current choice (a new session follows the deployment).
    #[serde(default)]
    pub cold_wake_opt_out: Option<bool>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
fn default_hibernate_wake_on_ttl() -> i64 { 86_400 }
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WakeSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Wake even though the session was superseded, or another live session holds the same work item in the same directory. Off by default, and session.input never sets it.
    #[serde(default)]
    pub force: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct KeepSessionAwakeParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// True pins the session awake; false lets policy hibernate it.
    pub keep_awake: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// The new display name. Trimmed by the engine, which refuses an empty one or one over its byte limit.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetSessionRoleParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// `oversight` nominates the session to supervise others (and pins it awake); `worker` makes it an ordinary session again and, for a session that was oversight, lifts that pin so the idle policy applies again.
    pub role: SetSessionRoleParamsRole,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindSessionWorkParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has). Omit it from inside a session Vogt started to bind that session itself.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub id: Option<String>,
    /// The work item the session now serves, e.g. WI-7 (a native ref or an upstream subject key); null unbinds it. Rebinding from one item to another is one call.
    #[schemars(required)]
    #[schemars(schema_with = "optional_bind_session_work_params_work_item")]
    pub work_item: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindSessionWorkResult {
    pub session: SessionSummary,
    /// The item the session was bound to before.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub previous_work_item: Option<String>,
    /// The item belongs to a different project from the session's. The bind still happened; binding never moves the terminal.
    #[serde(default)]
    pub project_mismatch: bool,
    /// Whether the engine's `work_item` label was written. For a linked session the core row is the truth and a label that could not be written is reported, not fatal; an unlinked session has only the label.
    pub engine_label: BindSessionWorkResultEngineLabel,
    /// What the engine said, when the label could not be written.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
}
/// Ask a person to approve one scoped item for one session.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestGrantParams {
    /// The session the grant is for: ses_… or the engine's UUID. Asking for a session other than your own needs you to be an oversight session.
    pub target: String,
    /// `credential` (one named secret). `capability` is not available yet (milestone 2).
    #[serde(default = "default_request_grant_params_kind")]
    pub kind: RequestGrantParamsKind,
    /// The secret's name in the secrets manager, e.g. 100.109.218.11_SSH. Cadastre `lookup` says where a credential lives.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub secret_name: Option<String>,
    /// The secrets-manager project that holds it. The engine accepts only projects open to grants.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project_id: Option<String>,
    /// The name the session fetches it under (`vogt-agent-auth fetch VAR`). Defaults to GRANT_<secret name>.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub var: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub capability: Option<String>,
    /// `once`: the first fetch consumes it. `ttl`: any number of fetches until it expires.
    #[serde(default = "default_request_grant_params_uses")]
    pub uses: RequestGrantParamsUses,
    /// Lifetime once approved, 60 s to 24 h.
    #[serde(default = "default_request_grant_params_ttl_seconds")]
    #[schemars(range(min = 60, max = 86400))]
    pub ttl_seconds: i64,
    /// Why the target needs it. The approver reads this.
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionGrantResult {
    pub grant: SessionGrantView,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecideGrantParams {
    /// The grant id (grt_…).
    pub id: String,
    pub decision: DecideGrantParamsDecision,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevokeGrantParams {
    /// The grant id (grt_…).
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListGrantsParams {
    #[serde(default)]
    pub state: Option<ListGrantsParamsState>,
    /// Only grants for this session (either id).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub target: Option<String>,
    #[serde(default = "default_list_grants_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListGrantsResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub grants: Vec<SessionGrantView>,
}
/// Ask a person to decide between 2-4 options, one recommended.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AskDecisionParams {
    #[schemars(length(min = 1, max = 2000))]
    pub question: String,
    /// 2-4 options; exactly one has recommended=true.
    #[schemars(extend("minItems" = 2))]
    #[schemars(extend("maxItems" = 4))]
    pub options: Vec<DecisionOptionModel>,
    /// The facts the person needs, in your words.
    #[serde(default)]
    #[schemars(length(max = 4000))]
    pub context: String,
    /// WI ref, if any.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// How long it stays answerable: default 24 h, at most 7 d. Past it the decision reads `expired`, with no implied answer.
    #[serde(default = "default_ask_decision_params_ttl_seconds")]
    #[schemars(range(min = 60, max = 604800))]
    pub ttl_seconds: i64,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionResult {
    pub decision: DecisionView,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerDecisionParams {
    /// The decision id (dec_…).
    pub id: String,
    /// The label of the option chosen.
    pub label: String,
    #[serde(default)]
    #[schemars(length(max = 2000))]
    pub note: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDecisionsParams {
    #[serde(default)]
    pub state: Option<ListDecisionsParamsState>,
    /// WI ref.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// identity_ref of the asker, e.g. agent:session:…
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub asker: Option<String>,
    #[serde(default = "default_list_decisions_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListDecisionsResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub decisions: Vec<DecisionView>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WithdrawDecisionParams {
    /// The decision id (dec_…).
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnswerSessionParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// The option's number on the dialog's menu.
    #[serde(default)]
    #[schemars(schema_with = "optional_answer_session_params_option")]
    pub option: Option<i64>,
    /// Or: text of the option's label (case-insensitive; must match one option only).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub label: Option<String>,
    /// Refuse unless the dialog still asks exactly this question, so an answer meant for one dialog never lands on the next. Pass `approval.question` from the screen or list you read.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub expect_question: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// Which option was chosen on which dialog, and whether it went away.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionAnswerResult {
    /// The id the caller named, as given.
    pub id: String,
    pub engine_session_id: String,
    pub question: String,
    pub kind: String,
    pub chosen: SessionApprovalOption,
    /// The dialog was gone from the screen afterwards. False: it still showed when the engine stopped looking — read the screen.
    pub dismissed: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionScreenParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// Also return this many lines that scrolled off the top of the screen (oldest first), for a reply or a command taller than the screen.
    #[serde(default)]
    #[schemars(range(min = 0, max = 2000))]
    pub scrollback_lines: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionLastReplyParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// How many of the last replies (1-20).
    #[serde(default = "default_session_last_reply_params_n")]
    #[schemars(range(min = 1, max = 20))]
    pub n: i64,
}
/// The last assistant messages of the agent conversation a session runs.
///
/// Read from the agent's own transcript (Claude Code or Codex JSONL), so a
/// reply is whole and current — not cut at the screen's edge or mangled by
/// redraws. `basis` says how the conversation was found: `session-id` /
/// `resume-id` (named on the command line), `engine-id` (a Claude Code
/// session the engine started), or `cwd` (the newest transcript in the
/// session's directory since it started — a guess when two agents share a
/// directory). Empty `messages` with `detail` set means it could not be read.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionLastReplyResult {
    /// The id the caller named, as given.
    pub id: String,
    pub engine_session_id: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub agent: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub conversation_id: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub basis: Option<String>,
    /// The transcript file the replies came from.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub transcript: Option<String>,
    /// Oldest first; the last one is the latest reply.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub messages: Vec<SessionReply>,
    /// Why there are no messages, when there are none.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionEconomicsParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
}
/// What a session's agent transcript cost (C16).
///
/// Read from the same transcript `session.last_reply` finds, whole rather
/// than tailed. `context_tokens` is the latest turn's window, not a sum.
/// `usage_measured` is true only when that number came from recorded usage
/// (Claude Code); Klaudia and Codex are estimated at four characters per
/// token. `cost_usd` stays null: this product has no price table, and a zero
/// would say the session was free. `commits` and `files_changed` stay null
/// until a worktree is measured; zero would say the tree was clean.
/// `detail` set means the transcript could not be read, and the counts are
/// then the empty reading, not a claim that the session did nothing.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionEconomicsResult {
    /// The id the caller named, as given.
    pub id: String,
    pub engine_session_id: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub agent: Option<String>,
    /// The named Claude Code account the session ran under, so usage can be totalled per subscription. Null is the default account.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub account: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub conversation_id: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub basis: Option<String>,
    /// The transcript file the numbers came from.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub transcript: Option<String>,
    #[serde(default)]
    pub turns: i64,
    #[serde(default)]
    pub context_tokens: i64,
    #[serde(default)]
    pub peak_context_tokens: i64,
    #[serde(default)]
    pub compactions: i64,
    /// Bytes of tool result the model was shown, by tool name.
    #[serde(default)]
    pub tool_result_bytes: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub reads_repeated: i64,
    /// Gaps between consecutive timestamped turns.
    #[serde(default)]
    pub wall_clock_count: i64,
    #[serde(default)]
    pub wall_clock_max_ms: i64,
    #[serde(default)]
    pub wall_clock_mean_ms: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub usage_measured: bool,
    /// Commits on the worktree since the session started; null when not measured.
    #[serde(default)]
    pub commits: Option<i64>,
    /// Files the worktree differs in; null when not measured.
    #[serde(default)]
    pub files_changed: Option<i64>,
    /// Null: no price table is configured, so cost is not claimed.
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// The digest line, e.g. `96 turns, 2 compactions`.
    #[serde(default)]
    pub budget_line: String,
    /// Why the transcript was not read, when it was not.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// One session's digest, read from the engine row rather than its transcript.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionDigestParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// The model's context window, in tokens. Given, `context_pct` is `context_tokens` over this window; omitted, the percentage is null. There is no model table.
    #[serde(default)]
    #[schemars(schema_with = "optional_session_digest_params_context_window")]
    pub context_window: Option<i64>,
}
/// What an overseer reads instead of screen, last reply and git log (C3).
///
/// One engine `get_session`. Numbers come from that row's `economics` when
/// the engine sends one, and are null when it does not — this read does not
/// meter a transcript and does not open a transcript file. `last_reply_excerpt`
/// is likewise only what the row already carries, clipped to 120 characters.
/// `context_pct` needs the caller to name `context_window`. `budget_line` is
/// the same line `session.economics` builds, from those numbers.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionDigestResult {
    /// The id the caller named, as given.
    pub id: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub name: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub activity: Option<String>,
    /// The blocked report (reason and items), or null when not blocked.
    #[serde(default)]
    pub blocked: Option<SessionBlocked>,
    /// The row's last-reply excerpt, at most 120 characters.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub last_reply_excerpt: Option<String>,
    /// The session's newest `session.report` (WI-2039).
    #[serde(default)]
    pub latest_report: Option<super::caught_up::SessionReportView>,
    #[serde(default)]
    pub turns: Option<i64>,
    #[serde(default)]
    pub context_tokens: Option<i64>,
    #[serde(default)]
    pub peak_context_tokens: Option<i64>,
    #[serde(default)]
    pub compactions: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
    /// Whether context came from recorded usage; null when unmeasured.
    #[serde(default)]
    pub usage_measured: Option<bool>,
    /// context_tokens / context_window as a rounded percent, or null when no window was given or no context was measured.
    #[serde(default)]
    pub context_pct: Option<i64>,
    /// The digest line, e.g. `96 turns, 61% ctx, 2 compactions`.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub budget_line: Option<String>,
    /// When the session's activity last changed.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub updated_at: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionWaitParams {
    /// Session id, in either form: Vogt's `ses_…` id or the engine's session UUID (`engine_session_id` in session.list; the only id an unlinked session has).
    pub id: String,
    /// `ready` (default): until the program is at its prompt — or needs a person (a permission dialog, a blocked report) or has exited, which also end the wait; `exited`: until the process ends; `any_change`: until its activity, blocked state or liveness changes at all.
    #[serde(default = "default_session_wait_params_until")]
    pub until: SessionWaitParamsUntil,
    /// Seconds to wait at most (1-600). Answers `timeout` then.
    #[serde(default = "default_session_wait_params_timeout_s")]
    #[schemars(range(min = 1, max = 600))]
    pub timeout_s: i64,
    /// True restores the full screen. False (default) returns the digest row plus an 8-line screen tail.
    #[serde(default)]
    pub screen: bool,
}
/// Why a wait ended, and the screen at that moment.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionWaitResult {
    /// The id the caller named, as given.
    pub id: String,
    pub engine_session_id: String,
    /// ready / awaiting-approval / blocked / exited / changed / timeout / hibernated (the session is hibernated: nothing will happen until it is woken, so the wait returns at once with its kept screen; session.wake or session.input wakes it).
    pub outcome: String,
    /// True when the outcome is what `until` asked for.
    pub matched: bool,
    pub waited_ms: i64,
    #[serde(default)]
    pub screen: Option<SessionScreenResult>,
    #[serde(default)]
    pub digest: Option<SessionDigest>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub screen_tail: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionChangesParams {
    /// Return deltas published after this seq. 0 reads the retained ring. Pass the previous result's `next_cursor`.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub since: i64,
    /// Sessions to read, each `ses_…` or an engine UUID. Empty reads every session. There is no supervised-by-me filter yet.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub ids: Vec<String>,
    /// How many deltas at most (1-500).
    #[serde(default = "default_session_changes_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
}
/// Deltas since a cursor, and where to read from next.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionChangesResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub changes: Vec<SessionChange>,
    /// The seq to pass as `since` on the next read.
    pub next_cursor: i64,
    /// True when the ring no longer holds everything after `since`; the page starts at the oldest event still retained.
    pub truncated: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionWaitAnyParams {
    /// Sessions to wait on, each `ses_…` or an engine UUID. Empty waits on every session. There is no supervised-by-me filter yet.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub ids: Vec<String>,
    /// Kinds that end the wait: ready, approval, exited, errored, blocked, unblocked, hibernated, woken. Empty uses the engine's default (ready, blocked, approval, exited, errored).
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub until: Vec<String>,
    /// Ignore deltas at or before this seq, so a replayed page is not waited on twice. Omit to accept the next match. `now` skips everything already published and waits only for a change that lands after this call starts; the result's `next_cursor` is that head when nothing new arrives.
    #[serde(default)]
    pub cursor: SessionWaitAnyParamsCursor,
    /// Seconds to wait at most (1-600). Answers `timeout` then.
    #[serde(default = "default_session_wait_any_params_timeout_s")]
    #[schemars(range(min = 1, max = 600))]
    pub timeout_s: i64,
}
/// Why a wait across sessions ended.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionWaitAnyResult {
    /// `changed` when a delta matched, else `timeout`.
    pub outcome: String,
    #[serde(default)]
    pub change: Option<SessionChange>,
    /// The seq to pass as `cursor` on the next wait.
    pub next_cursor: i64,
}
/// An agent's own report that it cannot go on without a person.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportBlockedParams {
    /// The blocked session, in either id form. Omit it from inside a session Vogt started (its token names the session); otherwise pass `$VOGT_ENGINE_SESSION_ID`.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub id: Option<String>,
    /// What you are blocked on, for the person: what they must decide or do before you can go on.
    #[schemars(length(min = 1, max = 2000))]
    pub blocker: String,
    /// The concrete things the person has to do, one per entry.
    #[serde(default)]
    #[schemars(extend("maxItems" = 20))]
    #[schemars(extend("x-keep-default" = true))]
    pub items: Vec<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionBlockedResult {
    pub id: String,
    pub engine_session_id: String,
    /// The report now on the session; null once cleared.
    #[serde(default)]
    pub blocked: Option<SessionBlocked>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportUnblockedParams {
    /// The session, in either id form; omit from inside a session Vogt started.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub id: Option<String>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCliListParams {
    /// Also ask npm for each package's latest version; the engine caches the answer for an hour and leaves it out when npm does not answer.
    #[serde(default)]
    pub upstream: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCliListResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub tools: Vec<AgentCliRow>,
    #[serde(default)]
    pub installer_present: bool,
    /// What the engine said, when it could not be asked. An empty tool list is returned rather than an error, so an outage never reads as 'no agent CLIs'.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCliUpdateParams {
    /// A tool from agent_cli.list, e.g. claude-code or codex.
    pub tool: String,
    /// An exact version (2.1.261), `image` for the copy baked into the pod, or a dist-tag (`latest`, `stable`) the deployment has opted into.
    pub version: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentCliUpdateResult {
    pub tool: String,
    pub requested: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub active_version: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub source: Option<String>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub tools: Vec<AgentCliRow>,
}
/// How the agent activity index is narrowed. Every field narrows.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentActivitySearchParams {
    /// Text to find, case-insensitively, in a call's tool name, its redacted one-line summary, or its error excerpt.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub q: Option<String>,
    /// A service tag, e.g. github, docker, komodo, infisical.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub service: Option<String>,
    /// An exact tool name, e.g. Bash or exec_command.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub tool: Option<String>,
    /// Only calls that failed.
    #[serde(default)]
    pub errors_only: bool,
    /// Inclusive lower bound on the call's time.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub since: Option<String>,
    /// Project slug: calls made in its root or anywhere under it.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// A Vogt session (`ses_…`), an engine session id, or an agent's own conversation id.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    #[serde(default = "default_agent_activity_search_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentActivitySearchResult {
    pub events: Vec<AgentActivityEvent>,
    /// Rows on this page; `total == limit` means there may be more.
    pub total: i64,
    /// Pass back as `offset` for the next page.
    #[serde(default)]
    pub next_offset: Option<i64>,
    /// When the index last finished a sweep; null if it never has.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub indexed_at: Option<String>,
    /// Why an empty answer is empty, where that is not 'there are none': indexing not configured, or never run.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// Which agent conversations to summarise. Every field narrows.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentActivitySummaryParams {
    /// A Vogt session (`ses_…`), an engine session id, or an agent's own conversation id.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    /// Project slug: conversations with calls in or under its root.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Count only calls at or after this time.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub since: Option<String>,
    #[serde(default = "default_agent_activity_summary_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
    /// Also meter each conversation's transcript (turns, context, compactions). Off by default: a summary must not read every transcript on the page.
    #[serde(default)]
    pub economics: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentActivitySummaryResult {
    pub sessions: Vec<AgentActivitySessionSummary>,
    /// Rows on this page; `total == limit` means there may be more.
    pub total: i64,
    #[serde(default)]
    pub next_offset: Option<i64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub indexed_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryListParams {
    #[serde(default = "default_history_list_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
    /// `summary` omits each row's command; `full` (default) keeps it.
    #[serde(default = "default_history_list_params_mode")]
    pub mode: HistoryListParamsMode,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HistoryListResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub sessions: Vec<HistorySessionRow>,
    /// What the engine said, when it could not be asked. Empty history is returned rather than an error, so an outage never reads as 'no history' .
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchOutputParams {
    /// Search terms. Plain words, ANDed together — not FTS query syntax.
    pub q: String,
    #[serde(default = "default_search_output_params_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: i64,
    /// Scan running sessions' output too, not just the archive, so output that has not been archived yet is still found. Live hits are flagged `live: true`.
    #[serde(default = "default_search_output_params_include_live")]
    pub include_live: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchOutputResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub matches: Vec<HistoryOutputMatch>,
    /// What the engine said, when it could not be asked. Empty history is returned rather than an error, so an outage never reads as 'no history' .
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
}

fn default_start_session_params_permission_mode() -> StartSessionParamsPermissionMode {
    StartSessionParamsPermissionMode::Default
}

fn optional_start_session_params_account(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"pattern": "^[a-z0-9][a-z0-9_-]{0,31}$", "type": "string"}, { "type": "null" }] })
}

fn optional_start_session_params_resume(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"pattern": "^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,127}$", "type": "string"}, { "type": "null" }] })
}

fn default_start_session_params_role() -> StartSessionParamsRole {
    StartSessionParamsRole::Worker
}

fn default_list_sessions_params_order() -> ListSessionsParamsOrder {
    ListSessionsParamsOrder::Started
}

fn default_list_sessions_params_mode() -> ListSessionsParamsMode {
    ListSessionsParamsMode::Digest
}

fn default_list_sessions_params_stall_after_minutes() -> i64 {
    10
}

fn default_list_sessions_params_limit() -> i64 {
    50
}

fn default_session_input_params_submit() -> bool {
    true
}

fn default_session_input_params_confirm() -> bool {
    true
}

fn default_session_input_params_wake_timeout_s() -> i64 {
    120
}

fn default_session_input_result_delivery() -> String {
    "unconfirmed".to_string()
}

fn default_sweep_sessions_params_mode() -> SweepSessionsParamsMode {
    SweepSessionsParamsMode::Digest
}

fn default_sweep_sessions_params_limit() -> i64 {
    50
}

fn default_sweep_sessions_params_stall_after_minutes() -> i64 {
    10
}

fn optional_bind_session_work_params_work_item(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"type": "string"}, { "type": "null" }] })
}

fn default_request_grant_params_kind() -> RequestGrantParamsKind {
    RequestGrantParamsKind::Credential
}

fn default_request_grant_params_uses() -> RequestGrantParamsUses {
    RequestGrantParamsUses::Once
}

fn default_request_grant_params_ttl_seconds() -> i64 {
    3600
}

fn default_list_grants_params_limit() -> i64 {
    50
}

fn default_ask_decision_params_ttl_seconds() -> i64 {
    86400
}

fn default_list_decisions_params_limit() -> i64 {
    50
}

fn optional_answer_session_params_option(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"maximum": 99, "minimum": 1, "type": "integer"}, { "type": "null" }] })
}

fn default_session_last_reply_params_n() -> i64 {
    1
}

fn optional_session_digest_params_context_window(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"exclusiveMinimum": 0, "type": "integer"}, { "type": "null" }] })
}

fn default_session_wait_params_until() -> SessionWaitParamsUntil {
    SessionWaitParamsUntil::Ready
}

fn default_session_wait_params_timeout_s() -> i64 {
    120
}

fn default_session_changes_params_limit() -> i64 {
    100
}

fn default_session_wait_any_params_timeout_s() -> i64 {
    120
}

fn default_agent_activity_search_params_limit() -> i64 {
    50
}

fn default_agent_activity_summary_params_limit() -> i64 {
    50
}

fn default_history_list_params_limit() -> i64 {
    50
}

fn default_history_list_params_mode() -> HistoryListParamsMode {
    HistoryListParamsMode::Full
}

fn default_search_output_params_limit() -> i64 {
    20
}

fn default_search_output_params_include_live() -> bool {
    true
}
