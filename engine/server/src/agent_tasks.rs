use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use axum::{
    extract::{Path as AxumPath, State},
    Json,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{Duration, OffsetDateTime, Time};
use tokio::sync::Notify;
use uuid::Uuid;
use vogt_engine_contract::ActivityState;

use crate::{
    activity::strip_ansi,
    app::AppState,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    gates::{self, GateRecord, GateSpec, GateState, SteerItem, AUTO_APPROVE_ACTOR},
    prompt_files,
    pty::{Session, SessionSpec},
    push::{NotificationKind, PushManager},
    sessions::SessionRegistry,
    workflow_engine::{
        map_conclusion, HttpWorkflowProvider, ProviderRunState, WorkflowEngineConfig,
        WorkflowProvider,
    },
};
use futures_util::StreamExt;

const TASKS_FILE: &str = "agent-tasks.json";
/// The prefix a run prints to ask for a push notification (FR-E7). Renamed to
/// `VOGT_NOTIFY:` with the product (#203); the engine still recognises the
/// legacy `MYDEVENV2_NOTIFY:` below, so a client emitting either prefix — and a
/// task definition created under the old default — keeps working.
const DEFAULT_NOTIFY_PHRASE: &str = "VOGT_NOTIFY:";
/// The historical notify prefix, still accepted. A task configured with either
/// default is matched against *both*, so the downstream web client can switch
/// its default to `VOGT_NOTIFY:` without stranding runs that still print the
/// old one (or vice versa).
const LEGACY_NOTIFY_PHRASE: &str = "MYDEVENV2_NOTIFY:";

/// The one mechanism that produces findings today (FR-E7). Named rather than
/// implied so that a second one arriving has to say it is a second one.
const FINDING_SOURCE_NOTIFY_PHRASE: &str = "notify-phrase";

/// The sentinel a run prints to declare itself a deliberate no-op (#291). A run
/// that prints this and exits cleanly is `skipped`, not `succeeded` — the
/// difference between "there was nothing to do" and "I did it", which a `why`
/// input reading these conclusions must be able to tell apart.
const SKIP_SENTINEL: &str = "VOGT_SKIP:";

/// The prefix a run prints to report what it cost (#291): the remainder is
/// either a JSON object (`{"total_usd":0.4,"input_tokens":1200}`) or a bare
/// dollar amount (`$0.40`). Absent output means an unknown cost, recorded as
/// `null`.
const COST_SENTINEL: &str = "VOGT_COST:";

/// Git's empty-tree object, the base a diff is measured from when a run's
/// workspace had no commit yet — the "everything this run added" case for a
/// fresh repo.
const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The default re-prompt budget for `output_schema` validation (#291): enough
/// to give a CLI a couple of chances to fix its findings, few enough that a
/// CLI that cannot produce the shape is not driven forever.
const DEFAULT_OUTPUT_SCHEMA_MAX_RETRIES: u32 = 2;

fn default_output_schema_max_retries() -> u32 {
    DEFAULT_OUTPUT_SCHEMA_MAX_RETRIES
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: Uuid,
    pub name: String,
    pub prompt: String,
    pub schedule: AgentTaskSchedule,
    /// Event triggers this task listens on (#290), each independently enabled.
    /// Additive to the schedule: a task may fire on the clock and on core-state
    /// events at once. Empty for a task that only runs manually or on a
    /// schedule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<AgentTaskTrigger>,
    /// How many runs of this task may be in flight at once (#290). A trigger
    /// (or the scheduler) that would exceed this waits — it logs and drops the
    /// fire rather than starting an overlapping run — so a burst of events
    /// cannot spawn a storm of PTYs against one workspace. Defaults to one.
    #[serde(default = "default_task_concurrency")]
    pub concurrency: u32,
    pub status: AgentTaskStatus,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub context: Option<String>,
    /// The Vogt subject this task's runs are about (FR-E7).
    ///
    /// Held as the names a person types — a project slug, a work-item ref —
    /// and never resolved here: the engine has no view of Vogt's registry,
    /// and a task that stored a Vogt id would be storing something it cannot
    /// check, cannot re-resolve, and cannot explain to anyone reading the
    /// file. Vogt resolves the name at collection time, where the registry
    /// is, and a binding naming something that instance does not have is
    /// simply not collected.
    ///
    /// The binding does not change what the run does. It says which subject
    /// the run is *about*, so that what the run reports can become evidence
    /// against that subject instead of evaporating into a push notification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vogt_project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vogt_work_item: Option<String>,
    /// Approval points this task's runs hold at (#289). Each is a first-class
    /// step with options; a run opens them in order at the prompt boundaries
    /// its CLI reaches, and holds the PTY at each until it is answered or fails
    /// closed to `blocked`. Empty for a task that never pauses for a decision.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateSpec>,
    /// The one audited bypass (#289): with it set, a run answers its own gates
    /// with each gate's `approve` option instead of waiting for a human. It is
    /// recorded on every gate it resolves (actor `auto-approve`), because a run
    /// that approved its own gates is a fact a reader must be able to see. A
    /// gate with no `approve` option still fails closed under it.
    #[serde(default)]
    pub auto_approve: bool,
    /// An optional JSON Schema the run's findings block must satisfy (#291).
    /// When set, the engine reads the findings the CLI writes — a fenced
    /// ```json block in its output, or the file named by `output_file` — and
    /// validates them; a mismatch re-prompts the CLI up to
    /// `output_schema_max_retries` times before the run is recorded
    /// `partially-succeeded`. When unset, findings stay free-text (the notify
    /// phrase, today's behaviour) and nothing is validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<serde_json::Value>,
    /// Where the findings block lives, when it is a file rather than a fenced
    /// block in the CLI's output. A path relative to the run's workspace. Only
    /// consulted when `output_schema` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    /// How many times a run may be re-prompted to fix its findings before it is
    /// recorded `partially-succeeded` (#291). Only meaningful with
    /// `output_schema`. Defaults to [`DEFAULT_OUTPUT_SCHEMA_MAX_RETRIES`].
    #[serde(default = "default_output_schema_max_retries")]
    pub output_schema_max_retries: u32,
    /// The branch this task's runs are bound to and work on (#283). Names the
    /// branch the conclusion's final sha and diff stats are read from; when
    /// unset the engine uses whatever branch is checked out in the workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// When set, this task's runs are handed to an external workflow engine
    /// (#293) instead of a PTY: the engine owns the agent loop, and Vogt tracks
    /// the run as a polled observation with a typed conclusion. `None` keeps the
    /// ordinary PTY path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_engine: Option<WorkflowEngineConfig>,
    #[serde(default)]
    pub notify_on_start: bool,
    #[serde(default = "default_notify_phrase")]
    pub notify_on_phrase: Option<String>,
    /// When the session prints a transient-failure phrase (429, rate limit,
    /// overloaded), write a retry keystroke back into the PTY after a
    /// backoff instead of just notifying. On by default — it's a safe,
    /// bounded automation sibling to the phrase watcher above.
    #[serde(default = "default_true")]
    pub auto_retry_on_rate_limit: bool,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub next_run: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_run: Option<OffsetDateTime>,
    #[serde(default)]
    pub run_count: u64,
    #[serde(default)]
    pub runs: Vec<AgentTaskRun>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AgentTaskSchedule {
    Manual,
    Interval { minutes: u64 },
    Daily { times: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskStatus {
    Active,
    Paused,
}

/// The default per-task concurrency cap (#290): a task runs one run at a time
/// unless it says otherwise. One is the safe default — a trigger that fires
/// twice while a run is in flight does not start a second overlapping run and
/// spawn two PTYs against the same workspace.
const DEFAULT_TASK_CONCURRENCY: u32 = 1;

fn default_task_concurrency() -> u32 {
    DEFAULT_TASK_CONCURRENCY
}

/// One event trigger a task listens on (#290).
///
/// Independent of the `schedule`: a task may fire on the clock, on demand, and
/// on any number of core-state events at once. Each trigger is separately
/// enabled — a disabled trigger keeps its filter so toggling it back on does
/// not lose the configuration — and each *firing* creates a normal run, with
/// the trigger recorded on it so #291's conclusion path is unchanged and a
/// reader can see why the run happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskTrigger {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(flatten)]
    pub spec: TriggerSpec,
}

/// A PR's aggregate check status a [`TriggerSpec::ForgePrChecks`] fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeCheckStatus {
    /// Either terminal status — green or red.
    #[default]
    Any,
    Green,
    Red,
}

/// The five trigger kinds (#290), tagged by `kind` so the wire spelling is the
/// dotted-free kebab a client filters on (`work-transition`, `observation-new`,
/// `drift-proposed`, `forge-pr-checks`, `api`).
///
/// A note on what the engine can actually match, because a filter that looks
/// right and matches nothing is the failure this whole area guards against.
/// The engine has no view of Vogt's registry — it matches against the fields
/// the core event *carries on the feed*, and nothing else. `work-transition`
/// and `drift-proposed` ride core events that fire today; the engine reads
/// their `summary` (`to`, `ref`, and — via the additive core enrichment #290
/// landed — `project`, `kind`, `labels`). `observation-new` and
/// `forge-pr-checks` are matched here against the event shape the core will
/// emit for them; until the core bridges its observed store onto the feed they
/// fire only from a synthetic event, which is exactly what the tests drive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TriggerSpec {
    /// A work item entered `to_state`. Optional filters narrow it to a project
    /// slug, an item kind, a label, or one specific item ref. The matched
    /// item's ref is bound to the run automatically (its `vogt_work_item`), so
    /// what the run reports files against the item that triggered it.
    WorkTransition {
        to_state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        /// One specific item ref (e.g. `WI-7`); when set only that item's
        /// transitions fire.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work_item: Option<String>,
    },
    /// A new observed subject of `observation_kind` (e.g. `forge.issue`,
    /// `forge.pull_request`, `marker`) appeared, optionally scoped to a
    /// project.
    ObservationNew {
        observation_kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// A drift proposal was raised for a project (optionally scoped).
    DriftProposed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
    },
    /// A PR linked to a work item (#284) reached a terminal check status. When
    /// `work_item` is set only that item's PR fires; the item is bound to the
    /// run.
    ForgePrChecks {
        #[serde(default)]
        status: ForgeCheckStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        work_item: Option<String>,
    },
    /// An explicit programmatic fire. It carries no filter and never matches a
    /// core event — it is armed so a run started through the API records `api`
    /// rather than `manual`, and so the trigger list can say "this task is
    /// driven by API" beside the event triggers.
    Api,
}

impl TriggerSpec {
    /// The kebab-case trigger kind — the audit token and the wire spelling.
    pub fn kind_str(&self) -> &'static str {
        match self {
            TriggerSpec::WorkTransition { .. } => "work-transition",
            TriggerSpec::ObservationNew { .. } => "observation-new",
            TriggerSpec::DriftProposed { .. } => "drift-proposed",
            TriggerSpec::ForgePrChecks { .. } => "forge-pr-checks",
            TriggerSpec::Api => "api",
        }
    }
}

/// What a matched trigger decided about a run (#290): which trigger fired, the
/// item to bind the run to (when the event named one), and a human line for the
/// audit so `why` can say "task ran because WI-7 entered ready".
#[derive(Debug, Clone, PartialEq)]
struct TriggerFire {
    trigger_kind: &'static str,
    bind_work_item: Option<String>,
    description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskRun {
    pub id: Uuid,
    pub task_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    pub trigger: AgentTaskRunTrigger,
    /// Why this run started, when it was a trigger (#290): the trigger kind and
    /// the core event that fired it. `None` for a manual or scheduled run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_detail: Option<RunTriggerDetail>,
    pub session_id: Uuid,
    pub session_name: String,
    pub prompt_file: String,
    pub context_file: String,
    #[serde(default = "default_run_status")]
    pub status: AgentTaskRunStatus,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub summary: Option<String>,
    /// What this run said it found (FR-E7).
    ///
    /// Until now the notify phrase produced a push notification and nothing
    /// else: if the phone was off, the finding was gone. Recording it on the
    /// run makes it durable, and makes it something a bound task's subject
    /// can be given as evidence. The push still fires — this is "not only as
    /// push notifications", not "instead of".
    #[serde(default)]
    pub findings: Vec<AgentTaskFinding>,
    /// The approval gates this run opened, in the order it opened them, each
    /// carrying its own terminal state (#289). This is the audit trail: an
    /// answered gate names who chose which option, a blocked gate names why it
    /// failed closed, and both survive a restart so a sweep can read what a run
    /// stopped for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<GateRecord>,
    /// The typed verdict, once the run has one (#291). `None` while running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<AgentTaskRunOutcome>,
    /// The durable conclusion record (#291), written the moment the run ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<AgentTaskRunConclusion>,
    /// How many times the CLI was re-prompted to fix its findings against the
    /// task's `output_schema` (#291). 0 when no schema was set or it validated
    /// first try.
    #[serde(default)]
    pub retries: u32,
    /// Whether the findings validated against the task's `output_schema`:
    /// `Some(true)` passed, `Some(false)` failed after the re-prompt budget,
    /// `None` when no schema was required. Drives the `partially-succeeded`
    /// verdict without the completion path re-parsing the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_ok: Option<bool>,
    /// The branch checked out in the run's workspace when it started, or the
    /// task's declared `branch` (#283/#291). The bound branch the conclusion
    /// reports against. `None` when the workspace is not a git repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The branch tip when the run started — the base the conclusion's diff
    /// stats measure against. `None` when the workspace had no commit yet (a
    /// fresh repo), in which case the diff is measured from the empty tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
}

/// One thing a run reported about itself.
///
/// `source` exists because there is exactly one producer today and there
/// will not always be: a reader of an old run should be able to tell what
/// mechanism put the line there without inferring it from the shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskFinding {
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    pub text: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskRunTrigger {
    Manual,
    Scheduled,
    /// A core-state event trigger fired the run (#290). Which trigger and which
    /// event are on the run's `trigger_detail`.
    Event,
    /// An explicit programmatic fire through the API (#290).
    Api,
}

/// The audit of why a run started (#290): which trigger fired it and, when a
/// core event did, that event's kind, entity, and sequence. This is what lets a
/// `why` reading a run say "task ran because WI-7 entered ready at seq 4102"
/// instead of only "a run happened". Absent (`None`) for a manual or scheduled
/// run, which has no originating trigger beyond its own name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunTriggerDetail {
    /// The trigger kind, kebab: `work-transition`, `observation-new`,
    /// `drift-proposed`, `forge-pr-checks`, or `api`.
    pub trigger_kind: String,
    /// The core event kind that fired it, when a core event did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_kind: Option<String>,
    /// The core entity the event was about, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    /// The core event's sequence number, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_seq: Option<i64>,
    /// A one-line human account: `WI-7 entered ready`.
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskRunStatus {
    Running,
    Completed,
    Errored,
}

fn default_run_status() -> AgentTaskRunStatus {
    AgentTaskRunStatus::Running
}

/// The typed verdict of a finished run (#291), a strict superset of the
/// exit-code story the `status` field kept before it.
///
/// * `Succeeded` — a clean exit with nothing that downgrades it.
/// * `Failed` — a non-zero exit that is not one of the more specific verdicts.
/// * `PartiallySucceeded` — the run's findings never validated against the
///   task's `output_schema` after the re-prompt budget was spent.
/// * `Skipped` — the run declared itself a no-op by printing the skip sentinel.
/// * `Blocked` — the run stopped at a #289 approval gate that failed closed;
///   the fail-closed guarantee is what makes this distinct from `Failed`.
///
/// The variants are ordered by the precedence [`resolve_outcome`] applies:
/// a blocked gate outranks a schema miss, which outranks a self-declared skip,
/// which outranks the bare exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskRunOutcome {
    Succeeded,
    Failed,
    PartiallySucceeded,
    Skipped,
    Blocked,
}

impl AgentTaskRunOutcome {
    /// The kebab-case wire token, e.g. `partially-succeeded`. Named so the SSE
    /// event and the API agree on one spelling without a second serializer.
    fn as_str(self) -> &'static str {
        match self {
            AgentTaskRunOutcome::Succeeded => "succeeded",
            AgentTaskRunOutcome::Failed => "failed",
            AgentTaskRunOutcome::PartiallySucceeded => "partially-succeeded",
            AgentTaskRunOutcome::Skipped => "skipped",
            AgentTaskRunOutcome::Blocked => "blocked",
        }
    }
}

/// Files/insertions/deletions a run left on its bound branch, `git diff
/// --numstat` reduced to three numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    pub files: u32,
    pub insertions: u64,
    pub deletions: u64,
}

/// What a run cost, parsed from a `VOGT_COST:` line the CLI printed. Every
/// field is optional because different CLIs report different subsets — a run
/// that reported only a dollar amount has `total_usd` and nothing else, and a
/// run that reported nothing has no [`RunCost`] at all (the conclusion's `cost`
/// is `None`, i.e. `null`, not a zeroed record that would read as "free").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunCost {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

/// The durable record a run leaves behind (#291): the one place a reader, a
/// sweep, or a future `why` input can find what a run actually did, instead of
/// an exit code and a scrollback that a restart may have dropped.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskRunConclusion {
    #[serde(with = "time::serde::rfc3339")]
    pub started: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub finished: OffsetDateTime,
    pub duration_ms: u64,
    pub outcome: AgentTaskRunOutcome,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub retries: u32,
    /// The bound branch the run worked on (#283's subject, seen from the
    /// engine as the branch checked out in the run's workspace, or the task's
    /// declared `branch`). `None` when the workspace is not a git repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The branch tip when the run finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_sha: Option<String>,
    /// The branch point captured when the run started, the base the diff stats
    /// are measured against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diffstat: Option<DiffStat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<RunCost>,
    #[serde(default)]
    pub findings: Vec<AgentTaskFinding>,
}

