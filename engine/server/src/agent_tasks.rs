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
};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTask {
    pub id: Uuid,
    pub name: String,
    pub prompt: String,
    pub schedule: AgentTaskSchedule,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentTaskRun {
    pub id: Uuid,
    pub task_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    pub trigger: AgentTaskRunTrigger,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskRunTrigger {
    Manual,
    Scheduled,
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

#[derive(Debug, Deserialize)]
pub struct AgentTaskCreate {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub schedule: Option<AgentTaskSchedule>,
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
            status,
            command: clean_command(req.command),
            cwd: clean_optional(req.cwd),
            env: req.env.unwrap_or_default(),
            context: clean_optional(req.context),
            vogt_project: clean_optional(req.vogt_project),
            vogt_work_item: clean_optional(req.vogt_work_item),
            gates,
            auto_approve: req.auto_approve.unwrap_or(false),
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
        self.start_run(id, AgentTaskRunTrigger::Manual)
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
            if let Err(e) = self.start_run(id, AgentTaskRunTrigger::Scheduled).await {
                tracing::warn!(task = %id, error = %e, "scheduled agent task failed to start");
                let _ = self.advance_next_run(id);
            }
        }
    }

    async fn start_run(
        self: &Arc<Self>,
        id: Uuid,
        trigger: AgentTaskRunTrigger,
    ) -> Result<Option<AgentTaskRun>> {
        {
            let mut executing = self.executing.lock();
            if !executing.insert(id) {
                if matches!(trigger, AgentTaskRunTrigger::Scheduled) {
                    let _ = self.advance_next_run(id);
                    return Ok(None);
                }
                return Err(ApiError::Conflict("task is already starting".into()));
            }
        }

        let result = self.start_run_inner(id, trigger).await;
        self.executing.lock().remove(&id);
        result
    }

    async fn start_run_inner(
        self: &Arc<Self>,
        id: Uuid,
        trigger: AgentTaskRunTrigger,
    ) -> Result<Option<AgentTaskRun>> {
        let task = self.get(id)?;
        if task.status != AgentTaskStatus::Active
            && matches!(trigger, AgentTaskRunTrigger::Scheduled)
        {
            return Ok(None);
        }
        if self.has_running_session(&task) {
            if matches!(trigger, AgentTaskRunTrigger::Scheduled) {
                self.advance_next_run(id)?;
                return Ok(None);
            }
            return Err(ApiError::Conflict(
                "latest task session is still running".into(),
            ));
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

        let run = AgentTaskRun {
            id: run_id,
            task_id: task.id,
            started_at: now,
            trigger,
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

        Ok(Some(run))
    }

    fn has_running_session(&self, task: &AgentTask) -> bool {
        task.runs
            .iter()
            .rev()
            .filter_map(|run| self.sessions.get(run.session_id).ok())
            .any(|session| session.exit_code().is_none())
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
        let mut tasks = self.tasks.lock();
        let mut changed = false;

        for task in tasks.iter_mut() {
            if let Some(run) = task
                .runs
                .iter_mut()
                .rev()
                .find(|run| run.session_id == session_id)
            {
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
                run.summary = Some(if code == 0 {
                    "Exited successfully".to_string()
                } else {
                    format!("Exited with status {code}")
                });
                task.updated_at = completed_at;
                changed = true;
                break;
            }
        }

        if changed {
            self.save_locked(&tasks)?;
        }
        Ok(())
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
        let record = self.resolve_gate_answer(run_id, gate_id, option_index, &actor, false, reason)?;
        control.notify.notify_one();
        Ok(record)
    }

    /// The in-flight run of a task and its live control, or a 409 when the task
    /// has none running.
    fn live_run(&self, task_id: Uuid) -> Result<(Uuid, Arc<RunControl>)> {
        let run_ids: Vec<Uuid> = {
            let tasks = self.tasks.lock();
            let task = tasks.iter().find(|t| t.id == task_id).ok_or(ApiError::NotFound)?;
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
        Err(ApiError::Conflict("task has no run in flight to steer".into()))
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
            gate.answer(option_index, actor, auto, now).map_err(|e| match e {
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
        let Some(gate) = task.runs[run_idx].gates.iter_mut().find(|g| g.id == gate_id) else {
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
            match tasks
                .iter()
                .find_map(|t| t.runs.iter().find(|r| r.id == run_id).map(|r| (t.id, r.session_id)))
            {
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

pub async fn run_now(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<Uuid>,
) -> Result<Json<AgentTaskRun>> {
    Ok(Json(state.agent_tasks.run_now(id).await?))
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
    state
        .agent_tasks
        .steer(id, req.text, req.interrupt, actor, clean_optional(req.reason))?;
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
    let record =
        state
            .agent_tasks
            .answer_gate(id, gate_id, req.option, actor, clean_optional(req.reason))?;
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
            status: AgentTaskStatus::Active,
            command: None,
            cwd: None,
            env: vec![],
            context: None,
            vogt_project: project.map(str::to_string),
            vogt_work_item: work_item.map(str::to_string),
            gates: vec![],
            auto_approve: false,
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
}
