use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use dashmap::DashMap;
use uuid::Uuid;
use vogt_engine_contract::{ActivityState, HibernateTrigger, Hibernation};

use crate::secret_broker::SecretBroker;

use crate::{
    agent_cli,
    config::Config,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    hibernation::{self, Record},
    history::{ArchiveRecord, SessionHistory},
    prompt_files,
    pty::{self, Session, SessionSpec, SessionSummary, SpawnDefaults},
    workspace_path,
};

/// The variable every session finds this engine's own URL in.
pub(crate) const ENGINE_URL_ENV: &str = "VOGT_ENGINE_URL";

/// The URL a process in this pod reaches the engine at: its bind address,
/// with a wildcard bind (`0.0.0.0`, `::`) read as loopback, since a session
/// runs beside the engine. Plain HTTP, because the engine serves no TLS.
/// `None` for port 0, where the port is chosen at bind time and the configured
/// address does not name it.
fn engine_self_url(bind: std::net::SocketAddr) -> Option<String> {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
    if bind.port() == 0 {
        return None;
    }
    let ip = match bind.ip() {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    Some(format!("http://{}", SocketAddr::new(ip, bind.port())))
}

/// A configured session template matched by name (case-insensitive) or,
/// failing that, by tag — so a caller can say `claude` and reach the template
/// tagged `claude`. When a tag matches more than one template an `agent`-tagged
/// one wins, then the alphabetically-first name, so the choice is deterministic
/// rather than list-order-dependent. An unknown name is refused with the
/// configured names, never quietly started as a shell.
fn resolve_template_in<'a>(
    templates: &'a [crate::config::SessionTemplate],
    name: &str,
) -> Result<&'a crate::config::SessionTemplate> {
    if let Some(t) = templates.iter().find(|t| t.name.eq_ignore_ascii_case(name)) {
        return Ok(t);
    }
    let mut by_tag: Vec<&crate::config::SessionTemplate> = templates
        .iter()
        .filter(|t| t.tags.iter().any(|tag| tag.eq_ignore_ascii_case(name)))
        .collect();
    by_tag.sort_by(|a, b| {
        let a_agent = a.tags.iter().any(|g| g.eq_ignore_ascii_case("agent"));
        let b_agent = b.tags.iter().any(|g| g.eq_ignore_ascii_case("agent"));
        b_agent.cmp(&a_agent).then_with(|| a.name.cmp(&b.name))
    });
    if let Some(t) = by_tag.first() {
        return Ok(t);
    }
    let known = templates
        .iter()
        .map(|t| t.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Err(ApiError::BadRequest(format!(
        "unknown session template {name:?}; configured: {known}"
    )))
}

pub struct SessionRegistry {
    cfg: Arc<Config>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    sessions: DashMap<Uuid, Arc<Session>>,
    /// Owned here because a broker token's lifetime is a session record's
    /// lifetime: issued as the session is created, revoked as it is
    /// forgotten.
    secret_broker: Arc<SecretBroker>,
    /// Every session's hibernation record, live or hibernated, mirrored from
    /// `state_dir/sessions` (see `hibernation`). A session is hibernated when
    /// its record says so and it has no live entry in `sessions`.
    records: Arc<DashMap<Uuid, Record>>,
    /// One lock per session, held across a hibernate or a wake, so two of
    /// them for one session never interleave.
    transitions: DashMap<Uuid, Arc<tokio::sync::Mutex<()>>>,
    /// Set once the engine is shutting down: from then on an exit is the
    /// engine stopping, never a session ending, and keeps its record.
    shutting_down: Arc<AtomicBool>,
}

const MAX_SESSION_NAME_BYTES: usize = 256;

/// Which sessions a hibernation may take, and how a session was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// `POST /api/sessions`, or vogt-core.
    Api,
    /// An agent-task run: its runner watches the session and reads its
    /// conclusion from it, so it is never hibernated or recorded.
    AgentTask,
}

/// What `SessionRegistry::create_inner` is doing.
enum Creating {
    Fresh(Origin),
    /// Starting a hibernated session again under its own id.
    Wake(Box<Record>),
}