#[derive(Debug, Deserialize)]
pub struct AgentTaskCreate {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub schedule: Option<AgentTaskSchedule>,
    #[serde(default)]
    pub triggers: Option<Vec<AgentTaskTrigger>>,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub vogt_project: Option<String>,
    #[serde(default)]
    pub vogt_work_item: Option<String>,
    #[serde(default)]
    pub gates: Option<Vec<GateSpec>>,
    #[serde(default)]
    pub auto_approve: Option<bool>,
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub output_file: Option<String>,
    #[serde(default)]
    pub output_schema_max_retries: Option<u32>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub workflow_engine: Option<WorkflowEngineConfig>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub notify_on_start: Option<bool>,
    #[serde(default)]
    pub notify_on_phrase: Option<String>,
    #[serde(default)]
    pub auto_retry_on_rate_limit: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct AgentTaskUpdate {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub schedule: Option<AgentTaskSchedule>,
    #[serde(default)]
    pub triggers: Option<Vec<AgentTaskTrigger>>,
    #[serde(default)]
    pub concurrency: Option<u32>,
    #[serde(default)]
    pub command: Option<Vec<String>>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: Option<Vec<(String, String)>>,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub vogt_project: Option<String>,
    #[serde(default)]
    pub vogt_work_item: Option<String>,
    #[serde(default)]
    pub gates: Option<Vec<GateSpec>>,
    #[serde(default)]
    pub auto_approve: Option<bool>,
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    #[serde(default)]
    pub output_file: Option<String>,
    #[serde(default)]
    pub output_schema_max_retries: Option<u32>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub workflow_engine: Option<WorkflowEngineConfig>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub notify_on_start: Option<bool>,
    #[serde(default)]
    pub notify_on_phrase: Option<String>,
    #[serde(default)]
    pub auto_retry_on_rate_limit: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PromptArtifactCleanupReq {
    #[serde(default = "default_prompt_artifact_keep_latest_runs")]
    pub keep_latest_runs_per_task: usize,
}

fn default_prompt_artifact_keep_latest_runs() -> usize {
    10
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentTaskStore {
    #[serde(default)]
    tasks: Vec<AgentTask>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptArtifactStats {
    pub task_dir_count: u64,
    pub prompt_file_count: u64,
    pub context_file_count: u64,
    /// Briefs written for sessions rather than for task runs. Counted apart
    /// from `prompt_file_count` so the task numbers still mean tasks.
    pub session_prompt_file_count: u64,
    pub total_bytes: u64,
    pub orphan_task_dir_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PromptArtifactCleanup {
    pub removed_task_dir_count: u64,
    pub removed_prompt_file_count: u64,
    pub removed_context_file_count: u64,
    pub removed_session_prompt_file_count: u64,
    pub removed_bytes: u64,
}

#[derive(Default)]
struct PromptArtifactRemovalTally {
    task_dirs: u64,
    prompt_files: u64,
    context_files: u64,
    session_prompts: u64,
    bytes: u64,
}

/// Why a run is being started, and what it should be bound to (#290).
///
/// One value carries everything the start path needs that is not the task
/// itself: the trigger to record, the audit detail naming the core event that
/// fired it (`None` for manual/scheduled), and an optional item ref to bind
/// this one run to. A manual Run Now and a `work.transition` fire reach
/// [`AgentTaskRegistry::start_run`] the same way, differing only in this.
struct RunOrigin {
    trigger: AgentTaskRunTrigger,
    detail: Option<RunTriggerDetail>,
    bind_work_item: Option<String>,
}

impl RunOrigin {
    fn manual() -> Self {
        Self {
            trigger: AgentTaskRunTrigger::Manual,
            detail: None,
            bind_work_item: None,
        }
    }

    fn scheduled() -> Self {
        Self {
            trigger: AgentTaskRunTrigger::Scheduled,
            detail: None,
            bind_work_item: None,
        }
    }

    fn api() -> Self {
        Self {
            trigger: AgentTaskRunTrigger::Api,
            detail: Some(RunTriggerDetail {
                trigger_kind: "api".to_string(),
                event_kind: None,
                event_id: None,
                event_seq: None,
                description: "started via API".to_string(),
            }),
            bind_work_item: None,
        }
    }

    /// A run fired by a matched core event: the trigger fire says which trigger
    /// matched and what to bind, and the event itself names the kind, entity,
    /// and sequence the audit records.
    fn event(fire: TriggerFire, event_kind: &str, event_id: &str, event_seq: i64) -> Self {
        Self {
            trigger: AgentTaskRunTrigger::Event,
            detail: Some(RunTriggerDetail {
                trigger_kind: fire.trigger_kind.to_string(),
                event_kind: Some(event_kind.to_string()),
                event_id: (!event_id.is_empty()).then(|| event_id.to_string()),
                event_seq: Some(event_seq),
                description: fire.description,
            }),
            bind_work_item: fire.bind_work_item,
        }
    }
}

/// The live controls for one in-flight run (#289): the handle its steer and
/// gate answers reach, and the wake-up the orchestrator waits on. Kept out of
/// the persisted task store because none of it outlives the process — a steer
/// queued for a run whose engine restarted has nothing to be delivered to, and
/// a gate left open by a restart fails closed on reconcile.
struct RunControl {
    session: Arc<Session>,
    /// Text queued to reach the PTY at the next prompt boundary, in order.
    steer: Mutex<VecDeque<SteerItem>>,
    /// Woken when a steer is queued or a gate is answered, so the orchestrator
    /// acts at once rather than on its next poll tick.
    notify: Arc<Notify>,
}

pub struct AgentTaskRegistry {
    path: PathBuf,
    prompt_dir: PathBuf,
    sessions: Arc<SessionRegistry>,
    push: Arc<PushManager>,
    bus: EventBus,
    tasks: Mutex<Vec<AgentTask>>,
    executing: Mutex<HashSet<Uuid>>,
    /// Live run controls, keyed by run id. An entry exists only while the
    /// orchestrator for that run is running; it removes its own on exit.
    runs: Mutex<HashMap<Uuid, Arc<RunControl>>>,
}

impl AgentTaskRegistry {
    pub fn new(
        state_dir: &Path,
        sessions: Arc<SessionRegistry>,
        push: Arc<PushManager>,
        bus: EventBus,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let prompt_dir = prompt_files::prompt_root(state_dir);
        std::fs::create_dir_all(&prompt_dir)?;
        let path = state_dir.join(TASKS_FILE);
        let tasks = load_tasks(&path)?;
        let registry = Self {
            path,
            prompt_dir,
            sessions,
            push,
            bus,
            tasks: Mutex::new(tasks),
            executing: Mutex::new(HashSet::new()),
            runs: Mutex::new(HashMap::new()),
        };
        registry.reconcile_run_history()?;
        registry.normalize_startup_schedule()?;
        Ok(registry)
    }

    pub fn spawn_scheduler(self: &Arc<Self>) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            loop {
                registry.run_due_once().await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });
    }

    pub fn spawn_run_watcher(self: &Arc<Self>, bus: EventBus) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(ServerEvent::SessionKilled { id, exit_code }) => {
                        if let Err(e) = registry.complete_run_for_session(id, exit_code) {
                            tracing::warn!(
                                session = %id,
                                error = %e,
                                "failed to update agent task run outcome"
                            );
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "agent task run watcher lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Subscribe to the engine's event bus and start runs from matching core
    /// events (#290).
    ///
    /// This is the subscription the feature turns on, and it is a subscription
    /// rather than a poll: the front door already follows vogt-core's event
    /// cursor once, in one place, and republishes each change onto this bus as
    /// `VogtChanged` (`vogt_core::spawn_event_follower`, FR-U10). This watcher
    /// reads that bus exactly as `spawn_run_watcher` above reads it for session
    /// deaths — no second cursor, no second poller, no core credential of its
    /// own. It inherits the follower's properties: silent when no core is
    /// configured, and starting from the core's current head so a boot replays
    /// nothing.
    pub fn spawn_trigger_watcher(self: &Arc<Self>, bus: EventBus) {
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            let mut rx = bus.subscribe();
            loop {
                match rx.recv().await {
                    Ok(ServerEvent::VogtChanged {
                        kind,
                        entity_id,
                        seq,
                        summary,
                        ..
                    }) => {
                        registry
                            .dispatch_core_event(&kind, &entity_id, seq, &summary)
                            .await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // A burst outran this consumer. What was lost is trigger
                        // fires, and the no-storm rule already accepts a missed
                        // fire (a paused engine misses them too); losing a few
                        // under a burst is the same trade, not a reason to stop.
                        tracing::warn!(skipped, "agent task trigger watcher lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Match one core event against every active task's enabled triggers and
    /// start a run for each task that matched (#290).
    ///
    /// Events are handled one at a time — the watcher awaits each start before
    /// the next event — so this is where the no-storm and concurrency rules
    /// bite: a task already at its cap has its fire logged and dropped by
    /// `start_run`, never requeued. A single event fires a given task at most
    /// once, even if two of its triggers would match, so one transition cannot
    /// multiply into several runs of the same task.
    async fn dispatch_core_event(
        self: &Arc<Self>,
        kind: &str,
        entity_id: &str,
        seq: i64,
        summary: &serde_json::Value,
    ) {
        let fires: Vec<(Uuid, TriggerFire)> = {
            let tasks = self.tasks.lock();
            let mut fires = Vec::new();
            let mut fired: HashSet<Uuid> = HashSet::new();
            for task in tasks.iter().filter(|t| t.status == AgentTaskStatus::Active) {
                for trigger in task.triggers.iter().filter(|t| t.enabled) {
                    if let Some(fire) = spec_matches(&trigger.spec, kind, entity_id, summary) {
                        if fired.insert(task.id) {
                            fires.push((task.id, fire));
                        }
                        break;
                    }
                }
            }
            fires
        };

        for (task_id, fire) in fires {
            let origin = RunOrigin::event(fire, kind, entity_id, seq);
            if let Err(e) = self.start_run(task_id, origin).await {
                tracing::warn!(
                    task = %task_id,
                    error = %e,
                    "agent-task trigger run failed to start"
                );
            }
        }
    }

    pub fn list(&self) -> Vec<AgentTask> {
        let mut out = self.tasks.lock().clone();
        out.sort_by_key(|t| t.created_at);
        out
    }

    pub fn get(&self, id: Uuid) -> Result<AgentTask> {
        self.tasks
            .lock()
            .iter()
            .find(|t| t.id == id)
            .cloned()
            .ok_or(ApiError::NotFound)
    }

    pub fn create(&self, req: AgentTaskCreate) -> Result<AgentTask> {
        let name = clean_required("name", req.name)?;
        let prompt = clean_required("prompt", req.prompt)?;
        let schedule = req.schedule.unwrap_or(AgentTaskSchedule::Manual);
        validate_schedule(&schedule)?;
        let triggers = clean_triggers(req.triggers)?;
        let concurrency = clean_concurrency(req.concurrency);
        let gates = clean_gates(req.gates)?;
        let now = OffsetDateTime::now_utc();
        let status = if req.enabled == Some(false) {
            AgentTaskStatus::Paused
        } else {
            AgentTaskStatus::Active
        };
        let next_run = if status == AgentTaskStatus::Active {
            compute_next_run(&schedule, now)?
        } else {
            None
        };
        let task = AgentTask {
            id: Uuid::new_v4(),
            name,
            prompt,
            schedule,
            triggers,
            concurrency,
            status,
            command: clean_command(req.command),
            cwd: clean_optional(req.cwd),
            env: req.env.unwrap_or_default(),
            context: clean_optional(req.context),
            vogt_project: clean_optional(req.vogt_project),
            vogt_work_item: clean_optional(req.vogt_work_item),
            gates,
            auto_approve: req.auto_approve.unwrap_or(false),
            output_schema: clean_output_schema(req.output_schema),
            output_file: clean_optional(req.output_file),
            output_schema_max_retries: req
                .output_schema_max_retries
                .unwrap_or(DEFAULT_OUTPUT_SCHEMA_MAX_RETRIES),
            branch: clean_optional(req.branch),
            workflow_engine: clean_workflow_engine(req.workflow_engine),
            notify_on_start: req.notify_on_start.unwrap_or(false),
            notify_on_phrase: clean_notify_phrase(req.notify_on_phrase),
            auto_retry_on_rate_limit: req.auto_retry_on_rate_limit.unwrap_or(true),
            next_run,
            last_run: None,
            run_count: 0,
            runs: vec![],
            created_at: now,
            updated_at: now,
        };
        let mut tasks = self.tasks.lock();
        tasks.push(task.clone());
        self.save_locked(&tasks)?;
        Ok(task)
    }

    pub fn update(&self, id: Uuid, req: AgentTaskUpdate) -> Result<AgentTask> {
        let mut tasks = self.tasks.lock();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(ApiError::NotFound)?;
        if let Some(name) = req.name {
            task.name = clean_required("name", name)?;
        }
        if let Some(prompt) = req.prompt {
            task.prompt = clean_required("prompt", prompt)?;
        }
        if let Some(schedule) = req.schedule {
            validate_schedule(&schedule)?;
            task.schedule = schedule;
        }
        if let Some(triggers) = req.triggers {
            task.triggers = clean_triggers(Some(triggers))?;
        }
        if let Some(concurrency) = req.concurrency {
            task.concurrency = clean_concurrency(Some(concurrency));
        }
        if req.command.is_some() {
            task.command = clean_command(req.command);
        }
        if req.cwd.is_some() {
            task.cwd = clean_optional(req.cwd);
        }
        if let Some(env) = req.env {
            task.env = env;
        }
        if req.context.is_some() {
            task.context = clean_optional(req.context);
        }
        // An empty string unbinds, the same way it clears `cwd` and
        // `context`: there has to be a way to say "this task is nobody's" ,
        // and a separate verb for it would be a second way to spell one edit.
        if req.vogt_project.is_some() {
            task.vogt_project = clean_optional(req.vogt_project);
        }
        if req.vogt_work_item.is_some() {
            task.vogt_work_item = clean_optional(req.vogt_work_item);
        }
        if let Some(gates) = req.gates {
            task.gates = clean_gates(Some(gates))?;
        }
        if let Some(auto_approve) = req.auto_approve {
            task.auto_approve = auto_approve;
        }
        if req.output_schema.is_some() {
            task.output_schema = clean_output_schema(req.output_schema);
        }
        if req.output_file.is_some() {
            task.output_file = clean_optional(req.output_file);
        }
        if let Some(retries) = req.output_schema_max_retries {
            task.output_schema_max_retries = retries;
        }
        if req.branch.is_some() {
            task.branch = clean_optional(req.branch);
        }
        if req.workflow_engine.is_some() {
            task.workflow_engine = clean_workflow_engine(req.workflow_engine);
        }
        if let Some(enabled) = req.enabled {
            task.status = if enabled {
                AgentTaskStatus::Active
            } else {
                AgentTaskStatus::Paused
            };
        }
        if let Some(v) = req.notify_on_start {
            task.notify_on_start = v;
        }
        if req.notify_on_phrase.is_some() {
            task.notify_on_phrase = clean_notify_phrase(req.notify_on_phrase);
        }
        if let Some(v) = req.auto_retry_on_rate_limit {
            task.auto_retry_on_rate_limit = v;
        }
        let now = OffsetDateTime::now_utc();
        task.next_run = if task.status == AgentTaskStatus::Active {
            compute_next_run(&task.schedule, now)?
        } else {
            None
        };
        task.updated_at = now;
        let out = task.clone();
        self.save_locked(&tasks)?;
        Ok(out)
    }

    pub fn pause(&self, id: Uuid) -> Result<AgentTask> {
        self.set_enabled(id, false)
    }

    pub fn resume(&self, id: Uuid) -> Result<AgentTask> {
        self.set_enabled(id, true)
    }

    pub fn delete(&self, id: Uuid) -> Result<bool> {
        let mut tasks = self.tasks.lock();
        let before = tasks.len();
        tasks.retain(|t| t.id != id);
        if tasks.len() == before {
            return Err(ApiError::NotFound);
        }
        self.save_locked(&tasks)?;
        remove_path_recursive(
            &self.task_prompt_dir(id),
            &mut PromptArtifactRemovalTally::default(),
        )?;
        Ok(true)
    }

    pub fn prompt_artifact_stats(&self) -> Result<PromptArtifactStats> {
        let tasks = self.tasks.lock().clone();
        let task_ids: HashSet<Uuid> = tasks.iter().map(|task| task.id).collect();
        let mut stats = PromptArtifactStats {
            task_dir_count: 0,
            prompt_file_count: 0,
            context_file_count: 0,
            session_prompt_file_count: 0,
            total_bytes: 0,
            orphan_task_dir_count: 0,
        };

        match std::fs::read_dir(&self.prompt_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() {
                        continue;
                    }
                    let name = entry.file_name();
                    if name == prompt_files::SESSION_SUBDIR {
                        accumulate_session_prompt_stats(&entry.path(), &mut stats)?;
                        continue;
                    }
                    stats.task_dir_count += 1;
                    let task_id = name.to_string_lossy();
                    let is_orphan = Uuid::parse_str(&task_id)
                        .ok()
                        .map(|id| !task_ids.contains(&id))
                        .unwrap_or(true);
                    if is_orphan {
                        stats.orphan_task_dir_count += 1;
                    }
                    accumulate_prompt_dir_stats(&entry.path(), &mut stats)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "read prompt artifact dir {}: {e}",
                    self.prompt_dir.display()
                )));
            }
        }

        Ok(stats)
    }

    pub fn cleanup_prompt_artifacts(
        &self,
        keep_latest_runs_per_task: usize,
    ) -> Result<PromptArtifactCleanup> {
        let tasks = self.tasks.lock().clone();
        let task_ids: HashSet<Uuid> = tasks.iter().map(|task| task.id).collect();
        let mut tally = PromptArtifactRemovalTally::default();

        match std::fs::read_dir(&self.prompt_dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if !entry.file_type()?.is_dir() {
                        continue;
                    }
                    let path = entry.path();
                    if entry.file_name() == prompt_files::SESSION_SUBDIR {
                        self.cleanup_session_prompt_dir(&path, &mut tally)?;
                        continue;
                    }
                    let task_id = Uuid::parse_str(&entry.file_name().to_string_lossy()).ok();
                    let Some(task_id) = task_id else {
                        remove_path_recursive(&path, &mut tally)?;
                        continue;
                    };
                    if !task_ids.contains(&task_id) {
                        remove_path_recursive(&path, &mut tally)?;
                        continue;
                    }
                    let task = tasks
                        .iter()
                        .find(|task| task.id == task_id)
                        .ok_or_else(|| {
                            ApiError::Internal("task disappeared during cleanup".into())
                        })?;
                    cleanup_task_prompt_dir(&path, task, keep_latest_runs_per_task, &mut tally)?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ApiError::Internal(format!(
                    "read prompt artifact dir {}: {e}",
                    self.prompt_dir.display()
                )));
            }
        }

        Ok(PromptArtifactCleanup {
            removed_task_dir_count: tally.task_dirs,
            removed_prompt_file_count: tally.prompt_files,
            removed_context_file_count: tally.context_files,
            removed_session_prompt_file_count: tally.session_prompts,
            removed_bytes: tally.bytes,
        })
    }

    /// Session briefs are retained by liveness, not by count: the registry is
    /// the authority on whether a session still exists, so
    /// `keep_latest_runs_per_task` has nothing to say here. A session's own
    /// deletion removes its brief; this pass exists for the ones a crash or a
    /// restart left behind, whose sessions the registry has already forgotten.
    fn cleanup_session_prompt_dir(
        &self,
        path: &Path,
        tally: &mut PromptArtifactRemovalTally,
    ) -> Result<()> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let entry_path = entry.path();
            if !file_type.is_file() {
                // Nothing writes directories here; whatever it is, it is not
                // a session brief.
                remove_path_recursive(&entry_path, tally)?;
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let live = prompt_files::session_id_from_prompt_file(name.as_ref())
                .is_some_and(|id| self.sessions.get(id).is_ok());
            if !live {
                remove_session_prompt_with_tally(&entry_path, tally)?;
            }
        }
        Ok(())
    }

    pub async fn run_now(self: &Arc<Self>, id: Uuid) -> Result<AgentTaskRun> {
        self.start_run(id, RunOrigin::manual())
            .await?
            .ok_or_else(|| ApiError::Conflict("task did not start".into()))
    }

    /// Run a task now, recording it as an explicit `api` fire (#290) rather than
    /// a human's `manual` Run Now. The task must have an `api` trigger armed —
    /// programmatic firing is opt-in, so a task nobody meant to be driven this
    /// way is refused by name rather than run.
    pub async fn run_via_api(self: &Arc<Self>, id: Uuid) -> Result<AgentTaskRun> {
        let task = self.get(id)?;
        if !task
            .triggers
            .iter()
            .any(|t| t.enabled && matches!(t.spec, TriggerSpec::Api))
        {
            return Err(ApiError::Conflict(
                "task has no enabled 'api' trigger; add one to allow programmatic runs".into(),
            ));
        }
        self.start_run(id, RunOrigin::api())
            .await?
            .ok_or_else(|| ApiError::Conflict("task did not start".into()))
    }

    async fn run_due_once(self: &Arc<Self>) {
        let now = OffsetDateTime::now_utc();
        let due: Vec<Uuid> = self
            .tasks
            .lock()
            .iter()
            .filter(|t| t.status == AgentTaskStatus::Active)
            .filter_map(|t| t.next_run.filter(|next| *next <= now).map(|_| t.id))
            .collect();

        for id in due {
            if let Err(e) = self.start_run(id, RunOrigin::scheduled()).await {
                tracing::warn!(task = %id, error = %e, "scheduled agent task failed to start");
                let _ = self.advance_next_run(id);
            }
        }
    }

    async fn start_run(
        self: &Arc<Self>,
        id: Uuid,
        origin: RunOrigin,
    ) -> Result<Option<AgentTaskRun>> {
        {
            let mut executing = self.executing.lock();
            if !executing.insert(id) {
                return self.defer_or_conflict(id, origin.trigger, "task is already starting");
            }
        }

        let result = self.start_run_inner(id, origin).await;
        self.executing.lock().remove(&id);
        result
    }

    /// What a start that cannot proceed *now* does, by trigger (#290). A
    /// scheduled fire re-arms its next tick; an event fire logs and drops — the
    /// no-storm rule, a fire that cannot start does not spin retrying; a manual
    /// or api fire is a 409 the caller sees. None of these retry.
    fn defer_or_conflict(
        &self,
        id: Uuid,
        trigger: AgentTaskRunTrigger,
        reason: &str,
    ) -> Result<Option<AgentTaskRun>> {
        match trigger {
            AgentTaskRunTrigger::Scheduled => {
                let _ = self.advance_next_run(id);
                Ok(None)
            }
            AgentTaskRunTrigger::Event => {
                tracing::info!(
                    task = %id,
                    reason,
                    "agent-task trigger fired but could not start now; dropped without retry"
                );
                Ok(None)
            }
            AgentTaskRunTrigger::Manual | AgentTaskRunTrigger::Api => {
                Err(ApiError::Conflict(reason.to_string()))
            }
        }
    }

    async fn start_run_inner(
        self: &Arc<Self>,
        id: Uuid,
        origin: RunOrigin,
    ) -> Result<Option<AgentTaskRun>> {
        let trigger = origin.trigger;
        let mut task = self.get(id)?;
        // An event or scheduled fire on a paused task is a no-op, not an error:
        // the trigger stays armed for when the task is resumed.
        if task.status != AgentTaskStatus::Active
            && matches!(
                trigger,
                AgentTaskRunTrigger::Scheduled | AgentTaskRunTrigger::Event
            )
        {
            return Ok(None);
        }
        // The per-task concurrency cap (#290). Counting live sessions, not the
        // `executing` guard above, so a cap of two genuinely allows two runs in
        // flight while a cap of one preserves the old one-at-a-time behaviour.
        let cap = task.concurrency.max(1) as usize;
        if self.running_run_count(&task) >= cap {
            return self.defer_or_conflict(id, trigger, "task is at its concurrency cap");
        }
        // A trigger fire binds the run to the item the event named (#290), for
        // this run only — the persisted task binding is untouched. This is what
        // makes "run, bound to that item" true: the run's VOGT_WORK_ITEM and its
        // prompt name the item that triggered it.
        if let Some(work_item) = origin.bind_work_item.clone() {
            task.vogt_work_item = Some(work_item);
        }

        // A task with a workflow-engine backend (#293) takes an entirely
        // separate path: no prompt expansion, no PTY, no session. The run is
        // handed to the engine and tracked by polling. Everything below — the
        // command build, the session spawn, the orchestrator — is the PTY path
        // and stays untouched when `workflow_engine` is None.
        if let Some(we) = task.workflow_engine.clone() {
            return self.start_workflow_engine_run(id, origin, task, we).await;
        }

        let run_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        let (prompt_file, context_file) = self.write_prompt_files(&task, run_id, now)?;
        let prompt_file_display = prompt_file.to_string_lossy().into_owned();
        let context_file_display = context_file.to_string_lossy().into_owned();
        let command = expand_command(
            task.command.clone().unwrap_or_else(default_command),
            &task,
            run_id,
            &prompt_file_display,
            &context_file_display,
        );
        let mut env = task.env.clone();
        env.push(("MYDEVENV2_AGENT_TASK_ID".to_string(), task.id.to_string()));
        env.push((
            "MYDEVENV2_AGENT_TASK_RUN_ID".to_string(),
            run_id.to_string(),
        ));
        env.push((
            prompt_files::PROMPT_FILE_ENV.to_string(),
            prompt_file_display.clone(),
        ));
        env.push((
            "MYDEVENV2_AGENT_TASK_CONTEXT_FILE".to_string(),
            context_file_display.clone(),
        ));
        // The binding travels into the run so the agent inside can name the
        // same subject Vogt will file the run's findings against. It is not
        // a credential and grants nothing: a task run has no Vogt token
        // (unlike a session, FR-S10), so an agent that wants to write to
        // Vogt still has to be given one the ordinary way.
        if let Some(project) = task.vogt_project.as_deref() {
            env.push(("VOGT_PROJECT".to_string(), project.to_string()));
        }
        if let Some(work_item) = task.vogt_work_item.as_deref() {
            env.push(("VOGT_WORK_ITEM".to_string(), work_item.to_string()));
        }

        let session_name = format!("[Task] {}", task.name);
        let session = self.sessions.create(SessionSpec {
            name: session_name.clone(),
            command: Some(command),
            cwd: task.cwd.clone(),
            env: Some(env),
            // A task run has already written its own prompt file above, with
            // the context and run history a session brief knows nothing
            // about, and has just named it in `env`. Asking the registry for
            // a second one would write the brief twice.
            prompt: None,
            model: None,
            effort: None,
            cols: Some(100),
            rows: Some(30),
            scrollback_bytes: None,
        })?;

        // Capture the run's starting git position in its own workspace, so the
        // conclusion can report the final sha of the bound branch and the diff
        // stats against where the run began (#283/#291). A workspace that is
        // not a repo yields `None` for both, and the conclusion simply omits
        // the git half — the feature degrades to "no git story here" rather
        // than failing the run.
        let (start_branch, start_sha) = git_run_start(&session.cwd, task.branch.as_deref());

        let run = AgentTaskRun {
            id: run_id,
            task_id: task.id,
            started_at: now,
            trigger,
            trigger_detail: origin.detail.clone(),
            session_id: session.id,
            session_name,
            prompt_file: prompt_file_display,
            context_file: context_file_display,
            status: AgentTaskRunStatus::Running,
            completed_at: None,
            exit_code: None,
            summary: None,
            findings: vec![],
            gates: vec![],
            outcome: None,
            conclusion: None,
            retries: 0,
            schema_ok: None,
            branch: start_branch,
            base_sha: start_sha,
        };

        {
            let mut tasks = self.tasks.lock();
            let task = tasks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ApiError::NotFound)?;
            task.last_run = Some(now);
            task.run_count = task.run_count.saturating_add(1);
            if matches!(trigger, AgentTaskRunTrigger::Scheduled) {
                task.next_run = compute_next_run(&task.schedule, now)?;
            }
            task.updated_at = now;
            task.runs.push(run.clone());
            if task.runs.len() > 50 {
                let extra = task.runs.len() - 50;
                task.runs.drain(0..extra);
            }
            self.save_locked(&tasks)?;
        }

        // Announce a triggered run on the stream the moment its session exists
        // (#290), so a client — and `why` — can see the run and the trigger
        // that caused it without waiting for the conclusion. Manual and
        // scheduled runs carry no originating event and emit nothing here.
        if let Some(detail) = origin.detail.as_ref() {
            self.bus.publish(ServerEvent::TaskRunTriggered {
                task_id: task.id,
                run_id: run.id,
                session_id: session.id,
                trigger_kind: detail.trigger_kind.clone(),
                event_kind: detail.event_kind.clone(),
                event_id: detail.event_id.clone(),
                event_seq: detail.event_seq,
            });
        }

        if task.notify_on_start {
            let data = json!({
                "kind": "agent-task-started",
                "task_id": task.id.to_string(),
                "run_id": run.id.to_string(),
                "session_id": session.id.to_string(),
                "url": format!("/#/t/{}", session.id),
            });
            let body = format!("Tap to open {}", task.name);
            let _ = self
                .push
                .notify(
                    NotificationKind::AgentTaskStarted,
                    "Scheduled agent started",
                    &body,
                    data,
                )
                .await;
        }

        if let Some(phrase) = task
            .notify_on_phrase
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            spawn_phrase_watcher(
                Arc::clone(self),
                Arc::clone(&session),
                task.name.clone(),
                task.id,
                run.id,
                phrase,
                session.subscribe(),
            );
        }

        if task.auto_retry_on_rate_limit {
            spawn_retry_watcher(
                Arc::clone(&self.push),
                Arc::clone(&session),
                task.name.clone(),
                session.subscribe(),
            );
        }

        // The orchestrator holds the PTY at each declared gate and drains the
        // steer queue between rounds (#289). Spawned for every run, not only
        // gated ones, because steering is a general ability a run acquires the
        // moment it exists — a task with no gates can still be redirected
        // mid-flight.
        let control = Arc::new(RunControl {
            session: Arc::clone(&session),
            steer: Mutex::new(VecDeque::new()),
            notify: Arc::new(Notify::new()),
        });
        self.runs.lock().insert(run.id, Arc::clone(&control));
        spawn_run_orchestrator(
            Arc::clone(self),
            control,
            run.id,
            task.gates.clone(),
            task.auto_approve,
        );

        // When the task declares an `output_schema`, a watcher validates the
        // findings block the CLI writes and re-prompts it on a mismatch (#291).
        // Spawned only for schema tasks — a task with no schema keeps today's
        // free-text findings and this watcher never exists.
        if let Some(schema) = task.output_schema.clone() {
            spawn_schema_watcher(
                Arc::clone(self),
                Arc::clone(&session),
                run.id,
                schema,
                task.output_schema_max_retries,
                task.output_file.clone(),
                session.subscribe(),
            );
        }

        Ok(Some(run))
    }

    /// The workflow-engine path (#293): hand a run to an external engine and
    /// track it by polling, instead of spawning a PTY. Reached only when the
    /// task carries a [`WorkflowEngineConfig`]; the PTY path above is untouched.
    ///
    /// Every provider failure — an unreadable token file, an unreachable engine,
    /// a non-terminal deadline — records the run *errored* with a written reason
    /// and returns `Ok`. It never panics and never propagates an error that
    /// would abort the scheduler: an absent engine is a run that failed, not a
    /// Vogt that fell over.
    async fn start_workflow_engine_run(
        self: &Arc<Self>,
        id: Uuid,
        origin: RunOrigin,
        task: AgentTask,
        we: WorkflowEngineConfig,
    ) -> Result<Option<AgentTaskRun>> {
        let trigger = origin.trigger;
        let run_id = Uuid::new_v4();
        // A workflow run has no PTY session; this id backs nothing and only
        // gives the run and its concluded event a stable, unique handle.
        let session_id = Uuid::new_v4();
        let now = OffsetDateTime::now_utc();
        // Write the prompt/context files for provenance, exactly as the PTY path
        // does, so a workflow run leaves the same durable brief behind.
        let (prompt_file, context_file) = self.write_prompt_files(&task, run_id, now)?;

        let mut run = AgentTaskRun {
            id: run_id,
            task_id: task.id,
            started_at: now,
            trigger,
            trigger_detail: origin.detail.clone(),
            session_id,
            session_name: format!("[Workflow] {}", task.name),
            prompt_file: prompt_file.to_string_lossy().into_owned(),
            context_file: context_file.to_string_lossy().into_owned(),
            status: AgentTaskRunStatus::Running,
            completed_at: None,
            exit_code: None,
            summary: None,
            findings: vec![],
            gates: vec![],
            outcome: None,
            conclusion: None,
            retries: 0,
            schema_ok: None,
            branch: None,
            base_sha: None,
        };

        // Resolve the engine token from its file, if configured (#293). A set
        // but unreadable file is a provider error, not a boot failure: the run
        // is recorded errored with the reason, the same non-fatal contract the
        // unreachable-engine case keeps.
        let token = match read_workflow_token(we.token_file.as_deref()) {
            Ok(token) => token,
            Err(reason) => {
                self.record_workflow_run(id, trigger, &origin.detail, &run, now)?;
                self.record_workflow_errored(
                    task.id,
                    run_id,
                    session_id,
                    now,
                    OffsetDateTime::now_utc(),
                    &format!("workflow engine token unavailable: {reason}"),
                );
                return Ok(Some(self.reload_run(id, run_id).unwrap_or(run)));
            }
        };

        let provider = HttpWorkflowProvider::from_config(&we, token);
        let goal = build_workflow_goal(&task);
        let repo_ref = we.repo_ref.clone();

        match provider.create_run(&goal, repo_ref.as_deref()).await {
            Ok(provider_run) => {
                run.summary = Some(match &provider_run.url {
                    Some(url) => format!("workflow engine run {} ({url})", provider_run.run_id),
                    None => format!("workflow engine run {}", provider_run.run_id),
                });
                self.record_workflow_run(id, trigger, &origin.detail, &run, now)?;
                // Track the run off its live SSE event stream (#293 inc-2),
                // degrading to the poller when the stream is absent or breaks.
                // DEFERRED to a later slice: bridging #289 gates onto the
                // engine's own gates, and collecting the #283/#284 checkpoint
                // checkpoint branches the engine produces as run observations.
                spawn_workflow_tracker(
                    Arc::clone(self),
                    provider,
                    provider_run.run_id,
                    task.id,
                    run_id,
                    session_id,
                    now,
                );
                Ok(Some(run))
            }
            Err(err) => {
                // Absence/unreachability is non-fatal: record the run errored
                // with the reason and return normally.
                self.record_workflow_run(id, trigger, &origin.detail, &run, now)?;
                self.record_workflow_errored(
                    task.id,
                    run_id,
                    session_id,
                    now,
                    OffsetDateTime::now_utc(),
                    &format!("workflow engine create-run failed: {err}"),
                );
                Ok(Some(self.reload_run(id, run_id).unwrap_or(run)))
            }
        }
    }

    /// Persist a freshly-built workflow run into its task, updating the same
    /// counters the PTY path does (last run, run count, next scheduled run) and
    /// announcing a triggered run on the stream. Written once at the start of a
    /// workflow run's life; the conclusion is written later by the poller.
    fn record_workflow_run(
        &self,
        id: Uuid,
        trigger: AgentTaskRunTrigger,
        detail: &Option<RunTriggerDetail>,
        run: &AgentTaskRun,
        now: OffsetDateTime,
    ) -> Result<()> {
        {
            let mut tasks = self.tasks.lock();
            let task = tasks
                .iter_mut()
                .find(|t| t.id == id)
                .ok_or(ApiError::NotFound)?;
            task.last_run = Some(now);
            task.run_count = task.run_count.saturating_add(1);
            if matches!(trigger, AgentTaskRunTrigger::Scheduled) {
                task.next_run = compute_next_run(&task.schedule, now)?;
            }
            task.updated_at = now;
            task.runs.push(run.clone());
            if task.runs.len() > 50 {
                let extra = task.runs.len() - 50;
                task.runs.drain(0..extra);
            }
            self.save_locked(&tasks)?;
        }
        if let Some(detail) = detail.as_ref() {
            self.bus.publish(ServerEvent::TaskRunTriggered {
                task_id: id,
                run_id: run.id,
                session_id: run.session_id,
                trigger_kind: detail.trigger_kind.clone(),
                event_kind: detail.event_kind.clone(),
                event_id: detail.event_id.clone(),
                event_seq: detail.event_seq,
            });
        }
        Ok(())
    }

    /// Write a workflow run's terminal verdict back (#293): set its status,
    /// outcome and durable conclusion, then announce `task.run.concluded` on the
    /// stream — the same event the PTY completion path emits. A run the registry
    /// no longer has, or one already concluded, is a no-op (the same tolerance
    /// the PTY path keeps).
    fn record_workflow_conclusion(
        &self,
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        outcome: AgentTaskRunOutcome,
        conclusion: AgentTaskRunConclusion,
        summary: String,
    ) {
        let finished = conclusion.finished;
        let duration_ms = conclusion.duration_ms;
        let branch = conclusion.branch.clone();
        let final_sha = conclusion.final_sha.clone();
        let diffstat = conclusion.diffstat;
        let cost_usd = conclusion.cost.as_ref().and_then(|c| c.total_usd);
        {
            let mut tasks = self.tasks.lock();
            let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) else {
                return;
            };
            let Some(run) = task.runs.iter_mut().find(|r| r.id == run_id) else {
                return;
            };
            if run.status != AgentTaskRunStatus::Running {
                return;
            }
            run.completed_at = Some(finished);
            run.status = match outcome {
                AgentTaskRunOutcome::Failed | AgentTaskRunOutcome::Blocked => {
                    AgentTaskRunStatus::Errored
                }
                _ => AgentTaskRunStatus::Completed,
            };
            run.summary = Some(summary);
            run.outcome = Some(outcome);
            run.conclusion = Some(conclusion);
            task.updated_at = finished;
            let _ = self.save_locked(&tasks);
        }
        self.bus.publish(ServerEvent::TaskRunConcluded {
            task_id,
            run_id,
            session_id,
            outcome: outcome.as_str().to_string(),
            exit_code: None,
            duration_ms,
            retries: 0,
            branch,
            final_sha,
            files_changed: diffstat.map(|d| d.files),
            insertions: diffstat.map(|d| d.insertions),
            deletions: diffstat.map(|d| d.deletions),
            cost_usd,
        });
    }

