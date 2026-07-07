use std::{
    collections::HashSet,
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
use uuid::Uuid;

use crate::{
    activity::strip_ansi,
    app::AppState,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    pty::{Session, SessionSpec},
    push::PushManager,
    sessions::SessionRegistry,
};

const TASKS_FILE: &str = "agent-tasks.json";
const PROMPT_DIR: &str = "agent-task-prompts";
const DEFAULT_NOTIFY_PHRASE: &str = "MYDEVENV2_NOTIFY:";

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
    #[serde(default)]
    pub notify_on_start: bool,
    #[serde(default = "default_notify_phrase")]
    pub notify_on_phrase: Option<String>,
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
    pub enabled: Option<bool>,
    #[serde(default)]
    pub notify_on_start: Option<bool>,
    #[serde(default)]
    pub notify_on_phrase: Option<String>,
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
    pub enabled: Option<bool>,
    #[serde(default)]
    pub notify_on_start: Option<bool>,
    #[serde(default)]
    pub notify_on_phrase: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentTaskStore {
    #[serde(default)]
    tasks: Vec<AgentTask>,
}

pub struct AgentTaskRegistry {
    path: PathBuf,
    prompt_dir: PathBuf,
    sessions: Arc<SessionRegistry>,
    push: Arc<PushManager>,
    tasks: Mutex<Vec<AgentTask>>,
    executing: Mutex<HashSet<Uuid>>,
}

impl AgentTaskRegistry {
    pub fn new(
        state_dir: &Path,
        sessions: Arc<SessionRegistry>,
        push: Arc<PushManager>,
    ) -> Result<Self> {
        std::fs::create_dir_all(state_dir)?;
        let prompt_dir = state_dir.join(PROMPT_DIR);
        std::fs::create_dir_all(&prompt_dir)?;
        let path = state_dir.join(TASKS_FILE);
        let tasks = load_tasks(&path)?;
        let registry = Self {
            path,
            prompt_dir,
            sessions,
            push,
            tasks: Mutex::new(tasks),
            executing: Mutex::new(HashSet::new()),
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
            notify_on_start: req.notify_on_start.unwrap_or(false),
            notify_on_phrase: clean_notify_phrase(req.notify_on_phrase),
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
        Ok(true)
    }

    pub async fn run_now(&self, id: Uuid) -> Result<AgentTaskRun> {
        self.start_run(id, AgentTaskRunTrigger::Manual)
            .await?
            .ok_or_else(|| ApiError::Conflict("task did not start".into()))
    }

    async fn run_due_once(&self) {
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
        &self,
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
        &self,
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
            "MYDEVENV2_AGENT_TASK_PROMPT_FILE".to_string(),
            prompt_file_display.clone(),
        ));
        env.push((
            "MYDEVENV2_AGENT_TASK_CONTEXT_FILE".to_string(),
            context_file_display.clone(),
        ));

        let session_name = format!("[Task] {}", task.name);
        let session = self.sessions.create(SessionSpec {
            name: session_name.clone(),
            command: Some(command),
            cwd: task.cwd.clone(),
            env: Some(env),
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
                .notify_all("Scheduled agent started", &body, data)
                .await;
        }

        if let Some(phrase) = task
            .notify_on_phrase
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
        {
            spawn_phrase_watcher(
                Arc::clone(&self.push),
                Arc::clone(&session),
                task.name.clone(),
                task.id,
                run.id,
                phrase,
                session.subscribe(),
            );
        }

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

        let prompt_file = task_dir.join(format!("{run_id}.md"));
        let mut prompt = String::new();
        prompt.push_str("# MyDevEnv2 Scheduled Agent Task\n\n");
        prompt.push_str(&format!("Task: {}\n", task.name));
        prompt.push_str(&format!("Task ID: {}\n", task.id));
        prompt.push_str(&format!("Run ID: {run_id}\n"));
        prompt.push_str(&format!("Run time (UTC): {now}\n\n"));
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
        std::fs::write(&prompt_file, prompt)?;
        Ok((prompt_file, context_file))
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

fn default_command() -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-lc".to_string(),
        "printf 'MyDevEnv2 scheduled agent task\\n\\n'; cat \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\"; printf '\\nPrompt file: %s\\nContext file: %s\\n' \"$MYDEVENV2_AGENT_TASK_PROMPT_FILE\" \"$MYDEVENV2_AGENT_TASK_CONTEXT_FILE\"; exec \"${SHELL:-/bin/bash}\"".to_string(),
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

fn spawn_phrase_watcher(
    push: Arc<PushManager>,
    session: Arc<Session>,
    task_name: String,
    task_id: Uuid,
    run_id: Uuid,
    phrase: String,
    mut rx: tokio::sync::broadcast::Receiver<crate::pty::OutputChunk>,
) {
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
            if let Some(idx) = tail.find(&phrase) {
                let msg = tail[idx + phrase.len()..]
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
                let data = json!({
                    "kind": "agent-task-notify",
                    "task_id": task_id.to_string(),
                    "run_id": run_id.to_string(),
                    "session_id": session.id.to_string(),
                    "url": format!("/#/t/{}", session.id),
                });
                let title = format!("{task_name} update");
                let _ = push.notify_all(&title, &body, data).await;
                break;
            }
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
}