impl SessionRegistry {
    pub fn new(cfg: Arc<Config>, bus: EventBus, history: Option<Arc<SessionHistory>>) -> Self {
        let secret_broker = Arc::new(SecretBroker::new(&cfg));
        let records = Arc::new(DashMap::new());
        recover_records(&cfg.state_dir, history.as_deref(), &records);
        Self {
            cfg,
            bus,
            history,
            sessions: DashMap::new(),
            secret_broker,
            records,
            transitions: DashMap::new(),
            shutting_down: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn secret_broker(&self) -> &Arc<SecretBroker> {
        &self.secret_broker
    }

    pub fn create(&self, spec: SessionSpec) -> Result<Arc<Session>> {
        self.create_inner(spec, Creating::Fresh(Origin::Api))
    }

    /// A session for an agent-task run: like [`Self::create`], but never
    /// recorded for hibernation — the run's runner owns its lifetime.
    pub fn create_for_task(&self, spec: SessionSpec) -> Result<Arc<Session>> {
        self.create_inner(spec, Creating::Fresh(Origin::AgentTask))
    }

    fn create_inner(&self, mut spec: SessionSpec, creating: Creating) -> Result<Arc<Session>> {
        spec.name = normalize_session_name(&spec.name)?;
        // Expand a template name into a command before anything downstream
        // reads `command`. Only when the caller gave no explicit command —
        // the GUI copies a template's command into the spec itself and sends
        // that, so it never takes this path. vogt-core sends the bare name
        // ("claude") and the deployment's config is where that becomes
        // `vogt-agent-auth run -- claude`, keeping the wrapper out of the
        // core and out of shipped code.
        if spec.command.is_none() {
            if let Some(name) = spec
                .template
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                let template = self.resolve_template(name)?;
                spec.command = template.command.clone();
                if !template.env.is_empty() {
                    let mut env = template.env.clone();
                    if let Some(existing) = spec.env.take() {
                        env.extend(existing);
                    }
                    spec.env = Some(env);
                }
            }
        }
        // What a hibernation record keeps: the command and environment as
        // the template and caller gave them, before the engine adds its own
        // launch arguments and variables (a wake adds those again).
        let base_command = spec.command.clone();
        let base_env = spec.env.clone().unwrap_or_default();

        // A resumed conversation starts where it ran, not where the caller
        // would have opened a fresh session: `claude --resume <id>` only finds
        // a conversation from the directory its transcript is keyed under,
        // which is often not a registered project root (`~/Working`, a
        // worktree) (WI-871). The transcript's own record of that directory
        // wins when it is inside the workspace; otherwise the caller's cwd is
        // kept and the CLI reports the conversation as not found.
        if let Some(dir) = self.resume_cwd(&spec) {
            spec.cwd = Some(dir);
        }
        // Resolve client-supplied cwd against workspace_root. Reject anything
        // that escapes the workspace via `..` so a stray API call can't spawn
        // a shell with cwd=/etc.
        if let Some(raw) = spec.cwd.as_deref() {
            let raw = raw.trim();
            if !raw.is_empty() {
                let canon =
                    workspace_path::resolve_existing_allow_absolute(&self.cfg.workspace_root, raw)
                        .map_err(|e| match e {
                            ApiError::BadRequest(msg) => {
                                ApiError::BadRequest(format!("cwd {raw:?}: {msg}"))
                            }
                            other => other,
                        })?;
                spec.cwd = Some(canon.to_string_lossy().into_owned());
            } else {
                spec.cwd = None;
            }
        }
        // Allocated here rather than inside `pty::spawn` so the prompt file
        // below can be named for the session it belongs to, so the file
        // exists before the child does, and so the agent's launch can name
        // both.
        let id = match &creating {
            Creating::Wake(record) => record.id,
            Creating::Fresh(_) => Uuid::new_v4(),
        };
        let brief = spec
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let brief_path = brief.map(|_| prompt_files::session_prompt_path(&self.cfg.state_dir, id));

        // Before the prompt file is written and before anything is spawned:
        // a request naming a model (or a conversation to resume) this command
        // cannot be told about is refused, not started plain. Doing it here
        // rather than in `pty::spawn` keeps the refusal free of side effects
        // to undo. An agent CLI started with a brief is also given its first
        // prompt here — a pointer to the file, so it begins the brief's task
        // instead of opening idle.
        let launch = agent_cli::launch(
            spec.command.as_deref(),
            &agent_cli::LaunchRequest {
                model: spec.model.as_deref(),
                effort: spec.effort.as_deref(),
                resume: spec.resume.as_deref(),
                brief_file: brief_path.as_deref(),
                session_id: Some(id),
            },
        )?;
        if let Some(rewritten) = launch.command {
            spec.command = Some(rewritten);
        }
        if !launch.env.is_empty() {
            // Defaults: first, so a template or caller that sets the same
            // variable still has the last word.
            let mut env = launch.env;
            env.extend(spec.env.take().unwrap_or_default());
            spec.env = Some(env);
        }

        let prompt_file = match brief {
            Some(text) => Some(prompt_files::write_session_prompt(
                &self.cfg.state_dir,
                id,
                text,
            )?),
            // A woken session is pointed at the brief it was first started
            // with, if that file is still there, but not told to read it
            // again: the conversation it resumes already has.
            None => match &creating {
                Creating::Wake(record) => record.brief_file.clone().filter(|p| p.is_file()),
                // A brief that is absent, empty, or all whitespace is no
                // brief: the child is left exactly as it was before this
                // field existed.
                Creating::Fresh(_) => None,
            },
        };
        let conversation = match &creating {
            Creating::Fresh(Origin::AgentTask) => None,
            _ => agent_cli::conversation(base_command.as_deref(), spec.resume.as_deref(), id),
        };
        if let Some(path) = prompt_file.as_ref() {
            // The child is told *where* the brief is, never handed the text.
            // A work item's brief runs to paragraphs of prose: as an argument
            // it would hit argv limits, need quoting no caller can be trusted
            // to get right, and stand in `ps` output for every process on the
            // box to read. A path is short, quoting-proof, and re-readable by
            // an agent that wants its instructions again later. Appended last
            // so the file the engine just wrote wins over a same-named
            // variable a caller supplied.
            spec.env.get_or_insert_with(Vec::new).push((
                prompt_files::PROMPT_FILE_ENV.to_string(),
                path.to_string_lossy().into_owned(),
            ));
        }

        // Every session is told where this engine is, so an agent in a
        // terminal opened from the GUI (which vogt-core never saw) can reach
        // the session APIs without reading engine source. First in the list:
        // a value the caller or a template sets — vogt-core passes the URL it
        // reaches the engine at — still has the last word.
        if let Some(url) = engine_self_url(self.cfg.bind) {
            spec.env
                .get_or_insert_with(Vec::new)
                .insert(0, (ENGINE_URL_ENV.to_string(), url));
        }

        // Names need not be unique — duplicates are merely confusing, not invalid.
        let spawned = pty::spawn(
            id,
            &spec,
            SpawnDefaults {
                default_shell: &self.cfg.default_shell,
                auto_agent_auth: self.cfg.auto_agent_auth,
                agent_auth_helper: &self.cfg.agent_auth_helper,
                default_cwd: &self.cfg.default_cwd,
                scrollback_bytes: self.cfg.scrollback_bytes,
                activity_idle_after_ms: self.cfg.activity_idle_after_ms,
                secret_broker: self.secret_broker.grant(id),
                on_exit: Some(self.exit_hook()),
            },
            self.bus.clone(),
            self.history.clone(),
        );
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(e) => {
                // A grant for a child that never started is a live token for
                // nobody; forget it with the rest of the failed spawn.
                self.secret_broker.revoke(id);
                // No child means nothing will ever read the brief — unless it
                // is a woken session's, which stays with its record.
                if prompt_file.is_some() && matches!(creating, Creating::Fresh(_)) {
                    prompt_files::remove_session_prompt(&self.cfg.state_dir, id);
                }
                return Err(e);
            }
        };
        let session = spawned.session;
        session.set_conversation(conversation.clone());

        // The record is written before the session is listed, so there is no
        // moment at which the engine runs a hibernatable session it would
        // forget on a SIGKILL. A failed write is logged, not fatal: the
        // session runs; it is only not recoverable.
        let record = match creating {
            Creating::Fresh(Origin::AgentTask) => None,
            Creating::Fresh(Origin::Api) => {
                let mut record = Record::new(id, session.name(), session.created_at_rfc3339());
                record.template = spec.template.clone();
                record.command = base_command;
                record.cwd = Some(session.cwd.clone());
                record.env = hibernation::without_secrets(&base_env);
                record.model = spec.model.clone();
                record.effort = spec.effort.clone();
                record.conversation = conversation;
                record.brief_file = prompt_file.clone();
                Some(record)
            }
            Creating::Wake(record) => {
                let mut record = *record;
                record.hibernation = None;
                record.name = session.name();
                session.set_keep_awake(record.keep_awake);
                hibernation::remove_screen(&self.cfg.state_dir, id);
                Some(record)
            }
        };
        if let Some(record) = record {
            if let Err(e) = hibernation::write(&self.cfg.state_dir, &record) {
                tracing::warn!(session = %id, error = %e, "could not write the session's hibernation record");
            }
            self.records.insert(id, record);
        }
        self.sessions.insert(session.id, Arc::clone(&session));
        // Provisional history row: written at spawn with `ended_at` and
        // `exit_code` NULL, so a long-lived session that is later SIGKILLed on
        // redeploy (never running `exit`) is still visible in the History tab.
        // The exit waiter upserts the real outcome later; the upsert's COALESCE
        // guard means this provisional write can never NULL a completed row,
        // even if it lands after the finalize under load. Metadata only — no
        // FTS write here, so it cannot wipe indexed output either.
        if let Some(history) = self.history.clone() {
            let record = ArchiveRecord {
                id: session.id,
                name: session.name(),
                created_at: session.created_at,
                ended_at: None,
                exit_code: None,
                cwd: Some(session.cwd.clone()),
                command: session.command(),
                scrollback_bytes: 0,
                end_reason: None,
            };
            let sid = session.id;
            tokio::spawn(async move {
                if let Err(e) = history.archive_session(record).await {
                    tracing::warn!(session = %sid, error = %e, "failed to record provisional history row");
                }
            });
        }
        self.bus.publish(ServerEvent::SessionCreated {
            id: session.id,
            name: session.name(),
        });
        Ok(session)
    }