    /// Record a workflow run as errored with a written reason (#293). Builds a
    /// `Failed` conclusion carrying nothing but the timing, so a reader sees a
    /// verdict and a cause rather than a run stuck `running` forever.
    fn record_workflow_errored(
        &self,
        task_id: Uuid,
        run_id: Uuid,
        session_id: Uuid,
        started: OffsetDateTime,
        finished: OffsetDateTime,
        reason: &str,
    ) {
        let duration_ms = (finished - started).whole_milliseconds().max(0) as u64;
        let conclusion = AgentTaskRunConclusion {
            started,
            finished,
            duration_ms,
            outcome: AgentTaskRunOutcome::Failed,
            exit_code: None,
            retries: 0,
            branch: None,
            final_sha: None,
            base_sha: None,
            diffstat: None,
            cost: None,
            findings: vec![],
        };
        self.record_workflow_conclusion(
            task_id,
            run_id,
            session_id,
            AgentTaskRunOutcome::Failed,
            conclusion,
            reason.to_string(),
        );
    }

    /// Re-read one run from a task by id, for returning the post-conclusion
    /// state of a run that errored during start.
    fn reload_run(&self, task_id: Uuid, run_id: Uuid) -> Option<AgentTaskRun> {
        let tasks = self.tasks.lock();
        tasks
            .iter()
            .find(|t| t.id == task_id)?
            .runs
            .iter()
            .find(|r| r.id == run_id)
            .cloned()
    }

    /// How many of a task's runs still have a live session — the number the
    /// concurrency cap (#290) is compared against.
    fn running_run_count(&self, task: &AgentTask) -> usize {
        task.runs
            .iter()
            .filter_map(|run| self.sessions.get(run.session_id).ok())
            .filter(|session| session.exit_code().is_none())
            .count()
    }

    fn set_enabled(&self, id: Uuid, enabled: bool) -> Result<AgentTask> {
        let mut tasks = self.tasks.lock();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(ApiError::NotFound)?;
        let now = OffsetDateTime::now_utc();
        task.status = if enabled {
            AgentTaskStatus::Active
        } else {
            AgentTaskStatus::Paused
        };
        task.next_run = if enabled {
            compute_next_run(&task.schedule, now)?
        } else {
            None
        };
        task.updated_at = now;
        let out = task.clone();
        self.save_locked(&tasks)?;
        Ok(out)
    }