    /// The workspace directory a resumed agent conversation ran in, read from
    /// its transcript under `$HOME`, or `None` when there is no resume, the
    /// command is not an agent CLI with readable transcripts, the transcript
    /// cannot be found, or its directory is not inside the workspace.
    fn resume_cwd(&self, spec: &SessionSpec) -> Option<String> {
        let id = spec.resume.as_deref().map(str::trim)?;
        // Used as a file name below, so only a well-formed id is looked up;
        // a malformed one is refused with its reason by `agent_cli::launch`.
        if !agent_cli::is_conversation_id(id) {
            return None;
        }
        let agent = agent_cli::agent_name(spec.command.as_deref()?)?;
        let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
        let dir = crate::transcripts::conversation_cwd(&agent, id, &home)?;
        let dir = dir.to_str()?;
        match workspace_path::resolve_existing_allow_absolute(&self.cfg.workspace_root, dir) {
            Ok(canon) => Some(canon.to_string_lossy().into_owned()),
            Err(e) => {
                tracing::info!(
                    resume = %id,
                    transcript_cwd = %dir,
                    error = %e,
                    "resumed conversation's directory is not usable; keeping the requested cwd"
                );
                None
            }
        }
    }

    /// Archive every live session to history before the process exits.
    ///
    /// Called from the engine's graceful-shutdown path (SIGTERM/SIGINT). On a
    /// redeploy the platform sends SIGTERM with a grace window; without this,
    /// the PTYs are SIGKILLed and long-lived agent shells that never `exit`
    /// leave no history row at all. Each session's row gets `ended_at` set and
    /// its output indexed, whether or not the child ever exited on its own.
    pub async fn drain_to_history(&self) {
        let Some(history) = self.history.as_ref() else {
            return;
        };
        let sessions = self.live_sessions();
        if sessions.is_empty() {
            return;
        }
        tracing::info!(count = sessions.len(), "draining live sessions to history");
        for session in sessions {
            pty::archive_live_session(&session, history).await;
        }
    }

    /// A live (or exited, still listed) session. A hibernated one is a
    /// `409` naming the way to wake it, so every route that needs a process
    /// — input, wait, resize, blocked — says why it cannot act rather than
    /// that the session does not exist.
    pub fn get(&self, id: Uuid) -> Result<Arc<Session>> {
        if let Some(s) = self.sessions.get(&id) {
            return Ok(Arc::clone(s.value()));
        }
        if self.is_hibernated(id) {
            return Err(ApiError::Conflict(format!(
                "session {id} is hibernated: its process was stopped to free memory. \
                 Wake it (POST /api/sessions/{id}/wake, or vogt's session.wake) first"
            )));
        }
        Err(ApiError::NotFound)
    }

    /// Whether the engine knows this session at all, live or hibernated.
    pub fn knows(&self, id: Uuid) -> bool {
        self.sessions.contains_key(&id) || self.records.contains_key(&id)
    }

    pub fn is_hibernated(&self, id: Uuid) -> bool {
        !self.sessions.contains_key(&id)
            && self
                .records
                .get(&id)
                .is_some_and(|r| r.hibernation.is_some())
    }

    /// A hibernated session's summary, or `None` when it is not hibernated.
    pub fn hibernated_summary(&self, id: Uuid) -> Option<SessionSummary> {
        if self.sessions.contains_key(&id) {
            return None;
        }
        let record = self.records.get(&id)?;
        let screen_len = hibernation::read_screen(&self.cfg.state_dir, id).len() as u64;
        hibernated_summary(&record, screen_len)
    }