    fn complete_run_for_session(&self, session_id: Uuid, exit_code: Option<i32>) -> Result<()> {
        let Some(code) = exit_code else {
            return Ok(());
        };
        let completed_at = OffsetDateTime::now_utc();

        // Phase 1: find the still-running run for this session and copy out what
        // the conclusion needs, under the lock but without doing any slow work
        // while holding it.
        struct RunSnapshot {
            task_id: Uuid,
            run_id: Uuid,
            started_at: OffsetDateTime,
            base_sha: Option<String>,
            branch: Option<String>,
            blocked: bool,
            schema_ok: Option<bool>,
            retries: u32,
            findings: Vec<AgentTaskFinding>,
        }
        let snapshot = {
            let tasks = self.tasks.lock();
            let mut found = None;
            for task in tasks.iter() {
                if let Some(run) = task
                    .runs
                    .iter()
                    .rev()
                    .find(|run| run.session_id == session_id)
                {
                    if run.status != AgentTaskRunStatus::Running {
                        return Ok(());
                    }
                    found = Some(RunSnapshot {
                        task_id: task.id,
                        run_id: run.id,
                        started_at: run.started_at,
                        base_sha: run.base_sha.clone(),
                        branch: run.branch.clone(),
                        blocked: run
                            .gates
                            .iter()
                            .any(|g| matches!(g.state, GateState::Blocked { .. })),
                        schema_ok: run.schema_ok,
                        retries: run.retries,
                        findings: run.findings.clone(),
                    });
                    break;
                }
            }
            match found {
                Some(s) => s,
                None => return Ok(()),
            }
        };

        // Phase 2: the slow half — read the session's scrollback and shell out
        // to git — with no lock held.
        let session = self.sessions.get(session_id).ok();
        let scrollback = session
            .as_ref()
            .map(|s| {
                let (bytes, _) = s.snapshot();
                String::from_utf8_lossy(&strip_ansi(&bytes)).into_owned()
            })
            .unwrap_or_default();
        let cwd = session.as_ref().map(|s| s.cwd.clone());

        let skipped = scrollback.contains(SKIP_SENTINEL);
        let cost = parse_cost(&scrollback);
        let outcome = resolve_outcome(code, snapshot.blocked, snapshot.schema_ok, skipped);

        let (final_sha, diffstat) = match cwd.as_deref() {
            Some(cwd) => {
                let final_sha = git_final_sha(cwd, snapshot.branch.as_deref());
                let diffstat = final_sha
                    .as_deref()
                    .and_then(|head| git_diff_stats(cwd, snapshot.base_sha.as_deref(), head));
                (final_sha, diffstat)
            }
            None => (None, None),
        };

        let duration_ms = (completed_at - snapshot.started_at)
            .whole_milliseconds()
            .max(0) as u64;

        let conclusion = AgentTaskRunConclusion {
            started: snapshot.started_at,
            finished: completed_at,
            duration_ms,
            outcome,
            exit_code: Some(code),
            retries: snapshot.retries,
            branch: snapshot.branch.clone(),
            final_sha: final_sha.clone(),
            base_sha: snapshot.base_sha.clone(),
            diffstat,
            cost: cost.clone(),
            findings: snapshot.findings,
        };

        // Phase 3: write the verdict back, re-checking the run is still the one
        // we snapshotted and still running (a concurrent path could have
        // resolved it while git ran).
        {
            let mut tasks = self.tasks.lock();
            let Some(task) = tasks.iter_mut().find(|t| t.id == snapshot.task_id) else {
                return Ok(());
            };
            let Some(run) = task.runs.iter_mut().find(|r| r.id == snapshot.run_id) else {
                return Ok(());
            };
            if run.status != AgentTaskRunStatus::Running {
                return Ok(());
            }
            run.exit_code = Some(code);
            run.completed_at = Some(completed_at);
            run.status = if code == 0 {
                AgentTaskRunStatus::Completed
            } else {
                AgentTaskRunStatus::Errored
            };
            run.summary = Some(outcome_summary(outcome, code));
            run.outcome = Some(outcome);
            run.conclusion = Some(conclusion);
            task.updated_at = completed_at;
            self.save_locked(&tasks)?;
        }

        // Phase 4: announce the conclusion on the stream, additive to the
        // `session.killed` a client already sees for the same run.
        self.bus.publish(ServerEvent::TaskRunConcluded {
            task_id: snapshot.task_id,
            run_id: snapshot.run_id,
            session_id,
            outcome: outcome.as_str().to_string(),
            exit_code: Some(code),
            duration_ms,
            retries: snapshot.retries,
            branch: snapshot.branch,
            final_sha,
            files_changed: diffstat.map(|d| d.files),
            insertions: diffstat.map(|d| d.insertions),
            deletions: diffstat.map(|d| d.deletions),
            cost_usd: cost.and_then(|c| c.total_usd),
        });
        Ok(())
    }

    /// Record the result of validating a run's findings against its
    /// `output_schema` (#291): how many re-prompts it took and whether it ever
    /// passed. Written by the schema watcher before the run ends, so the
    /// completion path reads the verdict rather than re-parsing the stream. A
    /// run the registry no longer has is not an error — the same tolerance the
    /// finding recorder keeps.
    fn record_schema_result(&self, run_id: Uuid, retries: u32, ok: bool) {
        let now = OffsetDateTime::now_utc();
        let mut tasks = self.tasks.lock();
        let Some(task) = task_with_run_mut(&mut tasks, run_id) else {
            return;
        };
        let Some(run) = task.runs.iter_mut().find(|r| r.id == run_id) else {
            return;
        };
        run.retries = retries;
        run.schema_ok = Some(ok);
        task.updated_at = now;
        let _ = self.save_locked(&tasks);
    }

    /// Record something a run reported about itself (FR-E7).
    ///
    /// Appended rather than replaced, and never deduplicated: two identical
    /// lines from one run are two moments the agent said the same thing, and
    /// collapsing them would lose the second. A run the registry no longer
    /// has — deleted mid-run, or trimmed out of the fifty it keeps — is not
    /// an error: there is nothing to attach the finding to, and the push
    /// that follows is still worth sending.
    pub fn record_finding(&self, task_id: Uuid, run_id: Uuid, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let mut tasks = self.tasks.lock();
        let Some(task) = tasks.iter_mut().find(|task| task.id == task_id) else {
            return Ok(());
        };
        let Some(run) = task.runs.iter_mut().find(|run| run.id == run_id) else {
            return Ok(());
        };
        let now = OffsetDateTime::now_utc();
        run.findings.push(AgentTaskFinding {
            at: now,
            text: text.to_string(),
            source: FINDING_SOURCE_NOTIFY_PHRASE.to_string(),
        });
        task.updated_at = now;
        self.save_locked(&tasks)
    }

    // --- steering and gates (#289) -----------------------------------------

    /// Queue a steer for a task's live run, delivered to the PTY at the next
    /// prompt boundary (FR #289). `interrupt` sends the CLI's cancel first.
    ///
    /// Addressed to the task rather than a session so a phone that knows only
    /// "the nightly audit" can steer it without first resolving which session
    /// this run happens to be. Refused when the task has no run in flight —
    /// there is nothing to steer, and queuing for a run that will never read it
    /// would be a silent no-op dressed as a success.
    pub fn steer(
        &self,
        task_id: Uuid,
        text: String,
        interrupt: bool,
        actor: String,
        reason: Option<String>,
    ) -> Result<()> {
        let text = text.trim_end_matches(['\r', '\n']).to_string();
        if text.is_empty() && !interrupt {
            return Err(ApiError::BadRequest(
                "steer needs text, or interrupt=true".into(),
            ));
        }
        let (_run_id, control) = self.live_run(task_id)?;
        control.steer.lock().push_back(SteerItem {
            text,
            interrupt,
            actor,
            reason,
        });
        control.notify.notify_one();
        Ok(())
    }

    /// Answer a currently-open gate on a task's live run. The chosen option's
    /// input is delivered to the PTY by the orchestrator; this only records the
    /// resolution and wakes it. Fail-closed lives elsewhere — this is the human
    /// path, and the only path that can produce an approval.
    pub fn answer_gate(
        &self,
        task_id: Uuid,
        gate_id: Uuid,
        option_index: usize,
        actor: String,
        reason: Option<String>,
    ) -> Result<GateRecord> {
        let (run_id, control) = self.live_run(task_id)?;
        let record =
            self.resolve_gate_answer(run_id, gate_id, option_index, &actor, false, reason)?;
        control.notify.notify_one();
        Ok(record)
    }

    /// The in-flight run of a task and its live control, or a 409 when the task
    /// has none running.
    fn live_run(&self, task_id: Uuid) -> Result<(Uuid, Arc<RunControl>)> {
        let run_ids: Vec<Uuid> = {
            let tasks = self.tasks.lock();
            let task = tasks
                .iter()
                .find(|t| t.id == task_id)
                .ok_or(ApiError::NotFound)?;
            task.runs.iter().rev().map(|run| run.id).collect()
        };
        let controls = self.runs.lock();
        for run_id in run_ids {
            if let Some(control) = controls.get(&run_id) {
                if control.session.exit_code().is_none() {
                    return Ok((run_id, Arc::clone(control)));
                }
            }
        }
        Err(ApiError::Conflict(
            "task has no run in flight to steer".into(),
        ))
    }

    /// Record a gate answer (human or the audited bypass) and emit the event.
    /// The pure state machine refuses a resolved or out-of-range gate; this
    /// only translates its verdict into an API error, a save, and an event.
    fn resolve_gate_answer(
        &self,
        run_id: Uuid,
        gate_id: Uuid,
        option_index: usize,
        actor: &str,
        auto: bool,
        reason: Option<String>,
    ) -> Result<GateRecord> {
        let now = OffsetDateTime::now_utc();
        let mut tasks = self.tasks.lock();
        let (task_id, session_id, record) = {
            let task = task_with_run_mut(&mut tasks, run_id).ok_or(ApiError::NotFound)?;
            let task_id = task.id;
            let run_idx = task
                .runs
                .iter()
                .position(|r| r.id == run_id)
                .expect("task_with_run_mut found the run");
            let session_id = task.runs[run_idx].session_id;
            let gate = task.runs[run_idx]
                .gates
                .iter_mut()
                .find(|g| g.id == gate_id)
                .ok_or(ApiError::NotFound)?;
            gate.answer(option_index, actor, auto, now)
                .map_err(|e| match e {
                    gates::GateError::AlreadyResolved => {
                        ApiError::Conflict("gate is already resolved".into())
                    }
                    gates::GateError::UnknownOption => {
                        ApiError::BadRequest("no such gate option".into())
                    }
                })?;
            let record = gate.clone();
            task.updated_at = now;
            (task_id, session_id, record)
        };
        self.save_locked(&tasks)?;
        drop(tasks);
        let (option, outcome) = match &record.state {
            GateState::Answered {
                option_label,
                approved,
                ..
            } => (
                Some(option_label.clone()),
                if *approved { "approved" } else { "resolved" }.to_string(),
            ),
            _ => (None, "resolved".to_string()),
        };
        self.bus.publish(ServerEvent::TaskGateAnswered {
            task_id,
            run_id,
            session_id,
            gate_id,
            option,
            outcome,
            actor: actor.to_string(),
            reason,
        });
        Ok(record)
    }

    /// Open a gate on a run at a prompt boundary: persist it, notify, and emit
    /// `task.gate.opened`. Called only by the orchestrator, only when the run
    /// is at a boundary.
    fn open_gate(&self, run_id: Uuid, spec: &GateSpec) -> Result<()> {
        let now = OffsetDateTime::now_utc();
        let record = GateRecord::open(spec, now);
        let (task_id, task_name, session_id) = {
            let mut tasks = self.tasks.lock();
            let task = task_with_run_mut(&mut tasks, run_id).ok_or(ApiError::NotFound)?;
            let out = (task.id, task.name.clone());
            let run = task
                .runs
                .iter_mut()
                .find(|r| r.id == run_id)
                .expect("task_with_run_mut found the run");
            let session_id = run.session_id;
            run.gates.push(record.clone());
            task.updated_at = now;
            self.save_locked(&tasks)?;
            (out.0, out.1, session_id)
        };
        self.bus.publish(ServerEvent::TaskGateOpened {
            task_id,
            run_id,
            session_id,
            gate_id: spec.id,
            question: spec.question.clone(),
            options: spec.options.iter().map(|o| o.label.clone()).collect(),
        });
        let data = json!({
            "kind": "agent-task-gate",
            "task_id": task_id.to_string(),
            "run_id": run_id.to_string(),
            "gate_id": spec.id.to_string(),
            "session_id": session_id.to_string(),
            "url": format!("/#/t/{session_id}"),
        });
        let title = format!("{task_name} needs approval");
        let push = Arc::clone(&self.push);
        let question = spec.question.clone();
        tokio::spawn(async move {
            let _ = push
                .notify(NotificationKind::AgentTaskNotify, &title, &question, data)
                .await;
        });
        Ok(())
    }

    /// Fail one gate closed. Emits `task.gate.answered` with a `blocked`
    /// outcome exactly once — the pure `block` returns whether it was the call
    /// that resolved it, so a second interrupt or a death-after-timeout does
    /// not double-emit.
    fn block_gate(&self, run_id: Uuid, gate_id: Uuid, reason: &str) {
        let now = OffsetDateTime::now_utc();
        let mut tasks = self.tasks.lock();
        let Some(task) = task_with_run_mut(&mut tasks, run_id) else {
            return;
        };
        let task_id = task.id;
        let run_idx = task
            .runs
            .iter()
            .position(|r| r.id == run_id)
            .expect("task_with_run_mut found the run");
        let session_id = task.runs[run_idx].session_id;
        let Some(gate) = task.runs[run_idx]
            .gates
            .iter_mut()
            .find(|g| g.id == gate_id)
        else {
            return;
        };
        if !gate.block(reason, now) {
            return;
        }
        task.updated_at = now;
        let _ = self.save_locked(&tasks);
        drop(tasks);
        self.bus.publish(ServerEvent::TaskGateAnswered {
            task_id,
            run_id,
            session_id,
            gate_id,
            option: None,
            outcome: "blocked".to_string(),
            actor: "system".to_string(),
            reason: Some(reason.to_string()),
        });
    }

    /// Fail every still-open gate on a run closed — used when a run's session
    /// dies with a gate held. Fail-closed, in as many words: a session that
    /// went away with an unanswered question answered nothing.
    fn block_open_gates(&self, run_id: Uuid, reason: &str) {
        let open: Vec<Uuid> = {
            let tasks = self.tasks.lock();
            match tasks.iter().flat_map(|t| &t.runs).find(|r| r.id == run_id) {
                Some(run) => run
                    .gates
                    .iter()
                    .filter(|g| g.state.is_open())
                    .map(|g| g.id)
                    .collect(),
                None => vec![],
            }
        };
        for gate_id in open {
            self.block_gate(run_id, gate_id, reason);
        }
    }

    /// A snapshot of one gate record on a run, for the orchestrator to read the
    /// state the API may have changed underneath it.
    fn gate_record(&self, run_id: Uuid, gate_id: Uuid) -> Option<GateRecord> {
        let tasks = self.tasks.lock();
        tasks
            .iter()
            .flat_map(|t| &t.runs)
            .find(|r| r.id == run_id)?
            .gates
            .iter()
            .find(|g| g.id == gate_id)
            .cloned()
    }

    fn emit_steered(&self, run_id: Uuid, item: &SteerItem) {
        let (task_id, session_id) = {
            let tasks = self.tasks.lock();
            match tasks.iter().find_map(|t| {
                t.runs
                    .iter()
                    .find(|r| r.id == run_id)
                    .map(|r| (t.id, r.session_id))
            }) {
                Some(pair) => pair,
                None => return,
            }
        };
        self.bus.publish(ServerEvent::TaskSteered {
            task_id,
            run_id,
            session_id,
            actor: item.actor.clone(),
            interrupt: item.interrupt,
            reason: item.reason.clone(),
        });
    }

    fn remove_run_control(&self, run_id: Uuid) {
        self.runs.lock().remove(&run_id);
    }

    fn advance_next_run(&self, id: Uuid) -> Result<()> {
        let mut tasks = self.tasks.lock();
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or(ApiError::NotFound)?;
        task.next_run = compute_next_run(&task.schedule, OffsetDateTime::now_utc())?;
        task.updated_at = OffsetDateTime::now_utc();
        self.save_locked(&tasks)
    }

    fn normalize_startup_schedule(&self) -> Result<()> {
        let now = OffsetDateTime::now_utc();
        let mut changed = false;
        let mut tasks = self.tasks.lock();
        for task in tasks.iter_mut() {
            if task.status != AgentTaskStatus::Active {
                task.next_run = None;
                continue;
            }
            if match task.next_run {
                Some(next) => next <= now,
                None => true,
            } {
                task.next_run = compute_next_run(&task.schedule, now)?;
                task.updated_at = now;
                changed = true;
            }
        }
        if changed {
            self.save_locked(&tasks)?;
        }
        Ok(())
    }

    fn reconcile_run_history(&self) -> Result<()> {
        let now = OffsetDateTime::now_utc();
        let mut changed = false;
        let mut tasks = self.tasks.lock();

        for task in tasks.iter_mut() {
            let mut task_changed = false;
            for run in &mut task.runs {
                if run.status == AgentTaskRunStatus::Running
                    && self.sessions.get(run.session_id).is_err()
                {
                    run.status = AgentTaskRunStatus::Errored;
                    run.completed_at.get_or_insert(now);
                    run.summary
                        .get_or_insert_with(|| "Outcome unavailable after restart".to_string());
                    task_changed = true;
                    changed = true;
                }
                // A gate left `Open` by a restart has no orchestrator to hold
                // it and no session to answer into: fail it closed, the same
                // rule that governs a live gate. An `Open` gate that survived a
                // restart as "approved" would be the exact failure #289 exists
                // against.
                for gate in &mut run.gates {
                    if gate.state.is_open() && gate.block("engine restarted", now) {
                        task_changed = true;
                        changed = true;
                    }
                }
            }
            if task_changed {
                task.updated_at = now;
            }
        }

        if changed {
            self.save_locked(&tasks)?;
        }
        Ok(())
    }

    fn write_prompt_files(
        &self,
        task: &AgentTask,
        run_id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(PathBuf, PathBuf)> {
        let task_dir = self.prompt_dir.join(task.id.to_string());
        std::fs::create_dir_all(&task_dir)?;
        let context_file = task_dir.join("context.md");
        let context = task.context.as_deref().unwrap_or("").trim();
        std::fs::write(&context_file, context)?;

        let mut prompt = String::new();
        prompt.push_str("# Vogt Scheduled Agent Task\n\n");
        prompt.push_str(&format!("Task: {}\n", task.name));
        prompt.push_str(&format!("Task ID: {}\n", task.id));
        prompt.push_str(&format!("Run ID: {run_id}\n"));
        prompt.push_str(&format!("Run time (UTC): {now}\n"));
        if let Some(binding) = vogt_binding_line(task) {
            // Named in the prompt as well as in the environment, because the
            // agent reads the prompt and only sometimes reads its environment
            // — and a run that does not know what it is about will report
            // findings that cannot be filed against anything.
            prompt.push_str(&format!("Vogt subject: {binding}\n"));
        }
        prompt.push('\n');
        prompt.push_str("## Instructions\n\n");
        prompt.push_str(task.prompt.trim());
        prompt.push_str("\n\n## Persistent Context\n\n");
        if context.is_empty() {
            prompt.push_str("No persistent context has been saved for this task yet.\n");
        } else {
            prompt.push_str(context);
            prompt.push('\n');
        }
        prompt.push_str("\n## Recent Runs\n\n");
        if task.runs.is_empty() {
            prompt.push_str("No previous runs.\n");
        } else {
            for run in task.runs.iter().rev().take(5) {
                prompt.push_str(&format!(
                    "- {}: session {} ({})\n",
                    run.started_at, run.session_id, run.trigger
                ));
            }
        }
        if let Some(phrase) = task
            .notify_on_phrase
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            prompt.push_str("\n## Notification Hook\n\n");
            prompt.push_str("If, and only if, this run discovers something the user asked to be notified about, print one line starting with:\n\n");
            prompt.push_str("```text\n");
            prompt.push_str(phrase);
            prompt.push_str(" <short notification text>\n");
            prompt.push_str("```\n");
        }
        let prompt_file = prompt_files::write_prompt(&task_dir, run_id, &prompt)?;
        Ok((prompt_file, context_file))
    }