    /// A hibernated session's kept output and the size to render it at,
    /// with its summary.
    pub fn hibernated_screen(&self, id: Uuid) -> Option<(bytes::Bytes, u16, u16, SessionSummary)> {
        if self.sessions.contains_key(&id) {
            return None;
        }
        let record = self.records.get(&id)?.clone();
        let bytes = hibernation::read_screen(&self.cfg.state_dir, id);
        let summary = hibernated_summary(&record, bytes.len() as u64)?;
        Some((
            bytes::Bytes::from(bytes),
            record.rows.unwrap_or(24),
            record.cols.unwrap_or(80),
            summary,
        ))
    }

    /// The hook a session's exit waiter calls: a session that ended — exited
    /// by itself or was killed — forgets its record, so it is not recovered
    /// as hibernated at the next boot. One that is hibernating, or that exits
    /// because the engine is shutting down, keeps it.
    fn exit_hook(&self) -> crate::pty::ExitHook {
        let records = Arc::clone(&self.records);
        let shutting_down = Arc::clone(&self.shutting_down);
        let state_dir = self.cfg.state_dir.clone();
        Box::new(move |session: &Session| {
            if session.is_hibernating() || shutting_down.load(Ordering::Acquire) {
                return;
            }
            records.remove(&session.id);
            hibernation::remove(&state_dir, session.id);
        })
    }

    fn transition_lock(&self, id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(
            self.transitions
                .entry(id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .value(),
        )
    }

    /// Why a live session cannot be hibernated, or `None` when it can.
    pub fn hibernate_refusal(&self, session: &Session, allow_shell: bool) -> Option<String> {
        if !session.is_alive() {
            return Some("the session has exited; there is nothing to hibernate".into());
        }
        let Some(record) = self.records.get(&session.id) else {
            return Some(
                "the session is an agent-task run, which its runner watches; it is never \
                 hibernated"
                    .into(),
            );
        };
        if record.conversation.is_none() && !allow_shell {
            return Some(
                "the engine does not know an agent conversation to resume for this session \
                 (a shell, or an agent CLI whose conversation id it did not pin — a fresh \
                 Codex or OpenCode session); pass allow_shell to hibernate it anyway, and \
                 it will wake as a fresh process in the same directory"
                    .into(),
            );
        }
        None
    }

    /// Stop a live session's process tree and keep it listed as hibernated,
    /// to be woken later by resuming its agent conversation under the same
    /// id. Hibernating a hibernated session returns it unchanged.
    pub async fn hibernate(
        &self,
        id: Uuid,
        reason: Option<String>,
        trigger: HibernateTrigger,
        allow_shell: bool,
    ) -> Result<SessionSummary> {
        let lock = self.transition_lock(id);
        let _held = lock.lock().await;
        let Some(session) = self.sessions.get(&id).map(|s| Arc::clone(s.value())) else {
            return self.hibernated_summary(id).ok_or(ApiError::NotFound);
        };
        if let Some(refusal) = self.hibernate_refusal(&session, allow_shell) {
            return Err(ApiError::Conflict(refusal));
        }
        let mut record = self
            .records
            .get(&id)
            .map(|r| r.clone())
            .ok_or(ApiError::NotFound)?;

        // The screen and the record are written before anything is stopped:
        // a failure here leaves the session running and says so.
        let (screen, rows, cols) = session.screen_source(hibernation::SCREEN_BYTES);
        record.name = session.name();
        record.keep_awake = session.keep_awake();
        record.rows = Some(rows);
        record.cols = Some(cols);
        record.hibernation = Some(Hibernation {
            at: now_rfc3339(),
            trigger,
            reason: reason
                .map(|r| r.trim().chars().take(500).collect::<String>())
                .filter(|r| !r.is_empty()),
            resumable: record.conversation.is_some(),
        });
        hibernation::write_screen(&self.cfg.state_dir, id, &screen)
            .and_then(|()| hibernation::write(&self.cfg.state_dir, &record))
            .map_err(|e| ApiError::Internal(format!("write the hibernation record: {e}")))?;
        session.mark_hibernating();
        self.records.insert(id, record);

        match session.pid() {
            Some(pid) => {
                let watched = Arc::clone(&session);
                hibernation::stop_tree(pid, hibernation::STOP_GRACE, move || !watched.is_alive())
                    .await
            }
            None => {
                let _ = session.kill();
            }
        }
        // The exit waiter records the exit; give it a moment so the history
        // row it writes is the one that says `hibernated`.
        for _ in 0..40 {
            if !session.is_alive() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        self.sessions.remove(&id);
        self.secret_broker.revoke(id);
        self.bus
            .publish(ServerEvent::SessionHibernated { id, trigger });
        tracing::info!(session = %id, ?trigger, "session hibernated");
        self.hibernated_summary(id).ok_or(ApiError::NotFound)
    }

    /// Start a hibernated session again, under the same id, by resuming its
    /// agent conversation in the directory it ran in. `env` is set on top of
    /// the recorded environment (which holds no secrets). Waking a live
    /// session returns it as it is.
    pub async fn wake(
        &self,
        id: Uuid,
        req: vogt_engine_contract::WakeRequest,
    ) -> Result<Arc<Session>> {
        let lock = self.transition_lock(id);
        let _held = lock.lock().await;
        if let Some(session) = self.sessions.get(&id).map(|s| Arc::clone(s.value())) {
            if session.is_alive() {
                return Ok(session);
            }
            return Err(ApiError::Conflict(
                "the session has exited; start a new one (session_start with resume) instead"
                    .into(),
            ));
        }
        let record = self
            .records
            .get(&id)
            .map(|r| r.clone())
            .filter(|r| r.hibernation.is_some())
            .ok_or(ApiError::NotFound)?;
        let mut env = record.env.clone();
        env.extend(req.env.unwrap_or_default());
        let spec = SessionSpec {
            name: record.name.clone(),
            command: record.command.clone(),
            template: record.template.clone(),
            cwd: record.cwd.clone(),
            env: Some(env),
            prompt: None,
            model: record.model.clone(),
            effort: record.effort.clone(),
            resume: record.conversation.as_ref().map(|c| c.id.clone()),
            cols: req.cols.or(record.cols),
            rows: req.rows.or(record.rows),
            scrollback_bytes: None,
        };
        let session = self.create_inner(spec, Creating::Wake(Box::new(record)))?;
        self.bus.publish(ServerEvent::SessionWoken { id });
        tracing::info!(session = %id, "session woken");
        Ok(session)
    }

    /// Pin a session awake (or unpin it), live or hibernated.
    pub fn set_keep_awake(&self, id: Uuid, keep: bool) -> Result<SessionSummary> {
        let live = self.sessions.get(&id).map(|s| Arc::clone(s.value()));
        if let Some(session) = live.as_ref() {
            session.set_keep_awake(keep);
        }
        let updated = self.records.get_mut(&id).map(|mut record| {
            record.keep_awake = keep;
            record.clone()
        });
        match (&live, updated) {
            (_, Some(record)) => {
                if let Err(e) = hibernation::write(&self.cfg.state_dir, &record) {
                    tracing::warn!(session = %id, error = %e, "could not write the session's hibernation record");
                }
            }
            (Some(_), None) => {}
            (None, None) => return Err(ApiError::NotFound),
        }
        match live {
            Some(session) => Ok(session.summary()),
            None => self.hibernated_summary(id).ok_or(ApiError::NotFound),
        }
    }

    /// Hibernate every live session that can be, as the engine shuts down,
    /// so a redeploy leaves them listed and wakeable rather than gone. From
    /// here on no exit forgets a record. Sessions that cannot be hibernated
    /// (shells, agent-task runs) are left to the history drain.
    pub async fn hibernate_for_shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        let candidates: Vec<Uuid> = self
            .live_sessions()
            .into_iter()
            .filter(|s| self.hibernate_refusal(s, false).is_none())
            .map(|s| s.id)
            .collect();
        if candidates.is_empty() {
            return;
        }
        tracing::info!(
            count = candidates.len(),
            "hibernating live sessions for shutdown"
        );
        let results = futures_util::future::join_all(candidates.into_iter().map(|id| async move {
            (
                id,
                self.hibernate(
                    id,
                    Some("the engine was shutting down".into()),
                    HibernateTrigger::Shutdown,
                    false,
                )
                .await,
            )
        }))
        .await;
        for (id, result) in results {
            if let Err(e) = result {
                tracing::warn!(session = %id, error = %e, "could not hibernate a session for shutdown; its record stays for recovery");
            }
        }
    }

    /// Live session handles, for internal watchers (idle-stall, phrase
    /// watchers) that need more than the summary snapshot.
    pub fn live_sessions(&self) -> Vec<Arc<Session>> {
        self.sessions
            .iter()
            .map(|kv| Arc::clone(kv.value()))
            .collect()
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        let mut out: Vec<_> = self
            .sessions
            .iter()
            .map(|kv| kv.value().summary())
            .collect();
        let hibernated: Vec<Uuid> = self
            .records
            .iter()
            .filter(|r| r.hibernation.is_some() && !self.sessions.contains_key(r.key()))
            .map(|r| *r.key())
            .collect();
        out.extend(
            hibernated
                .into_iter()
                .filter_map(|id| self.hibernated_summary(id)),
        );
        out.sort_by_key(|s| s.created_at.clone());
        out
    }

    /// A configured session template matched by name (case-insensitive) or,
    /// failing that, by tag — so a caller can say `claude` and reach the
    /// template tagged `claude`. When a tag matches more than one template an
    /// `agent`-tagged one wins, then the alphabetically-first name, so the
    /// choice is deterministic rather than list-order-dependent. An unknown
    /// name is refused with the configured names, never started as a shell.
    fn resolve_template(&self, name: &str) -> Result<&crate::config::SessionTemplate> {
        resolve_template_in(&self.cfg.session_templates, name)
    }

    pub fn rename(&self, id: Uuid, new_name: String) -> Result<()> {
        let new_name = normalize_session_name(&new_name)?;
        let live = self.sessions.get(&id).map(|s| Arc::clone(s.value()));
        if live.is_none() && !self.records.contains_key(&id) {
            return Err(ApiError::NotFound);
        }
        if let Some(s) = live {
            s.rename(new_name.clone());
        }
        if let Some(mut record) = self.records.get_mut(&id) {
            record.name = new_name.clone();
            if let Err(e) = hibernation::write(&self.cfg.state_dir, &record) {
                tracing::warn!(session = %id, error = %e, "could not write the session's hibernation record");
            }
        }
        self.bus
            .publish(ServerEvent::SessionRenamed { id, name: new_name });
        Ok(())
    }

    /// Sends SIGKILL to the child but keeps the session in the registry so
    /// callers can still inspect scrollback. Use `remove` to forget it entirely.
    ///
    /// A hibernated session has no process to kill; killing it means it is
    /// not to come back, so it is forgotten — record, kept screen and brief.
    pub fn kill(&self, id: Uuid) -> Result<()> {
        if self.is_hibernated(id) {
            return self.remove(id);
        }
        let s = self.get(id)?;
        s.kill()?;
        Ok(())
    }

    pub fn remove(&self, id: Uuid) -> Result<()> {
        let live = self.sessions.remove(&id).map(|(_, v)| v);
        let recorded = self.records.remove(&id).is_some();
        hibernation::remove(&self.cfg.state_dir, id);
        if live.is_none() && !recorded {
            return Err(ApiError::NotFound);
        }
        if let Some(s) = live {
            let _ = s.kill();
        }
        // Forgetting the session forgets its leave to ask the broker.
        self.secret_broker.revoke(id);
        // The brief outlives the child on purpose — a killed session is still
        // inspectable, and an agent may re-read its prompt after a restart —
        // but not the session record. Forgetting the session forgets its
        // prompt. Anything left behind by a crash or a server restart is
        // collected by the agent-task artifact cleanup
        // (`POST /api/agent-tasks/artifacts/cleanup`), which sweeps prompt
        // files whose session the registry no longer knows.
        prompt_files::remove_session_prompt(&self.cfg.state_dir, id);
        Ok(())
    }
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// A hibernated record as the wire lists it.
fn hibernated_summary(record: &Record, screen_bytes: u64) -> Option<SessionSummary> {
    let hibernation = record.hibernation.clone()?;
    Some(SessionSummary {
        id: record.id,
        name: record.name.clone(),
        activity: ActivityState::Hibernated,
        exit_code: None,
        alive: false,
        scrollback_bytes: screen_bytes,
        cwd: record.cwd.clone().unwrap_or_default(),
        command: record.command.as_ref().map(|argv| argv.join(" ")),
        created_at: record.created_at.clone(),
        activity_changed_at: hibernation.at.clone(),
        turn_started_at: None,
        last_output_at: None,
        approval: None,
        blocked: None,
        conversation: record.conversation.clone(),
        hibernation: Some(hibernation),
        keep_awake: record.keep_awake,
    })
}

/// Load the records left in `state_dir` and turn every one without a process
/// — at boot that is all of them — into a hibernated session. One the engine
/// had already hibernated keeps its trigger; one it never got to (a SIGKILL,
/// a crash) is `recovered`, with its screen taken from the history log when
/// there is one. A record with no conversation to resume that was not
/// hibernated on purpose (a shell the engine lost) is forgotten.
fn recover_records(
    state_dir: &std::path::Path,
    history: Option<&SessionHistory>,
    records: &DashMap<Uuid, Record>,
) {
    for mut record in hibernation::load_all(state_dir) {
        let id = record.id;
        if record.hibernation.is_none() {
            if record.conversation.is_none() {
                hibernation::remove(state_dir, id);
                continue;
            }
            record.hibernation = Some(Hibernation {
                at: now_rfc3339(),
                trigger: HibernateTrigger::Recovered,
                reason: Some("the engine stopped without hibernating it".into()),
                resumable: true,
            });
            if hibernation::read_screen(state_dir, id).is_empty() {
                if let Some(tail) = history.and_then(|h| log_tail(&h.log_path(id))) {
                    let _ = hibernation::write_screen(state_dir, id, &tail);
                }
            }
            if let Err(e) = hibernation::write(state_dir, &record) {
                tracing::warn!(session = %id, error = %e, "could not update a recovered session record");
            }
            tracing::info!(session = %id, name = %record.name, "recovered a session as hibernated");
        }
        records.insert(id, record);
    }
}

/// The last `SCREEN_BYTES` of a session's raw output log.
fn log_tail(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let max = hibernation::SCREEN_BYTES as u64;
    if len > max {
        file.seek(SeekFrom::Start(len - max)).ok()?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn normalize_session_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(ApiError::BadRequest("name must not be empty".into()));
    }
    let len = trimmed.len();
    if len > MAX_SESSION_NAME_BYTES {
        return Err(ApiError::BadRequest(format!(
            "name must be at most {MAX_SESSION_NAME_BYTES} bytes after trimming (got {len})"
        )));
    }
    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::normalize_session_name;

    #[test]
    fn the_engine_url_is_its_bind_address_with_wildcards_on_loopback() {
        let url = |bind: &str| super::engine_self_url(bind.parse().unwrap());
        assert_eq!(
            url("0.0.0.0:8910").as_deref(),
            Some("http://127.0.0.1:8910")
        );
        assert_eq!(
            url("127.0.0.1:9001").as_deref(),
            Some("http://127.0.0.1:9001")
        );
        assert_eq!(url("[::]:8910").as_deref(), Some("http://[::1]:8910"));
        assert_eq!(
            url("10.1.2.3:8910").as_deref(),
            Some("http://10.1.2.3:8910")
        );
        // An ephemeral port is not known until bind time.
        assert_eq!(url("127.0.0.1:0"), None);
    }

    #[test]
    fn trims_session_names() {
        assert_eq!(
            normalize_session_name("  spaced shell  ").unwrap(),
            "spaced shell"
        );
    }

    #[test]
    fn rejects_empty_session_names() {
        let err = normalize_session_name("   ").unwrap_err();
        assert!(err.to_string().contains("name must not be empty"));
    }

    #[test]
    fn rejects_names_over_byte_limit() {
        let long = "a".repeat(257);
        let err = normalize_session_name(&long).unwrap_err();
        assert!(err.to_string().contains("at most 256 bytes"));
    }
    #[test]
    fn a_template_resolves_by_name_then_by_tag() {
        use crate::config::SessionTemplate;
        let templates = SessionTemplate::default_templates();
        // Exact name, case-insensitive.
        assert_eq!(
            super::resolve_template_in(&templates, "claude code (protected)")
                .unwrap()
                .name,
            "Claude Code (protected)"
        );
        // By tag: `claude` reaches the protected Claude template, whose
        // command is the deployment's wrapper — the whole point.
        let claude = super::resolve_template_in(&templates, "claude").unwrap();
        assert_eq!(claude.name, "Claude Code (protected)");
        assert!(claude
            .command
            .as_ref()
            .unwrap()
            .iter()
            .any(|arg| arg == "claude"));
        // A plain shell is still reachable by name.
        assert_eq!(
            super::resolve_template_in(&templates, "Shell")
                .unwrap()
                .name,
            "Shell"
        );
    }

    #[test]
    fn an_unknown_template_is_refused_with_the_configured_names() {
        use crate::config::SessionTemplate;
        let templates = SessionTemplate::default_templates();
        let err = super::resolve_template_in(&templates, "kardashian").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown session template"), "{msg}");
        assert!(msg.contains("Claude Code (protected)"), "{msg}");
    }

    #[test]
    fn the_agent_tag_wins_when_a_tag_is_ambiguous() {
        use crate::config::SessionTemplate;
        // Two templates share a tag; the agent one must win deterministically.
        let templates = vec![
            SessionTemplate {
                name: "Zeta Shell".into(),
                description: String::new(),
                command: Some(vec!["bash".into()]),
                cwd: None,
                env: vec![],
                default_name: None,
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["claude".into()],
            },
            SessionTemplate {
                name: "Alpha Agent".into(),
                description: String::new(),
                command: Some(vec![
                    "vogt-agent-auth".into(),
                    "run".into(),
                    "--".into(),
                    "claude".into(),
                ]),
                cwd: None,
                env: vec![],
                default_name: None,
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["agent".into(), "claude".into()],
            },
        ];
        assert_eq!(
            super::resolve_template_in(&templates, "claude")
                .unwrap()
                .name,
            "Alpha Agent"
        );
    }
}