    fn task_prompt_dir(&self, id: Uuid) -> PathBuf {
        self.prompt_dir.join(id.to_string())
    }

    fn save_locked(&self, tasks: &[AgentTask]) -> Result<()> {
        let tmp = self.path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&AgentTaskStore {
            tasks: tasks.to_vec(),
        })
        .map_err(|e| ApiError::Internal(format!("serialize agent tasks: {e}")))?;
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl std::fmt::Display for AgentTaskRunTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentTaskRunTrigger::Manual => f.write_str("manual"),
            AgentTaskRunTrigger::Scheduled => f.write_str("scheduled"),
            AgentTaskRunTrigger::Event => f.write_str("event"),
            AgentTaskRunTrigger::Api => f.write_str("api"),
        }
    }
}

pub async fn list(State(state): State<Arc<AppState>>) -> Json<Vec<AgentTask>> {
    Json(state.agent_tasks.list())
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AgentTaskCreate>,
) -> Result<Json<AgentTask>> {
    Ok(Json(state.agent_tasks.create(req)?))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AgentTask>> {
    Ok(Json(state.agent_tasks.get(id)?))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<AgentTaskUpdate>,
) -> Result<Json<AgentTask>> {
    Ok(Json(state.agent_tasks.update(id, req)?))
}

pub async fn delete(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let removed = state.agent_tasks.delete(id)?;
    Ok(Json(json!({ "ok": removed })))
}

pub async fn pause(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AgentTask>> {
    Ok(Json(state.agent_tasks.pause(id)?))
}

pub async fn resume(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AgentTask>> {
    Ok(Json(state.agent_tasks.resume(id)?))
}

/// Query for `POST /api/agent-tasks/:id/run`. `trigger=api` records the run as
/// an explicit programmatic fire (#290) — allowed only when the task has an
/// `api` trigger armed. Omitted (or anything else) is the human Run Now, which
/// records `manual` and needs no trigger.
#[derive(Debug, Default, Deserialize)]
pub struct RunNowQuery {
    #[serde(default)]
    pub trigger: Option<String>,
}

pub async fn run_now(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    axum::extract::Query(query): axum::extract::Query<RunNowQuery>,
) -> Result<Json<AgentTaskRun>> {
    let run = if query.trigger.as_deref() == Some("api") {
        state.agent_tasks.run_via_api(id).await?
    } else {
        state.agent_tasks.run_now(id).await?
    };
    Ok(Json(run))
}

#[derive(Debug, Deserialize)]
pub struct SteerReq {
    #[serde(default)]
    pub text: String,
    /// Send the CLI's cancel (Ctrl-C) before the text.
    #[serde(default)]
    pub interrupt: bool,
    /// Who is steering, for the audit trail. Defaults to `operator`.
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn steer(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
    Json(req): Json<SteerReq>,
) -> Result<Json<serde_json::Value>> {
    let actor = clean_optional(req.actor).unwrap_or_else(|| "operator".to_string());
    state.agent_tasks.steer(
        id,
        req.text,
        req.interrupt,
        actor,
        clean_optional(req.reason),
    )?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Debug, Deserialize)]
pub struct GateAnswerReq {
    /// Index of the chosen option in the gate's declared order.
    pub option: usize,
    /// Who answered, for the audit trail. Defaults to `operator`.
    #[serde(default)]
    pub actor: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

pub async fn answer_gate(
    State(state): State<Arc<AppState>>,
    AxumPath((id, gate_id)): AxumPath<(Uuid, Uuid)>,
    Json(req): Json<GateAnswerReq>,
) -> Result<Json<GateRecord>> {
    let actor = clean_optional(req.actor).unwrap_or_else(|| "operator".to_string());
    let record = state.agent_tasks.answer_gate(
        id,
        gate_id,
        req.option,
        actor,
        clean_optional(req.reason),
    )?;
    Ok(Json(record))
}

pub async fn cleanup_prompt_artifacts(
    State(state): State<Arc<AppState>>,
    Json(req): Json<PromptArtifactCleanupReq>,
) -> Result<Json<PromptArtifactCleanup>> {
    Ok(Json(
        state
            .agent_tasks
            .cleanup_prompt_artifacts(req.keep_latest_runs_per_task)?,
    ))
}

fn load_tasks(path: &Path) -> Result<Vec<AgentTask>> {
    if !path.exists() {
        return Ok(vec![]);
    }
    let raw = std::fs::read(path)?;
    let store = serde_json::from_slice::<AgentTaskStore>(&raw)
        .map_err(|e| ApiError::Internal(format!("parse agent task store: {e}")))?;
    Ok(store.tasks)
}

fn clean_required(field: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(ApiError::BadRequest(format!("{field} must not be empty")));
    }
    Ok(value)
}

fn accumulate_prompt_dir_stats(path: &Path, stats: &mut PromptArtifactStats) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if meta.is_dir() {
            accumulate_prompt_dir_stats(&entry.path(), stats)?;
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        stats.total_bytes = stats.total_bytes.saturating_add(meta.len());
        let name = entry.file_name();
        if name.to_string_lossy() == "context.md" {
            stats.context_file_count += 1;
        } else {
            stats.prompt_file_count += 1;
        }
    }
    Ok(())
}

fn accumulate_session_prompt_stats(path: &Path, stats: &mut PromptArtifactStats) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        stats.total_bytes = stats.total_bytes.saturating_add(meta.len());
        stats.session_prompt_file_count += 1;
    }
    Ok(())
}

fn remove_session_prompt_with_tally(
    path: &Path,
    tally: &mut PromptArtifactRemovalTally,
) -> Result<()> {
    let bytes = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    match std::fs::remove_file(path) {
        Ok(()) => {
            tally.bytes = tally.bytes.saturating_add(bytes);
            tally.session_prompts += 1;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::Internal(format!(
            "remove session prompt {}: {e}",
            path.display()
        ))),
    }
}

fn cleanup_task_prompt_dir(
    path: &Path,
    task: &AgentTask,
    keep_latest_runs_per_task: usize,
    tally: &mut PromptArtifactRemovalTally,
) -> Result<()> {
    let keep: HashSet<String> = task
        .runs
        .iter()
        .rev()
        .take(keep_latest_runs_per_task)
        .map(|run| prompt_files::prompt_file_name(run.id))
        .collect();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let entry_path = entry.path();
        if file_type.is_dir() {
            remove_path_recursive(&entry_path, tally)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy() == "context.md" {
            continue;
        }
        let name = name.to_string_lossy();
        if !keep.contains(name.as_ref()) {
            remove_file_with_tally(&entry_path, &name, tally)?;
        }
    }
    Ok(())
}

fn remove_file_with_tally(
    path: &Path,
    file_name: &str,
    tally: &mut PromptArtifactRemovalTally,
) -> Result<()> {
    let bytes = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    match std::fs::remove_file(path) {
        Ok(()) => {
            tally.bytes = tally.bytes.saturating_add(bytes);
            if file_name == "context.md" {
                tally.context_files += 1;
            } else {
                tally.prompt_files += 1;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::Internal(format!(
            "remove artifact {}: {e}",
            path.display()
        ))),
    }
}

fn remove_path_recursive(path: &Path, tally: &mut PromptArtifactRemovalTally) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                remove_path_recursive(&entry.path(), tally)?;
            }
            match std::fs::remove_dir(path) {
                Ok(()) => {
                    tally.task_dirs += 1;
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(ApiError::Internal(format!(
                    "remove artifact dir {}: {e}",
                    path.display()
                ))),
            }
        }
        Ok(meta) if meta.is_file() => {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let bytes = meta.len();
            match std::fs::remove_file(path) {
                Ok(()) => {
                    tally.bytes = tally.bytes.saturating_add(bytes);
                    if name == "context.md" {
                        tally.context_files += 1;
                    } else {
                        tally.prompt_files += 1;
                    }
                    Ok(())
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(ApiError::Internal(format!(
                    "remove artifact file {}: {e}",
                    path.display()
                ))),
            }
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::Internal(format!(
            "stat artifact path {}: {e}",
            path.display()
        ))),
    }
}

/// The task owning a given run, mutably. A run id is unique across the store,
/// so the first task carrying it is the only one.
fn task_with_run_mut(tasks: &mut [AgentTask], run_id: Uuid) -> Option<&mut AgentTask> {
    tasks
        .iter_mut()
        .find(|t| t.runs.iter().any(|r| r.id == run_id))
}

/// Match one enabled trigger against a core event (#290).
///
/// Pure by design: it reads only the event's own `kind`, `entity_id`, and
/// `summary`, never the registry — the engine has no view of Vogt's registry,
/// so a filter it cannot satisfy from the event is one it must not pretend to.
/// Returns the [`TriggerFire`] when the event matches, `None` otherwise. The
/// [`TriggerSpec::Api`] arm never matches an event; it is fired only through the
/// API path.
fn spec_matches(
    spec: &TriggerSpec,
    kind: &str,
    entity_id: &str,
    summary: &serde_json::Value,
) -> Option<TriggerFire> {
    let field = |name: &str| summary.get(name).and_then(|v| v.as_str());
    match spec {
        TriggerSpec::WorkTransition {
            to_state,
            project,
            item_kind,
            label,
            work_item,
        } => {
            if kind != "work.transitioned" {
                return None;
            }
            if field("to") != Some(to_state.as_str()) {
                return None;
            }
            let event_ref = field("ref");
            if let Some(want) = work_item {
                if event_ref != Some(want.as_str()) {
                    return None;
                }
            }
            if let Some(want) = project {
                if field("project") != Some(want.as_str()) {
                    return None;
                }
            }
            if let Some(want) = item_kind {
                if field("kind") != Some(want.as_str()) {
                    return None;
                }
            }
            if let Some(want) = label {
                let has = summary
                    .get("labels")
                    .and_then(|v| v.as_array())
                    .is_some_and(|arr| arr.iter().any(|l| l.as_str() == Some(want.as_str())));
                if !has {
                    return None;
                }
            }
            let subject = event_ref.unwrap_or(entity_id);
            Some(TriggerFire {
                trigger_kind: spec.kind_str(),
                bind_work_item: event_ref.map(str::to_string),
                description: format!("{subject} entered {to_state}"),
            })
        }
        TriggerSpec::ObservationNew {
            observation_kind,
            project,
        } => {
            if kind != "observation.new" {
                return None;
            }
            if field("kind") != Some(observation_kind.as_str()) {
                return None;
            }
            if let Some(want) = project {
                if field("project") != Some(want.as_str()) {
                    return None;
                }
            }
            let subject = field("subject").unwrap_or(entity_id);
            Some(TriggerFire {
                trigger_kind: spec.kind_str(),
                bind_work_item: None,
                description: format!("new {observation_kind}: {subject}"),
            })
        }
        TriggerSpec::DriftProposed { project } => {
            if kind != "drift.raised" {
                return None;
            }
            if let Some(want) = project {
                if field("project") != Some(want.as_str()) {
                    return None;
                }
            }
            let what = field("summary")
                .or_else(|| field("kind"))
                .unwrap_or(entity_id);
            Some(TriggerFire {
                trigger_kind: spec.kind_str(),
                bind_work_item: None,
                description: format!("drift raised: {what}"),
            })
        }
        TriggerSpec::ForgePrChecks { status, work_item } => {
            if kind != "forge.pr.checks" {
                return None;
            }
            let event_status = field("status")?;
            let status_ok = match status {
                ForgeCheckStatus::Any => matches!(event_status, "green" | "red"),
                ForgeCheckStatus::Green => event_status == "green",
                ForgeCheckStatus::Red => event_status == "red",
            };
            if !status_ok {
                return None;
            }
            let event_item = field("work_item");
            if let Some(want) = work_item {
                if event_item != Some(want.as_str()) {
                    return None;
                }
            }
            let pr = field("pr").or(event_item).unwrap_or(entity_id);
            Some(TriggerFire {
                trigger_kind: spec.kind_str(),
                bind_work_item: event_item.map(str::to_string),
                description: format!("PR {pr} checks {event_status}"),
            })
        }
        TriggerSpec::Api => None,
    }
}

/// The concurrency cap a task is stored with, clamped to a sane band: at least
/// one (zero would mean a task that can never run), and a ceiling so a typo
/// cannot ask for a thousand simultaneous PTYs.
fn clean_concurrency(value: Option<u32>) -> u32 {
    value
        .map(|c| c.clamp(1, 20))
        .unwrap_or(DEFAULT_TASK_CONCURRENCY)
}

/// Validate and normalise a task's declared triggers (#290): trim the string
/// fields, drop blanks to `None`, and reject a trigger whose required field is
/// empty — a `work-transition` with no `to_state` matches nothing and is a
/// configuration mistake, caught at write time rather than at fire time.
fn clean_triggers(triggers: Option<Vec<AgentTaskTrigger>>) -> Result<Vec<AgentTaskTrigger>> {
    let Some(triggers) = triggers else {
        return Ok(vec![]);
    };
    let mut out = Vec::with_capacity(triggers.len());
    for mut trigger in triggers {
        validate_trigger_spec(&mut trigger.spec)?;
        out.push(trigger);
    }
    Ok(out)
}

fn validate_trigger_spec(spec: &mut TriggerSpec) -> Result<()> {
    match spec {
        TriggerSpec::WorkTransition {
            to_state,
            project,
            item_kind,
            label,
            work_item,
        } => {
            *to_state = to_state.trim().to_string();
            if to_state.is_empty() {
                return Err(ApiError::BadRequest(
                    "work-transition trigger needs a to_state".into(),
                ));
            }
            trim_opt(project);
            trim_opt(item_kind);
            trim_opt(label);
            trim_opt(work_item);
        }
        TriggerSpec::ObservationNew {
            observation_kind,
            project,
        } => {
            *observation_kind = observation_kind.trim().to_string();
            if observation_kind.is_empty() {
                return Err(ApiError::BadRequest(
                    "observation-new trigger needs an observation_kind".into(),
                ));
            }
            trim_opt(project);
        }
        TriggerSpec::DriftProposed { project } => trim_opt(project),
        TriggerSpec::ForgePrChecks { work_item, .. } => trim_opt(work_item),
        TriggerSpec::Api => {}
    }
    Ok(())
}

/// Trim an optional string in place, folding a now-empty value to `None` — the
/// same rule `clean_optional` applies, so a GUI's empty filter field is no
/// filter rather than a filter on the empty string.
fn trim_opt(value: &mut Option<String>) {
    if let Some(inner) = value {
        let trimmed = inner.trim().to_string();
        *value = (!trimmed.is_empty()).then_some(trimmed);
    }
}

/// Validate and normalise a task's declared gates. A gate a client could not
/// meaningfully answer — no question, no options, a blank option label — is
/// rejected at write time rather than discovered when a run tries to open it.
fn clean_gates(gates: Option<Vec<GateSpec>>) -> Result<Vec<GateSpec>> {
    let Some(gates) = gates else {
        return Ok(vec![]);
    };
    for gate in &gates {
        gate.validate().map_err(ApiError::BadRequest)?;
    }
    Ok(gates)
}

/// Normalise a task's `output_schema`: a JSON `null` (or an omitted field)
/// means "no schema", the same as unset. Anything else is stored verbatim and
/// validated against at run time — the engine does not police that it is itself
/// a well-formed JSON Schema, only that findings match whatever it says.
fn clean_output_schema(value: Option<serde_json::Value>) -> Option<serde_json::Value> {
    value.filter(|v| !v.is_null())
}

/// Normalise a submitted [`WorkflowEngineConfig`] (#293): a config with a blank
/// `engine_url` or `workflow` is treated as absent (the way a blank string
/// unbinds elsewhere), so a client can clear the backend by sending an empty
/// one. String fields are trimmed; the optionals are dropped when blank.
fn clean_workflow_engine(value: Option<WorkflowEngineConfig>) -> Option<WorkflowEngineConfig> {
    let cfg = value?;
    let engine_url = cfg.engine_url.trim().to_string();
    let workflow = cfg.workflow.trim().to_string();
    if engine_url.is_empty() || workflow.is_empty() {
        return None;
    }
    Some(WorkflowEngineConfig {
        engine_url,
        workflow,
        token_file: clean_optional(cfg.token_file),
        repo_ref: clean_optional(cfg.repo_ref),
    })
}

/// How often a workflow run is polled for a terminal state (#293). A steer/gate
/// bridge that would let the engine push instead is increment-2 work; until
/// then a fixed interval is the whole tracking mechanism.
const WORKFLOW_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// How many consecutive poll errors are tolerated before a workflow run is
/// given up on as errored — a transient blip is ridden out, a persistently
/// unreachable engine is not polled forever.
const WORKFLOW_POLL_MAX_ERRORS: u32 = 5;
/// The ceiling on how long a workflow run may stay non-terminal before it is
/// recorded errored, so a run the engine never finishes does not stay `running`
/// in Vogt indefinitely.
const WORKFLOW_POLL_DEADLINE: Duration = Duration::hours(6);
/// How long the SSE tracker waits for the next event before giving up on the
/// stream and degrading to polling (#293 inc-2). A live stream sends progress or
/// keep-alives well inside this; a stream that has gone quiet is treated as
/// broken rather than blocking the run's tracking indefinitely.
const WORKFLOW_SSE_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Read the workflow engine's bearer token from `path`, following the
/// token-from-file pattern in `config.rs` (`read_token_path`). `None` when no
/// file is configured; an `Err(reason)` when the file is set but unreadable or
/// empty, so the caller can record the run errored with the reason rather than
/// panicking.
fn read_workflow_token(path: Option<&str>) -> std::result::Result<Option<String>, String> {
    let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading token_file ({path}): {e}"))?;
    let value = raw.trim().to_string();
    if value.is_empty() {
        return Err(format!("token_file ({path}) is empty"));
    }
    Ok(Some(value))
}

/// Build the goal handed to the workflow engine: the task prompt, with the Vogt
/// subject appended when the task is bound to one, so the engine's run names the
/// same subject Vogt will file its conclusion against.
fn build_workflow_goal(task: &AgentTask) -> String {
    let base = task.prompt.trim();
    match vogt_binding_line(task) {
        Some(binding) => format!("{base}\n\n(Vogt subject: {binding})"),
        None => base.to_string(),
    }
}

/// The one-line summary stored on a concluded workflow run when the engine gave
/// no summary of its own — phrased by outcome, and naming the final sha when
/// there is one.
fn workflow_outcome_summary(
    outcome: AgentTaskRunOutcome,
    conclusion: &AgentTaskRunConclusion,
) -> String {
    let base = match outcome {
        AgentTaskRunOutcome::Succeeded => "Workflow engine run succeeded",
        AgentTaskRunOutcome::Failed => "Workflow engine run failed",
        AgentTaskRunOutcome::PartiallySucceeded => "Workflow engine run partially succeeded",
        AgentTaskRunOutcome::Skipped => "Workflow engine run skipped",
        AgentTaskRunOutcome::Blocked => "Workflow engine run blocked",
    };
    match &conclusion.final_sha {
        Some(sha) => format!("{base} at {sha}"),
        None => base.to_string(),
    }
}

/// Track a workflow run off its live SSE event stream, degrading to polling
/// (#293, increment 2).
///
/// Spawned once per workflow run. It first subscribes to the engine's event
/// stream ([`WorkflowProvider::stream_events`]): each event is mirrored onto the
/// engine's log for observability, and the moment one signals a terminal state
/// the run is concluded via [`finalize_workflow_run`]. When the stream is absent
/// (unreachable engine, non-success status), breaks mid-way, goes idle past
/// [`WORKFLOW_SSE_IDLE_TIMEOUT`], or ends without a terminal event, it falls
/// back to [`poll_workflow_to_terminal`] — the original increment-1 behaviour.
///
/// Absence is non-fatal throughout: nothing here panics or touches the
/// scheduler; a broken or missing stream simply becomes a polled run.
fn spawn_workflow_tracker(
    registry: Arc<AgentTaskRegistry>,
    provider: HttpWorkflowProvider,
    provider_run_id: String,
    task_id: Uuid,
    run_id: Uuid,
    session_id: Uuid,
    started: OffsetDateTime,
) {
    tokio::spawn(async move {
        match provider.stream_events(&provider_run_id).await {
            Ok(mut stream) => loop {
                // Honour the overall deadline even while the stream is open, so a
                // run the engine never finishes cannot stay `running` forever.
                if OffsetDateTime::now_utc() - started > WORKFLOW_POLL_DEADLINE {
                    registry.record_workflow_errored(
                        task_id,
                        run_id,
                        session_id,
                        started,
                        OffsetDateTime::now_utc(),
                        "workflow engine run did not reach a terminal state before the deadline",
                    );
                    return;
                }
                match tokio::time::timeout(WORKFLOW_SSE_IDLE_TIMEOUT, stream.next()).await {
                    Ok(Some(Ok(event))) => {
                        // Mirror the progress line onto the engine's log — the
                        // observability surface a workflow run has, since it owns
                        // no PTY whose output would otherwise carry progress.
                        tracing::info!(
                            target: "workflow_engine",
                            run = %run_id,
                            provider_run = %provider_run_id,
                            kind = %event.kind,
                            step = event.step_id.as_deref().unwrap_or("-"),
                            message = event.message.as_deref().unwrap_or(""),
                            "workflow run event",
                        );
                        if let Some(state) = event.terminal_state {
                            finalize_workflow_run(
                                &registry,
                                &provider,
                                &provider_run_id,
                                task_id,
                                run_id,
                                session_id,
                                started,
                                state,
                                event.message,
                            )
                            .await;
                            return;
                        }
                    }
                    Ok(Some(Err(err))) => {
                        tracing::warn!(
                            target: "workflow_engine",
                            run = %run_id,
                            error = %err,
                            "workflow event stream errored; falling back to polling",
                        );
                        break;
                    }
                    Ok(None) => {
                        tracing::info!(
                            target: "workflow_engine",
                            run = %run_id,
                            "workflow event stream ended without a terminal event; polling",
                        );
                        break;
                    }
                    Err(_elapsed) => {
                        tracing::info!(
                            target: "workflow_engine",
                            run = %run_id,
                            "workflow event stream idle; falling back to polling",
                        );
                        break;
                    }
                }
            },
            Err(err) => {
                tracing::info!(
                    target: "workflow_engine",
                    run = %run_id,
                    error = %err,
                    "workflow event stream unavailable; tracking by polling",
                );
            }
        }
        // Fallback: track the run to terminal by polling (increment-1 path).
        poll_workflow_to_terminal(
            registry,
            provider,
            provider_run_id,
            task_id,
            run_id,
            session_id,
            started,
        )
        .await;
    });
}

/// Conclude a run whose live stream reported a terminal state (#293 inc-2). The
/// provisional SSE envelope carries the terminal *state* but not the full
/// conclusion (diff, final sha, cost), so one confirming poll enriches it; if
/// that poll fails or is no longer terminal, the stream's own state is recorded
/// with an empty conclusion. Either way the run is concluded — never left
/// running on the strength of a stream event alone.
#[allow(clippy::too_many_arguments)]
async fn finalize_workflow_run(
    registry: &Arc<AgentTaskRegistry>,
    provider: &HttpWorkflowProvider,
    provider_run_id: &str,
    task_id: Uuid,
    run_id: Uuid,
    session_id: Uuid,
    started: OffsetDateTime,
    state: ProviderRunState,
    event_message: Option<String>,
) {
    let finished = OffsetDateTime::now_utc();
    let (outcome, conclusion, provider_summary) = match provider.poll(provider_run_id).await {
        Ok(status) if status.state.is_terminal() => {
            let summary = status.conclusion.as_ref().and_then(|c| c.summary.clone());
            let (outcome, conclusion) =
                map_conclusion(status.state, status.conclusion, started, finished);
            (outcome, conclusion, summary)
        }
        _ => {
            let (outcome, conclusion) = map_conclusion(state, None, started, finished);
            (outcome, conclusion, None)
        }
    };
    let summary = provider_summary
        .or(event_message)
        .unwrap_or_else(|| workflow_outcome_summary(outcome, &conclusion));
    registry.record_workflow_conclusion(task_id, run_id, session_id, outcome, conclusion, summary);
}

/// Poll a workflow run to a terminal state and record its conclusion (#293).
///
/// The increment-1 tracking mechanism, now the fallback [`spawn_workflow_tracker`]
/// degrades to. It polls on a fixed interval until the engine reports a terminal
/// state (mapped to a conclusion), a persistent poll failure exhausts the error
/// budget, or the deadline passes — every exit but the happy one records the run
/// errored with a written reason. Absence is non-fatal: nothing here panics or
/// touches the scheduler.
async fn poll_workflow_to_terminal(
    registry: Arc<AgentTaskRegistry>,
    provider: HttpWorkflowProvider,
    provider_run_id: String,
    task_id: Uuid,
    run_id: Uuid,
    session_id: Uuid,
    started: OffsetDateTime,
) {
    let mut consecutive_errors = 0u32;
    loop {
        tokio::time::sleep(WORKFLOW_POLL_INTERVAL).await;

        if OffsetDateTime::now_utc() - started > WORKFLOW_POLL_DEADLINE {
            registry.record_workflow_errored(
                task_id,
                run_id,
                session_id,
                started,
                OffsetDateTime::now_utc(),
                "workflow engine run did not reach a terminal state before the deadline",
            );
            return;
        }

        match provider.poll(&provider_run_id).await {
            Ok(status) => {
                consecutive_errors = 0;
                if status.state.is_terminal() {
                    let finished = OffsetDateTime::now_utc();
                    let provider_summary =
                        status.conclusion.as_ref().and_then(|c| c.summary.clone());
                    let (outcome, conclusion) =
                        map_conclusion(status.state, status.conclusion, started, finished);
                    let summary = provider_summary
                        .unwrap_or_else(|| workflow_outcome_summary(outcome, &conclusion));
                    registry.record_workflow_conclusion(
                        task_id, run_id, session_id, outcome, conclusion, summary,
                    );
                    return;
                }
            }
            Err(err) => {
                consecutive_errors += 1;
                if consecutive_errors >= WORKFLOW_POLL_MAX_ERRORS {
                    registry.record_workflow_errored(
                        task_id,
                        run_id,
                        session_id,
                        started,
                        OffsetDateTime::now_utc(),
                        &format!("workflow engine poll failed {consecutive_errors}x: {err}"),
                    );
                    return;
                }
            }
        }
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn clean_command(command: Option<Vec<String>>) -> Option<Vec<String>> {
    command
        .map(|argv| {
            argv.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|argv| !argv.is_empty())
}

fn clean_notify_phrase(value: Option<String>) -> Option<String> {
    match clean_optional(value) {
        Some(v) if v.eq_ignore_ascii_case("none") => None,
        Some(v) => Some(v),
        None => Some(DEFAULT_NOTIFY_PHRASE.to_string()),
    }
}

fn default_notify_phrase() -> Option<String> {
    Some(DEFAULT_NOTIFY_PHRASE.to_string())
}

/// The prefixes a run's output is scanned for, given the task's configured
/// notify phrase (#203).
///
/// A task left on either default — the current `VOGT_NOTIFY:` or the legacy
/// `MYDEVENV2_NOTIFY:` a task created before the rename still carries — is
/// matched against *both*, so a run that prints either prefix is recognised
/// through the client's own default changeover. A task configured with a
/// bespoke phrase is matched on exactly that and nothing else.
fn accepted_notify_phrases(configured: &str) -> Vec<String> {
    if configured == DEFAULT_NOTIFY_PHRASE || configured == LEGACY_NOTIFY_PHRASE {
        vec![
            DEFAULT_NOTIFY_PHRASE.to_string(),
            LEGACY_NOTIFY_PHRASE.to_string(),
        ]
    } else {
        vec![configured.to_string()]
    }
}

/// The earliest accepted phrase present in `tail`, as `(byte index, phrase)`.
///
/// "Earliest" so that when a run prints one prefix the message after it is the
/// one read, regardless of which accepted phrase it was.
fn find_notify_match<'a>(tail: &str, phrases: &'a [String]) -> Option<(usize, &'a str)> {
    phrases
        .iter()
        .filter_map(|phrase| tail.find(phrase.as_str()).map(|idx| (idx, phrase.as_str())))
        .min_by_key(|(idx, _)| *idx)
}

fn default_true() -> bool {
    true
}

fn default_command() -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-lc".to_string(),
        "printf 'Vogt scheduled agent task\\n\\n'; cat \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\"; printf '\\nPrompt file: %s\\nContext file: %s\\n' \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\" \"$MYDEVENV2_AGENT_TASK_CONTEXT_FILE\"; exec \"${SHELL:-/bin/bash}\"".to_string(),
    ]
}

fn expand_command(
    command: Vec<String>,
    task: &AgentTask,
    run_id: Uuid,
    prompt_file: &str,
    context_file: &str,
) -> Vec<String> {
    command
        .into_iter()
        .map(|arg| {
            arg.replace("{task_id}", &task.id.to_string())
                .replace("{task_name}", &task.name)
                .replace("{run_id}", &run_id.to_string())
                .replace("{prompt_file}", prompt_file)
                .replace("{context_file}", context_file)
        })
        .collect()
}

fn validate_schedule(schedule: &AgentTaskSchedule) -> Result<()> {
    let _ = compute_next_run(schedule, OffsetDateTime::now_utc())?;
    Ok(())
}

fn compute_next_run(
    schedule: &AgentTaskSchedule,
    after: OffsetDateTime,
) -> Result<Option<OffsetDateTime>> {
    match schedule {
        AgentTaskSchedule::Manual => Ok(None),
        AgentTaskSchedule::Interval { minutes } => {
            if *minutes == 0 {
                return Err(ApiError::BadRequest(
                    "interval schedule minutes must be greater than zero".into(),
                ));
            }
            Ok(Some(after + Duration::minutes(*minutes as i64)))
        }
        AgentTaskSchedule::Daily { times } => {
            if times.is_empty() {
                return Err(ApiError::BadRequest(
                    "daily schedule requires at least one HH:MM time".into(),
                ));
            }
            let mut best = None;
            for raw in times {
                let time = parse_hhmm(raw)?;
                let today = after.date().with_time(time).assume_utc();
                let candidate = if today > after {
                    today
                } else {
                    after
                        .date()
                        .next_day()
                        .ok_or_else(|| ApiError::Internal("date overflow".into()))?
                        .with_time(time)
                        .assume_utc()
                };
                if match best {
                    Some(current) => candidate < current,
                    None => true,
                } {
                    best = Some(candidate);
                }
            }
            Ok(best)
        }
    }
}

/// How a task's Vogt binding reads in its prompt, or `None` when unbound.
///
/// A task may name both a project and a work item; the item is named first
/// because it is the more specific of the two and is what a run should
/// report against when it has one.
fn vogt_binding_line(task: &AgentTask) -> Option<String> {
    match (task.vogt_work_item.as_deref(), task.vogt_project.as_deref()) {
        (Some(item), Some(project)) => Some(format!("{item} (project {project})")),
        (Some(item), None) => Some(item.to_string()),
        (None, Some(project)) => Some(format!("project {project}")),
        (None, None) => None,
    }
}

fn parse_hhmm(raw: &str) -> Result<Time> {
    let (hour, minute) = raw
        .split_once(':')
        .ok_or_else(|| ApiError::BadRequest(format!("invalid time {raw:?}; expected HH:MM")))?;
    let hour = hour
        .parse::<u8>()
        .map_err(|e| ApiError::BadRequest(format!("invalid hour in {raw:?}: {e}")))?;
    let minute = minute
        .parse::<u8>()
        .map_err(|e| ApiError::BadRequest(format!("invalid minute in {raw:?}: {e}")))?;
    Time::from_hms(hour, minute, 0)
        .map_err(|e| ApiError::BadRequest(format!("invalid time {raw:?}: {e}")))
}

/// How often the orchestrator re-checks a run it is not otherwise woken for —
/// short enough that a gate timeout or a session death is noticed promptly,
/// long enough not to spin. A steer or an answer wakes it immediately through
/// its `Notify`, so this is only the ceiling on latency for the polled events.
const ORCHESTRATOR_POLL_MS: u64 = 120;

/// Drive one run's gates and steer queue for its whole life (#289).
///
/// The single loop is the whole mechanism, and it is small on purpose:
///
/// * **Holding a gate.** While a gate it opened is still `Open`, the loop stays
///   in the gate branch and never reaches the steer drain — that is what
///   "hold the PTY at the prompt boundary" means in code. Nothing is written
///   to the PTY until the gate resolves.
/// * **Failing closed.** A gate that is not answered before its deadline is
///   blocked and its session killed; a session that dies with a gate held has
///   every open gate blocked. Neither path can write an approval — only the
///   answer branch delivers a chosen option's input.
/// * **Steering between rounds.** With no gate pending or open, at a prompt
///   boundary (activity ≠ Running) the loop drains one queued steer per pass.
///
/// The orchestrator opens gates only at a boundary, so a gate is always put to
/// the human at a point the CLI is actually waiting — never mid-thought.
fn spawn_run_orchestrator(
    registry: Arc<AgentTaskRegistry>,
    control: Arc<RunControl>,
    run_id: Uuid,
    gates: Vec<GateSpec>,
    auto_approve: bool,
) {
    tokio::spawn(async move {
        let session = Arc::clone(&control.session);
        let mut pending: VecDeque<GateSpec> = gates.into();
        // The gate this loop is currently holding, if any. Held as its spec so
        // its deadline is known without another lookup; its live state is read
        // back from the store each pass, because the API may have answered it.
        let mut current: Option<GateSpec> = None;

        loop {
            if session.exit_code().is_some() {
                // Fail closed: a run that ended with a question unanswered
                // answered nothing.
                registry.block_open_gates(run_id, "session ended while a gate was open");
                break;
            }

            if let Some(spec) = current.clone() {
                let Some(record) = registry.gate_record(run_id, spec.id) else {
                    // The run (or its gate) was deleted underneath us; nothing
                    // left to hold.
                    current = None;
                    continue;
                };
                match &record.state {
                    GateState::Open => {
                        let elapsed_ms = (OffsetDateTime::now_utc() - record.opened_at)
                            .whole_milliseconds()
                            .max(0) as u64;
                        if elapsed_ms >= spec.timeout_ms() {
                            registry.block_gate(run_id, spec.id, "timed out awaiting an answer");
                            // The CLI is still blocked at its prompt; end the
                            // run rather than leave it paused forever.
                            let _ = session.kill();
                            current = None;
                            continue;
                        }
                        wait_a_beat(&control.notify).await;
                        continue;
                    }
                    GateState::Answered { .. } => {
                        if let Some(input) = record.answered_input() {
                            deliver_line(&session, input);
                        }
                        current = None;
                        continue;
                    }
                    GateState::Blocked { .. } => {
                        // Blocked by a timeout we already handled, or by a
                        // death race; the run cannot proceed past a blocked
                        // gate, so end it.
                        let _ = session.kill();
                        current = None;
                        continue;
                    }
                }
            }

            // No gate held. Only act at a prompt boundary.
            if session.activity() == ActivityState::Running {
                wait_a_beat(&control.notify).await;
                continue;
            }

            if let Some(spec) = pending.pop_front() {
                if let Err(e) = registry.open_gate(run_id, &spec) {
                    tracing::warn!(run = %run_id, error = %e, "failed to open agent-task gate");
                    continue;
                }
                if auto_approve {
                    // The audited bypass: answer with the gate's own approve
                    // option, recorded as `auto-approve`. A gate with no
                    // approve option is left open and will fail closed on its
                    // deadline — the bypass is "yes", not "pick anything".
                    if let Some(idx) = spec.approve_index() {
                        if let Err(e) = registry.resolve_gate_answer(
                            run_id,
                            spec.id,
                            idx,
                            AUTO_APPROVE_ACTOR,
                            true,
                            Some("--auto-approve".to_string()),
                        ) {
                            tracing::warn!(run = %run_id, error = %e, "auto-approve failed");
                        }
                    }
                }
                current = Some(spec);
                continue;
            }

            // No gates. Drain one steer per boundary pass.
            let next = control.steer.lock().pop_front();
            if let Some(item) = next {
                let bytes = gates::steer_delivery_bytes(&item);
                if let Err(e) = session.write_input(&bytes) {
                    tracing::warn!(run = %run_id, error = %e, "failed to deliver steer to PTY");
                }
                registry.emit_steered(run_id, &item);
                continue;
            }

            wait_a_beat(&control.notify).await;
        }

        registry.remove_run_control(run_id);
    });
}

/// Wait for a wake-up (a steer queued, a gate answered) or the poll ceiling,
/// whichever comes first. Swallows the timeout: the caller re-checks
/// everything each pass regardless of why it woke.
async fn wait_a_beat(notify: &Notify) {
    let _ = tokio::time::timeout(
        std::time::Duration::from_millis(ORCHESTRATOR_POLL_MS),
        notify.notified(),
    )
    .await;
}

/// Write a line to the PTY: the text, then the Enter the CLI is waiting for.
fn deliver_line(session: &Arc<Session>, text: &str) {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(b'\r');
    if let Err(e) = session.write_input(&bytes) {
        tracing::warn!(session = %session.id, error = %e, "failed to deliver gate answer to PTY");
    }
}

/// Watch a run's output for the phrase its task asked to be told about.
///
/// Takes the registry rather than only the push manager (FR-E7): the matched
/// line is recorded on the run *before* it is pushed, so that a finding
/// survives a phone that was off, a subscription that had expired, and a
/// push service that was down. The push is unchanged — this is "not only as
/// push notifications", and both halves happen for every task, bound or not.
/// Whether anything collects the finding is Vogt's decision, made from the
/// binding, and is not this watcher's business.
fn spawn_phrase_watcher(
    registry: Arc<AgentTaskRegistry>,
    session: Arc<Session>,
    task_name: String,
    task_id: Uuid,
    run_id: Uuid,
    phrase: String,
    mut rx: tokio::sync::broadcast::Receiver<crate::pty::OutputChunk>,
) {
    let push = Arc::clone(&registry.push);
    let phrases = accepted_notify_phrases(&phrase);
    tokio::spawn(async move {
        let mut tail = String::new();
        loop {
            let chunk =
                match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                    Err(_) if session.exit_code().is_some() => break,
                    Err(_) => continue,
                };
            let stripped = strip_ansi(&chunk.data);
            tail.push_str(&String::from_utf8_lossy(&stripped));
            if tail.len() > 8192 {
                let keep_from = tail.len().saturating_sub(4096);
                tail.drain(..keep_from);
            }
            if let Some((idx, matched)) = find_notify_match(&tail, &phrases) {
                let msg = tail[idx + matched.len()..]
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let body = if msg.is_empty() {
                    format!("{task_name} requested your attention.")
                } else {
                    msg
                };
                if let Err(e) = registry.record_finding(task_id, run_id, &body) {
                    // A finding that could not be written is worth a line in
                    // the log and nothing more: the notification below is
                    // still the behaviour this task was configured for, and
                    // losing the durable copy must not lose the alert too.
                    tracing::warn!(
                        task = %task_id,
                        run = %run_id,
                        error = %e,
                        "failed to record agent task finding"
                    );
                }
                let data = json!({
                    "kind": "agent-task-notify",
                    "task_id": task_id.to_string(),
                    "run_id": run_id.to_string(),
                    "session_id": session.id.to_string(),
                    "url": format!("/#/t/{}", session.id),
                });
                let title = format!("{task_name} update");
                let _ = push
                    .notify(NotificationKind::AgentTaskNotify, &title, &body, data)
                    .await;
                break;
            }
        }
    });
}

/// Cap on consecutive auto-retries for a single run — a persistently invalid
/// key or a genuinely dead upstream shouldn't retry forever.
const MAX_AUTO_RETRIES: u32 = 5;

/// Sibling to `spawn_phrase_watcher`: tails the same session output and,
/// on a transient-failure phrase (429 / rate limit / overloaded), writes a
/// retry keystroke back into the PTY after an exponential backoff instead of
/// only notifying. Gives up and notifies once after `MAX_AUTO_RETRIES`.
fn spawn_retry_watcher(
    push: Arc<PushManager>,
    session: Arc<Session>,
    task_name: String,
    mut rx: tokio::sync::broadcast::Receiver<crate::pty::OutputChunk>,
) {
    tokio::spawn(async move {
        let mut tail = String::new();
        let mut attempts: u32 = 0;
        loop {
            let chunk =
                match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                    Err(_) if session.exit_code().is_some() => break,
                    Err(_) => continue,
                };
            if session.exit_code().is_some() {
                break;
            }
            let stripped = strip_ansi(&chunk.data);
            tail.push_str(&String::from_utf8_lossy(&stripped));
            if tail.len() > 8192 {
                let keep_from = tail.len().saturating_sub(4096);
                tail.drain(..keep_from);
            }
            if !crate::activity::is_rate_limited(tail.as_bytes()) {
                continue;
            }
            // Consume the matched text so the same message can't re-trigger
            // a retry on the next chunk.
            tail.clear();

            if attempts >= MAX_AUTO_RETRIES {
                let data = json!({
                    "kind": "agent-task-notify",
                    "session_id": session.id.to_string(),
                    "url": format!("/#/t/{}", session.id),
                });
                let title = format!("{task_name} auto-retry gave up");
                let body = format!(
                    "Still rate-limited/overloaded after {MAX_AUTO_RETRIES} retries. Check the session."
                );
                let _ = push
                    .notify(NotificationKind::AgentTaskNotify, &title, &body, data)
                    .await;
                break;
            }

            let backoff_secs = 5u64.saturating_mul(1u64 << attempts.min(4));
            attempts += 1;
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            if session.exit_code().is_some() {
                break;
            }
            let _ = session.write_input(b"\r");
        }
    });
}

// --- outcomes, conclusion, and schema validation (#291) --------------------

/// Resolve a run's typed outcome from everything known at completion, applying
/// the precedence documented on [`AgentTaskRunOutcome`]: a blocked gate first,
/// then a schema miss, then a self-declared skip, then the bare exit code.
fn resolve_outcome(
    exit_code: i32,
    blocked: bool,
    schema_ok: Option<bool>,
    skipped: bool,
) -> AgentTaskRunOutcome {
    if blocked {
        AgentTaskRunOutcome::Blocked
    } else if schema_ok == Some(false) {
        AgentTaskRunOutcome::PartiallySucceeded
    } else if skipped {
        AgentTaskRunOutcome::Skipped
    } else if exit_code == 0 {
        AgentTaskRunOutcome::Succeeded
    } else {
        AgentTaskRunOutcome::Failed
    }
}

/// The human-readable one-liner stored on `run.summary`, phrased by outcome so
/// a reader sees *why* it ended, not only the exit code. The `succeeded` and
/// `failed` wordings are kept verbatim from before #291 so existing clients and
/// tests reading `summary` are undisturbed.
fn outcome_summary(outcome: AgentTaskRunOutcome, code: i32) -> String {
    match outcome {
        AgentTaskRunOutcome::Succeeded => "Exited successfully".to_string(),
        AgentTaskRunOutcome::Failed => format!("Exited with status {code}"),
        AgentTaskRunOutcome::Blocked => "Blocked at an approval gate".to_string(),
        AgentTaskRunOutcome::PartiallySucceeded => {
            "Findings did not match the output schema".to_string()
        }
        AgentTaskRunOutcome::Skipped => "Skipped: nothing to do".to_string(),
    }
}

/// Run `git` in `cwd` and return trimmed stdout on success, or `None` for a
/// non-repo, a missing git, or any non-zero status. Blocking, but only a
/// handful of fast calls per completed run.
fn git_capture(cwd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The run's starting git position: the branch it is on (or the task's declared
/// branch) and the sha it starts from. Both `None` when the workspace is not a
/// repo; the sha alone is `None` for a fresh repo with no commit yet, which the
/// diff later measures from the empty tree.
fn git_run_start(cwd: &str, declared_branch: Option<&str>) -> (Option<String>, Option<String>) {
    if git_capture(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return (None, None);
    }
    let branch = declared_branch
        .map(str::to_string)
        .or_else(|| git_capture(cwd, &["rev-parse", "--abbrev-ref", "HEAD"]));
    let sha = git_capture(cwd, &["rev-parse", "HEAD"]);
    (branch, sha)
}

/// The tip sha of the bound branch at completion: the declared branch if named
/// and resolvable, else `HEAD`.
fn git_final_sha(cwd: &str, branch: Option<&str>) -> Option<String> {
    if let Some(branch) = branch {
        if let Some(sha) = git_capture(cwd, &["rev-parse", "--verify", "--quiet", branch]) {
            return Some(sha);
        }
    }
    git_capture(cwd, &["rev-parse", "--verify", "--quiet", "HEAD"])
}

/// Diff stats for what the run left on its branch: `git diff --numstat` from the
/// run's starting sha (or the empty tree, for a repo that had no commit) to the
/// final sha, reduced to files/insertions/deletions.
fn git_diff_stats(cwd: &str, base: Option<&str>, head: &str) -> Option<DiffStat> {
    let base = base.unwrap_or(EMPTY_TREE_SHA);
    let numstat = git_capture(cwd, &["diff", "--numstat", &format!("{base}..{head}")])?;
    Some(parse_numstat(&numstat))
}

/// Reduce `git diff --numstat` output to three numbers. A binary file's counts
/// read as `-\t-`; it still counts as a changed file, with no line deltas.
fn parse_numstat(numstat: &str) -> DiffStat {
    let mut stat = DiffStat::default();
    for line in numstat.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut cols = line.split('\t');
        let ins = cols.next().unwrap_or("-");
        let del = cols.next().unwrap_or("-");
        if cols.next().is_none() {
            continue;
        }
        stat.files += 1;
        stat.insertions += ins.parse::<u64>().unwrap_or(0);
        stat.deletions += del.parse::<u64>().unwrap_or(0);
    }
    stat
}

/// Parse the last `VOGT_COST:` line a run printed. The remainder is either a
/// JSON object or a bare dollar amount; anything unparseable yields `None`,
/// which the conclusion records as an unknown (`null`) cost.
fn parse_cost(scrollback: &str) -> Option<RunCost> {
    let rest = scrollback
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(COST_SENTINEL))
        .map(str::trim)?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(rest) {
        let cost = RunCost {
            total_usd: value
                .get("total_usd")
                .or_else(|| value.get("usd"))
                .and_then(serde_json::Value::as_f64),
            input_tokens: value
                .get("input_tokens")
                .and_then(serde_json::Value::as_u64),
            output_tokens: value
                .get("output_tokens")
                .and_then(serde_json::Value::as_u64),
        };
        if cost.total_usd.is_some() || cost.input_tokens.is_some() || cost.output_tokens.is_some() {
            return Some(cost);
        }
    }
    // A bare amount, tolerating a leading `$`.
    let amount = rest.trim_start_matches('$').trim();
    amount.parse::<f64>().ok().map(|usd| RunCost {
        total_usd: Some(usd),
        ..RunCost::default()
    })
}

/// Validate a JSON `instance` against a pragmatic subset of JSON Schema — the
/// keywords a task author actually reaches for on a findings block: `type`,
/// `required`, `properties`, `items`, `enum`, and the common numeric/length
/// bounds. Returns the first violation as a human-readable path + reason,
/// which is exactly what the re-prompt hands back to the CLI.
///
/// It is deliberately not a complete validator (no `$ref`, `allOf`, patterns):
/// the engine ships no schema crate, and the whole-of-JSON-Schema surface is
/// not what a findings contract needs. An unknown keyword is ignored, so a
/// schema using one is under-enforced rather than rejected.
fn validate_json_schema(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
) -> std::result::Result<(), String> {
    validate_at(schema, instance, "$")
}

fn validate_at(
    schema: &serde_json::Value,
    instance: &serde_json::Value,
    path: &str,
) -> std::result::Result<(), String> {
    let Some(obj) = schema.as_object() else {
        // A non-object schema (e.g. `true`) imposes nothing.
        return Ok(());
    };

    if let Some(ty) = obj.get("type").and_then(|t| t.as_str()) {
        if !json_type_matches(ty, instance) {
            return Err(format!("{path}: expected type {ty}"));
        }
    }

    if let Some(allowed) = obj.get("enum").and_then(|e| e.as_array()) {
        if !allowed.iter().any(|a| a == instance) {
            return Err(format!(
                "{path}: value is not one of the permitted enum values"
            ));
        }
    }

    if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
        if let Some(map) = instance.as_object() {
            for (key, subschema) in props {
                if let Some(child) = map.get(key) {
                    validate_at(subschema, child, &format!("{path}.{key}"))?;
                }
            }
        }
    }

    if let Some(required) = obj.get("required").and_then(|r| r.as_array()) {
        if let Some(map) = instance.as_object() {
            for key in required.iter().filter_map(|k| k.as_str()) {
                if !map.contains_key(key) {
                    return Err(format!("{path}: missing required property '{key}'"));
                }
            }
        }
    }

    if let Some(items) = obj.get("items") {
        if let Some(arr) = instance.as_array() {
            for (i, elem) in arr.iter().enumerate() {
                validate_at(items, elem, &format!("{path}[{i}]"))?;
            }
        }
    }

    if let (Some(min), Some(n)) = (
        obj.get("minimum").and_then(|m| m.as_f64()),
        instance.as_f64(),
    ) {
        if n < min {
            return Err(format!("{path}: {n} is below minimum {min}"));
        }
    }
    if let (Some(max), Some(n)) = (
        obj.get("maximum").and_then(|m| m.as_f64()),
        instance.as_f64(),
    ) {
        if n > max {
            return Err(format!("{path}: {n} is above maximum {max}"));
        }
    }
    if let (Some(min), Some(s)) = (
        obj.get("minLength").and_then(|m| m.as_u64()),
        instance.as_str(),
    ) {
        if (s.chars().count() as u64) < min {
            return Err(format!("{path}: string shorter than minLength {min}"));
        }
    }
    if let (Some(min), Some(arr)) = (
        obj.get("minItems").and_then(|m| m.as_u64()),
        instance.as_array(),
    ) {
        if (arr.len() as u64) < min {
            return Err(format!("{path}: fewer than minItems {min}"));
        }
    }

    Ok(())
}

fn json_type_matches(ty: &str, instance: &serde_json::Value) -> bool {
    match ty {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        "number" => instance.is_number(),
        // An integer is a number with no fractional part; JSON has no separate
        // integer type, so this accepts `2` and rejects `2.5`.
        "integer" => instance.is_i64() || instance.is_u64(),
        _ => true,
    }
}

/// Take the first complete fenced code block out of `buf`, returning its inner
/// text and consuming everything up to and including its closing fence. The
/// opening fence line (```` ```json ````) is skipped whole, so the returned text
/// is the block body. `None` when no complete block is present yet.
fn take_fenced_block(buf: &mut String) -> Option<String> {
    let open = buf.find("```")?;
    // Skip to the end of the opening fence's line, so a language tag on it
    // (```json) is not part of the body.
    let line_end = buf[open + 3..].find('\n').map(|i| open + 3 + i + 1)?;
    let rest = &buf[line_end..];
    let close_rel = rest.find("```")?;
    let inner = rest[..close_rel].to_string();
    let consumed = line_end + close_rel + 3;
    buf.replace_range(..consumed, "");
    Some(inner)
}

/// Validate a run's findings against its `output_schema`, re-prompting the CLI
/// on a mismatch up to `max_retries` times before recording the run
/// `partially-succeeded` (#291).
///
/// The findings block is the first complete fenced block the run prints (or,
/// when `output_file` is set, that file read at each block boundary). On a
/// match the verdict is recorded and the watcher steps aside — the run finishes
/// on its own. On a mismatch with budget left, a correction line is written
/// into the PTY and the next block awaited. When the budget is spent the
/// verdict is recorded failed and the session killed, so a run that cannot
/// produce the shape ends rather than hangs.
fn spawn_schema_watcher(
    registry: Arc<AgentTaskRegistry>,
    session: Arc<Session>,
    run_id: Uuid,
    schema: serde_json::Value,
    max_retries: u32,
    output_file: Option<String>,
    mut rx: tokio::sync::broadcast::Receiver<crate::pty::OutputChunk>,
) {
    tokio::spawn(async move {
        let mut buf = String::new();
        // Failures so far; also the count of re-prompts spent, recorded as the
        // run's `retries`.
        let mut retries: u32 = 0;
        loop {
            let chunk =
                match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                    Ok(Ok(chunk)) => chunk,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                        // The run ended without ever producing a valid block —
                        // a schema task that reported nothing is a partial
                        // success, not a clean one.
                        registry.record_schema_result(run_id, retries, false);
                        break;
                    }
                    Err(_) if session.exit_code().is_some() => {
                        registry.record_schema_result(run_id, retries, false);
                        break;
                    }
                    Err(_) => continue,
                };
            let stripped = strip_ansi(&chunk.data);
            buf.push_str(&String::from_utf8_lossy(&stripped));
            if buf.len() > 65_536 {
                let keep_from = buf.len().saturating_sub(32_768);
                buf.drain(..keep_from);
            }

            let Some(block) = take_fenced_block(&mut buf) else {
                continue;
            };
            // The file, when configured, is the source of truth; the fenced
            // block is only the signal that the run has produced its findings.
            let candidate = match output_file.as_deref() {
                Some(rel) => read_output_file(&session.cwd, rel).unwrap_or(block),
                None => block,
            };
            let parsed = serde_json::from_str::<serde_json::Value>(candidate.trim());
            let verdict = match &parsed {
                Ok(value) => validate_json_schema(&schema, value),
                Err(e) => Err(format!("findings were not valid JSON: {e}")),
            };
            match verdict {
                Ok(()) => {
                    registry.record_schema_result(run_id, retries, true);
                    break;
                }
                Err(reason) => {
                    if retries >= max_retries {
                        registry.record_schema_result(run_id, retries, false);
                        let _ = session.kill();
                        break;
                    }
                    retries += 1;
                    // Deliberately free of any code-fence markers: this line is
                    // echoed back onto the PTY, and a fence in it would be
                    // mistaken for the start of the very findings block we are
                    // asking for. The instruction to fence the JSON lives in the
                    // task prompt, not here.
                    let msg = format!(
                        "Your findings did not match output_schema ({reason}). \
                         Please output a corrected JSON findings block."
                    );
                    deliver_line(&session, &msg);
                }
            }
        }
    });
}

/// Read a run's findings file, relative to its workspace. `None` when it is
/// missing or unreadable, in which case the fenced block stands in.
fn read_output_file(cwd: &str, rel: &str) -> Option<String> {
    let path = Path::new(cwd).join(rel);
    std::fs::read_to_string(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_daily_time() {
        assert!(parse_hhmm("09:30").is_ok());
        assert!(parse_hhmm("24:00").is_err());
        assert!(parse_hhmm("nope").is_err());
    }

    #[test]
    fn interval_schedule_advances() {
        let now = OffsetDateTime::now_utc();
        let next = compute_next_run(&AgentTaskSchedule::Interval { minutes: 720 }, now)
            .unwrap()
            .unwrap();
        assert_eq!(next, now + Duration::hours(12));
    }

    fn task_bound_to(project: Option<&str>, work_item: Option<&str>) -> AgentTask {
        let now = OffsetDateTime::now_utc();
        AgentTask {
            id: Uuid::new_v4(),
            name: "Nightly".into(),
            prompt: "Look".into(),
            schedule: AgentTaskSchedule::Manual,
            triggers: vec![],
            concurrency: DEFAULT_TASK_CONCURRENCY,
            status: AgentTaskStatus::Active,
            command: None,
            cwd: None,
            env: vec![],
            context: None,
            vogt_project: project.map(str::to_string),
            vogt_work_item: work_item.map(str::to_string),
            gates: vec![],
            auto_approve: false,
            output_schema: None,
            output_file: None,
            output_schema_max_retries: DEFAULT_OUTPUT_SCHEMA_MAX_RETRIES,
            branch: None,
            workflow_engine: None,
            notify_on_start: false,
            notify_on_phrase: None,
            auto_retry_on_rate_limit: false,
            next_run: None,
            last_run: None,
            run_count: 0,
            runs: vec![],
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn the_more_specific_half_of_a_binding_is_named_first() {
        assert_eq!(
            vogt_binding_line(&task_bound_to(Some("vogt"), Some("WI-7"))).as_deref(),
            Some("WI-7 (project vogt)")
        );
        assert_eq!(
            vogt_binding_line(&task_bound_to(Some("vogt"), None)).as_deref(),
            Some("project vogt")
        );
        assert_eq!(
            vogt_binding_line(&task_bound_to(None, Some("WI-7"))).as_deref(),
            Some("WI-7")
        );
    }

    #[test]
    fn an_unbound_task_has_no_binding_line() {
        assert!(vogt_binding_line(&task_bound_to(None, None)).is_none());
    }

    #[test]
    fn a_default_notify_task_accepts_both_the_current_and_legacy_prefix() {
        // The rename (#203) must not strand a run that still prints the old
        // prefix, nor an old task definition that still carries it: either
        // default is matched against both.
        let both = vec![
            DEFAULT_NOTIFY_PHRASE.to_string(),
            LEGACY_NOTIFY_PHRASE.to_string(),
        ];
        assert_eq!(accepted_notify_phrases(DEFAULT_NOTIFY_PHRASE), both);
        assert_eq!(accepted_notify_phrases(LEGACY_NOTIFY_PHRASE), both);
        // The current default really is the new name; the legacy one is the old.
        assert_eq!(DEFAULT_NOTIFY_PHRASE, "VOGT_NOTIFY:");
        assert_eq!(LEGACY_NOTIFY_PHRASE, "MYDEVENV2_NOTIFY:");
    }

    #[test]
    fn a_bespoke_notify_phrase_is_matched_on_exactly_itself() {
        assert_eq!(
            accepted_notify_phrases("ALERT:"),
            vec!["ALERT:".to_string()]
        );
    }

    #[test]
    fn find_notify_match_recognises_either_default_prefix_and_reads_the_message() {
        let phrases = accepted_notify_phrases(DEFAULT_NOTIFY_PHRASE);
        for prefix in ["VOGT_NOTIFY:", "MYDEVENV2_NOTIFY:"] {
            let line = format!("noise\n{prefix} the sky is falling\nmore");
            let (idx, matched) =
                find_notify_match(&line, &phrases).expect("both defaults are recognised");
            assert_eq!(matched, prefix);
            let msg = line[idx + matched.len()..].lines().next().unwrap().trim();
            assert_eq!(msg, "the sky is falling");
        }
        assert!(find_notify_match("nothing to see here", &phrases).is_none());
    }

    #[test]
    fn a_binding_of_whitespace_is_no_binding() {
        // The GUI's empty field arrives as `""`, and a task bound to the
        // empty string would be bound to a project slug nothing can name.
        assert_eq!(clean_optional(Some("   ".into())), None);
        assert_eq!(
            clean_optional(Some(" vogt ".into())).as_deref(),
            Some("vogt")
        );
    }

    // --- #291: outcomes, conclusion inputs, schema validation ---------------

    #[test]
    fn outcome_precedence_is_blocked_then_schema_then_skip_then_exit() {
        use AgentTaskRunOutcome::*;
        // A clean exit with nothing to downgrade it.
        assert_eq!(resolve_outcome(0, false, None, false), Succeeded);
        // A non-zero exit with nothing more specific.
        assert_eq!(resolve_outcome(7, false, None, false), Failed);
        // A gate that failed closed outranks everything, even a clean exit.
        assert_eq!(resolve_outcome(0, true, Some(true), false), Blocked);
        // A schema miss outranks a skip and the exit code.
        assert_eq!(
            resolve_outcome(1, false, Some(false), true),
            PartiallySucceeded
        );
        // A self-declared skip on a clean exit.
        assert_eq!(resolve_outcome(0, false, None, true), Skipped);
        // A passing schema does not, by itself, change a clean exit.
        assert_eq!(resolve_outcome(0, false, Some(true), false), Succeeded);
    }

    #[test]
    fn the_outcome_wire_tokens_are_kebab_case() {
        assert_eq!(AgentTaskRunOutcome::Succeeded.as_str(), "succeeded");
        assert_eq!(AgentTaskRunOutcome::Failed.as_str(), "failed");
        assert_eq!(
            AgentTaskRunOutcome::PartiallySucceeded.as_str(),
            "partially-succeeded"
        );
        assert_eq!(AgentTaskRunOutcome::Skipped.as_str(), "skipped");
        assert_eq!(AgentTaskRunOutcome::Blocked.as_str(), "blocked");
        // The serde spelling matches the hand-written token.
        let json = serde_json::to_string(&AgentTaskRunOutcome::PartiallySucceeded).unwrap();
        assert_eq!(json, "\"partially-succeeded\"");
    }

    #[test]
    fn numstat_sums_lines_and_counts_binary_as_a_changed_file() {
        let stat = parse_numstat("3\t1\tsrc/a.rs\n10\t0\tsrc/b.rs\n-\t-\timg.png\n");
        assert_eq!(stat.files, 3);
        assert_eq!(stat.insertions, 13);
        assert_eq!(stat.deletions, 1);
        // Empty diff is three zeroes, not an error.
        assert_eq!(parse_numstat(""), DiffStat::default());
    }

    #[test]
    fn cost_parses_json_and_bare_dollar_amounts_else_none() {
        let json = parse_cost("noise\nVOGT_COST: {\"total_usd\": 0.42, \"input_tokens\": 1200}\n")
            .expect("a JSON cost line parses");
        assert_eq!(json.total_usd, Some(0.42));
        assert_eq!(json.input_tokens, Some(1200));

        let bare = parse_cost("VOGT_COST: $1.50").expect("a bare dollar amount parses");
        assert_eq!(bare.total_usd, Some(1.50));

        // The last cost line wins, and a run that reported nothing has no cost.
        let last = parse_cost("VOGT_COST: $1\nVOGT_COST: $2").unwrap();
        assert_eq!(last.total_usd, Some(2.0));
        assert!(parse_cost("no cost here").is_none());
    }

    #[test]
    fn schema_validation_accepts_a_match_and_names_the_first_miss() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["summary", "risk"],
            "properties": {
                "summary": { "type": "string", "minLength": 1 },
                "risk": { "type": "string", "enum": ["low", "medium", "high"] }
            }
        });
        let ok = serde_json::json!({ "summary": "all clear", "risk": "low" });
        assert!(validate_json_schema(&schema, &ok).is_ok());

        // Missing required property.
        let missing = serde_json::json!({ "summary": "hm" });
        assert!(validate_json_schema(&schema, &missing)
            .unwrap_err()
            .contains("risk"));

        // Wrong enum value.
        let bad_enum = serde_json::json!({ "summary": "hm", "risk": "critical" });
        assert!(validate_json_schema(&schema, &bad_enum)
            .unwrap_err()
            .contains("enum"));

        // Wrong type.
        let bad_type = serde_json::json!({ "summary": 3, "risk": "low" });
        assert!(validate_json_schema(&schema, &bad_type)
            .unwrap_err()
            .contains("type"));
    }

    #[test]
    fn a_fenced_block_is_taken_out_of_the_buffer_leaving_the_rest() {
        let mut buf = "chatter\n```json\n{\"ok\": true}\n```\ntrailing".to_string();
        let block = take_fenced_block(&mut buf).expect("a complete block is present");
        assert_eq!(block.trim(), "{\"ok\": true}");
        // The opening chatter and the block are consumed; the trailing text
        // remains for the next block.
        assert_eq!(buf, "\ntrailing");
        // An incomplete block (no closing fence) is not yet takeable.
        let mut partial = "```json\n{\"ok\": true}".to_string();
        assert!(take_fenced_block(&mut partial).is_none());
    }

    // --- #290: event triggers, matching, validation, audit -----------------

    fn work_transitioned(summary: serde_json::Value) -> (String, String, serde_json::Value) {
        ("work.transitioned".into(), "wi_1".into(), summary)
    }

    #[test]
    fn work_transition_matches_the_state_and_binds_the_item_ref() {
        let spec = TriggerSpec::WorkTransition {
            to_state: "ready".into(),
            project: None,
            item_kind: None,
            label: None,
            work_item: None,
        };
        let (kind, id, summary) =
            work_transitioned(serde_json::json!({"ref": "WI-7", "from": "review", "to": "ready"}));
        let fire = spec_matches(&spec, &kind, &id, &summary).expect("the state matches");
        assert_eq!(fire.trigger_kind, "work-transition");
        // The run is bound to the item that transitioned, by its ref.
        assert_eq!(fire.bind_work_item.as_deref(), Some("WI-7"));
        assert_eq!(fire.description, "WI-7 entered ready");

        // A different destination state does not match.
        let (kind, id, other) = work_transitioned(serde_json::json!({"ref": "WI-7", "to": "done"}));
        assert!(spec_matches(&spec, &kind, &id, &other).is_none());
    }

    #[test]
    fn work_transition_project_kind_and_label_filters_narrow_the_match() {
        let spec = TriggerSpec::WorkTransition {
            to_state: "ready".into(),
            project: Some("vogt".into()),
            item_kind: Some("bug".into()),
            label: Some("urgent".into()),
            work_item: None,
        };
        let (kind, id, ok) = work_transitioned(serde_json::json!({
            "ref": "WI-7", "to": "ready",
            "project": "vogt", "kind": "bug", "labels": ["urgent", "backend"],
        }));
        assert!(spec_matches(&spec, &kind, &id, &ok).is_some());

        // Wrong project, wrong kind, and a missing label each fail the match.
        for bad in [
            serde_json::json!({"ref":"WI-7","to":"ready","project":"other","kind":"bug","labels":["urgent"]}),
            serde_json::json!({"ref":"WI-7","to":"ready","project":"vogt","kind":"feature","labels":["urgent"]}),
            serde_json::json!({"ref":"WI-7","to":"ready","project":"vogt","kind":"bug","labels":["backend"]}),
        ] {
            assert!(spec_matches(&spec, &kind, &id, &bad).is_none());
        }
    }

    #[test]
    fn work_transition_scoped_to_one_item_ignores_the_others() {
        let spec = TriggerSpec::WorkTransition {
            to_state: "ready".into(),
            project: None,
            item_kind: None,
            label: None,
            work_item: Some("WI-7".into()),
        };
        let (kind, id, mine) = work_transitioned(serde_json::json!({"ref": "WI-7", "to": "ready"}));
        assert!(spec_matches(&spec, &kind, &id, &mine).is_some());
        let (kind, id, theirs) =
            work_transitioned(serde_json::json!({"ref": "WI-9", "to": "ready"}));
        assert!(spec_matches(&spec, &kind, &id, &theirs).is_none());
    }

    #[test]
    fn observation_new_matches_kind_and_optional_project() {
        let spec = TriggerSpec::ObservationNew {
            observation_kind: "forge.issue".into(),
            project: Some("vogt".into()),
        };
        let ok = serde_json::json!({"kind": "forge.issue", "project": "vogt", "subject": "#42"});
        let fire = spec_matches(&spec, "observation.new", "obs_1", &ok).expect("kind and project");
        assert_eq!(fire.trigger_kind, "observation-new");
        assert!(fire.bind_work_item.is_none());
        assert_eq!(fire.description, "new forge.issue: #42");

        // A PR is a different observation kind, and a different project is out.
        assert!(spec_matches(
            &spec,
            "observation.new",
            "obs_1",
            &serde_json::json!({"kind": "forge.pull_request", "project": "vogt"}),
        )
        .is_none());
        assert!(spec_matches(
            &spec,
            "observation.new",
            "obs_1",
            &serde_json::json!({"kind": "forge.issue", "project": "other"}),
        )
        .is_none());
    }

    #[test]
    fn drift_proposed_matches_drift_raised_and_scopes_by_project() {
        let any = TriggerSpec::DriftProposed { project: None };
        let raised = serde_json::json!({"kind": "coupling", "summary": "vogt depends on x", "project": "vogt"});
        assert!(spec_matches(&any, "drift.raised", "d_1", &raised).is_some());
        // The core spells it `drift.raised`, never `drift.proposed`.
        assert!(spec_matches(&any, "drift.proposed", "d_1", &raised).is_none());

        let scoped = TriggerSpec::DriftProposed {
            project: Some("vogt".into()),
        };
        assert!(spec_matches(&scoped, "drift.raised", "d_1", &raised).is_some());
        assert!(spec_matches(
            &scoped,
            "drift.raised",
            "d_1",
            &serde_json::json!({"kind": "coupling", "project": "other"}),
        )
        .is_none());
    }

    #[test]
    fn forge_pr_checks_matches_status_and_binds_the_linked_item() {
        let red = TriggerSpec::ForgePrChecks {
            status: ForgeCheckStatus::Red,
            work_item: None,
        };
        let event = serde_json::json!({"status": "red", "work_item": "WI-7", "pr": "#12"});
        let fire = spec_matches(&red, "forge.pr.checks", "pr_12", &event).expect("red matches red");
        assert_eq!(fire.trigger_kind, "forge-pr-checks");
        assert_eq!(fire.bind_work_item.as_deref(), Some("WI-7"));
        assert_eq!(fire.description, "PR #12 checks red");

        // A green event does not fire a red trigger; `Any` fires on either.
        let green = serde_json::json!({"status": "green", "work_item": "WI-7"});
        assert!(spec_matches(&red, "forge.pr.checks", "pr_12", &green).is_none());
        let any = TriggerSpec::ForgePrChecks {
            status: ForgeCheckStatus::Any,
            work_item: Some("WI-7".into()),
        };
        assert!(spec_matches(&any, "forge.pr.checks", "pr_12", &green).is_some());
        // Scoped to WI-7, a PR linked to WI-9 is ignored.
        assert!(spec_matches(
            &any,
            "forge.pr.checks",
            "pr_12",
            &serde_json::json!({"status": "green", "work_item": "WI-9"}),
        )
        .is_none());
    }

    #[test]
    fn an_api_trigger_never_matches_a_core_event() {
        for kind in ["work.transitioned", "drift.raised", "observation.new"] {
            assert!(spec_matches(&TriggerSpec::Api, kind, "x", &serde_json::json!({})).is_none());
        }
    }

    #[test]
    fn concurrency_is_clamped_to_a_sane_band() {
        assert_eq!(clean_concurrency(None), 1);
        assert_eq!(clean_concurrency(Some(0)), 1);
        assert_eq!(clean_concurrency(Some(3)), 3);
        assert_eq!(clean_concurrency(Some(1000)), 20);
    }

    #[test]
    fn clean_triggers_trims_filters_and_rejects_an_empty_required_field() {
        let cleaned = clean_triggers(Some(vec![AgentTaskTrigger {
            enabled: true,
            spec: TriggerSpec::WorkTransition {
                to_state: " ready ".into(),
                project: Some("  ".into()),
                item_kind: None,
                label: Some(" urgent ".into()),
                work_item: None,
            },
        }]))
        .expect("a valid trigger");
        match &cleaned[0].spec {
            TriggerSpec::WorkTransition {
                to_state,
                project,
                label,
                ..
            } => {
                assert_eq!(to_state, "ready");
                // A blank filter is no filter, not a filter on the empty string.
                assert!(project.is_none());
                assert_eq!(label.as_deref(), Some("urgent"));
            }
            other => panic!("wrong spec: {other:?}"),
        }

        // A work-transition with no destination state matches nothing and is
        // refused at write time.
        assert!(clean_triggers(Some(vec![AgentTaskTrigger {
            enabled: true,
            spec: TriggerSpec::WorkTransition {
                to_state: "  ".into(),
                project: None,
                item_kind: None,
                label: None,
                work_item: None,
            },
        }]))
        .is_err());
    }

    #[test]
    fn a_run_origin_from_an_event_names_the_trigger_and_the_event_id() {
        // The audit guarantee: a triggered run records which trigger fired and
        // which event caused it, so `why` can explain the run.
        let fire = TriggerFire {
            trigger_kind: "work-transition",
            bind_work_item: Some("WI-7".into()),
            description: "WI-7 entered ready".into(),
        };
        let origin = RunOrigin::event(fire, "work.transitioned", "wi_7", 4102);
        assert_eq!(origin.trigger, AgentTaskRunTrigger::Event);
        assert_eq!(origin.bind_work_item.as_deref(), Some("WI-7"));
        let detail = origin.detail.expect("an event origin carries a detail");
        assert_eq!(detail.trigger_kind, "work-transition");
        assert_eq!(detail.event_kind.as_deref(), Some("work.transitioned"));
        assert_eq!(detail.event_id.as_deref(), Some("wi_7"));
        assert_eq!(detail.event_seq, Some(4102));
        assert_eq!(detail.description, "WI-7 entered ready");
    }

    #[test]
    fn outcome_summary_is_phrased_by_verdict_and_keeps_the_old_wording() {
        assert_eq!(
            outcome_summary(AgentTaskRunOutcome::Succeeded, 0),
            "Exited successfully"
        );
        assert_eq!(
            outcome_summary(AgentTaskRunOutcome::Failed, 7),
            "Exited with status 7"
        );
        assert_eq!(
            outcome_summary(AgentTaskRunOutcome::Blocked, 137),
            "Blocked at an approval gate"
        );
    }

    // --- workflow-engine dispatch (#293) ------------------------------------

    fn test_config(state_dir: std::path::PathBuf) -> crate::config::Config {
        crate::config::Config {
            bind: "127.0.0.1:0".parse().unwrap(),
            token: "test-token-1234567890".into(),
            token_mutating_request_limit_per_minute: 600,
            extra_tokens: vec![],
            scrollback_bytes: 64 * 1024,
            default_shell: "/bin/bash".into(),
            default_cwd: std::env::temp_dir(),
            activity_idle_after_ms: 200,
            idle_stall_after_ms: 10 * 60 * 1_000,
            workspace_root: std::env::temp_dir(),
            gui_stream_url: None,
            gui_stream_verified: false,
            state_dir,
            fcm_service_account_json: None,
            vapid_subject: "mailto:test@example.invalid".into(),
            allowed_origins: vec![],
            auto_agent_auth: false,
            agent_auth_helper: "/usr/local/bin/mydevenv2-agent-auth".into(),
            session_templates: vec![],
            assistant_api_key: None,
            assistant_base_url: "http://unused.invalid".into(),
            assistant_model: "test-model".into(),
            assistant_max_tool_calls: 8,
            assistant_allow_claude_proxy: false,
            assistant_reasoning_effort: None,
            assistant_profiles: vec![],
            assistant_default_profile: None,
            assistant_log_retention_days: 30,
            assistant_stt_base_urls: vec![],
            assistant_stt_api_key: None,
            assistant_stt_model: "whisper-1".into(),
            assistant_tts_base_urls: vec![],
            assistant_tts_api_key: None,
            assistant_tts_model: "tts-1".into(),
            assistant_tts_voice: "alloy".into(),
            assistant_speech_attempt_timeout_ms: 30_000,
            public_url: None,
            vogt_core_url: None,
            vogt_import_root: None,
            vogt_engine_state_dir: None,
            vogt_core_token: None,
        }
    }

    fn test_registry() -> Arc<AgentTaskRegistry> {
        let dir = tempfile::tempdir().unwrap().keep();
        let cfg = Arc::new(test_config(dir.clone()));
        let bus = EventBus::default();
        let sessions = Arc::new(SessionRegistry::new(cfg, bus.clone(), None));
        let push = Arc::new(PushManager::new(&dir, None).unwrap());
        Arc::new(AgentTaskRegistry::new(&dir, sessions, push, bus).unwrap())
    }

    /// The non-fatal contract (#293): a task whose workflow engine cannot be
    /// reached records its run *errored* with a written reason and never
    /// panics — the create-run against a dead port fails, and the run is a
    /// durable `Failed` conclusion rather than a crash or a stuck `running`.
    #[tokio::test]
    async fn a_workflow_task_with_a_dead_engine_records_the_run_errored_non_fatally() {
        let registry = test_registry();
        let task = registry
            .create(AgentTaskCreate {
                name: "engine nightly".into(),
                prompt: "audit the repo".into(),
                schedule: None,
                triggers: None,
                concurrency: None,
                command: None,
                cwd: None,
                env: None,
                context: None,
                vogt_project: Some("vogt".into()),
                vogt_work_item: None,
                gates: None,
                auto_approve: None,
                output_schema: None,
                output_file: None,
                output_schema_max_retries: None,
                branch: None,
                workflow_engine: Some(WorkflowEngineConfig {
                    // Port 1 has nothing listening: the connect is refused fast.
                    engine_url: "http://127.0.0.1:1".into(),
                    workflow: "nightly".into(),
                    token_file: None,
                    repo_ref: None,
                }),
                enabled: None,
                notify_on_start: None,
                notify_on_phrase: None,
                auto_retry_on_rate_limit: None,
            })
            .expect("task created");

        // The workflow-engine config survived create unchanged.
        assert!(task.workflow_engine.is_some());

        let run = registry.run_now(task.id).await.expect("run recorded");
        assert_eq!(run.status, AgentTaskRunStatus::Errored);
        assert_eq!(run.outcome, Some(AgentTaskRunOutcome::Failed));
        let conclusion = run.conclusion.expect("a durable conclusion was written");
        assert_eq!(conclusion.outcome, AgentTaskRunOutcome::Failed);
        assert!(
            run.summary
                .as_deref()
                .unwrap_or_default()
                .contains("create-run failed"),
            "summary names the cause: {:?}",
            run.summary
        );

        // The stored task carries exactly this one errored run — no PTY session
        // was spawned, and the scheduler is untouched.
        let stored = registry.get(task.id).unwrap();
        assert_eq!(stored.runs.len(), 1);
        assert_eq!(stored.runs[0].status, AgentTaskRunStatus::Errored);
    }

    #[test]
    fn a_workflow_engine_config_round_trips_on_a_task() {
        let mut task = task_bound_to(Some("vogt"), Some("WI-7"));
        task.workflow_engine = Some(WorkflowEngineConfig {
            engine_url: "https://engine.internal".into(),
            workflow: "nightly-audit".into(),
            token_file: Some("/run/secrets/workflow-engine".into()),
            repo_ref: Some("main".into()),
        });
        let json = serde_json::to_string(&task).unwrap();
        let back: AgentTask = serde_json::from_str(&json).unwrap();
        let we = back
            .workflow_engine
            .expect("workflow_engine survives serde");
        assert_eq!(we.engine_url, "https://engine.internal");
        assert_eq!(we.workflow, "nightly-audit");
        assert_eq!(
            we.token_file.as_deref(),
            Some("/run/secrets/workflow-engine")
        );
        assert_eq!(we.repo_ref.as_deref(), Some("main"));

        // A task without the field omits it from the wire form entirely.
        let bare = task_bound_to(None, None);
        let wire = serde_json::to_value(&bare).unwrap();
        assert!(wire.get("workflow_engine").is_none());
    }
}
