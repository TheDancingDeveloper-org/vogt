use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::{
    agent_cli,
    config::Config,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    history::{ArchiveRecord, SessionHistory},
    prompt_files,
    pty::{self, Session, SessionSpec, SessionSummary, SpawnDefaults},
    workspace_path,
};

pub struct SessionRegistry {
    cfg: Arc<Config>,
    bus: EventBus,
    history: Option<Arc<SessionHistory>>,
    sessions: DashMap<Uuid, Arc<Session>>,
}

const MAX_SESSION_NAME_BYTES: usize = 256;

impl SessionRegistry {
    pub fn new(cfg: Arc<Config>, bus: EventBus, history: Option<Arc<SessionHistory>>) -> Self {
        Self {
            cfg,
            bus,
            history,
            sessions: DashMap::new(),
        }
    }

    pub fn create(&self, mut spec: SessionSpec) -> Result<Arc<Session>> {
        spec.name = normalize_session_name(&spec.name)?;
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
        // Before the prompt file is written and before anything is spawned:
        // a request naming a model this command cannot be told about is
        // refused, not started plain (FR-T11). Doing it here rather than in
        // `pty::spawn` keeps the refusal free of side effects to undo.
        if let Some(rewritten) = agent_cli::apply(
            spec.command.as_deref(),
            spec.model.as_deref(),
            spec.effort.as_deref(),
        )? {
            spec.command = Some(rewritten);
        }

        // Allocated here rather than inside `pty::spawn` so the prompt file
        // below can be named for the session it belongs to, and so the file
        // exists before the child does.
        let id = Uuid::new_v4();
        let prompt_file = match spec
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            Some(text) => Some(prompt_files::write_session_prompt(
                &self.cfg.state_dir,
                id,
                text,
            )?),
            // A brief that is absent, empty, or all whitespace is no brief:
            // the child is left exactly as it was before this field existed.
            None => None,
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
            },
            self.bus.clone(),
            self.history.clone(),
        );
        let spawned = match spawned {
            Ok(spawned) => spawned,
            Err(e) => {
                // No child means nothing will ever read the brief.
                if prompt_file.is_some() {
                    prompt_files::remove_session_prompt(&self.cfg.state_dir, id);
                }
                return Err(e);
            }
        };
        let session = spawned.session;
        self.sessions.insert(session.id, Arc::clone(&session));
        // Provisional history row (#475): written at spawn with `ended_at` and
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

    /// Archive every live session to history before the process exits (#475).
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

    pub fn get(&self, id: Uuid) -> Result<Arc<Session>> {
        self.sessions
            .get(&id)
            .map(|s| Arc::clone(s.value()))
            .ok_or(ApiError::NotFound)
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
        out.sort_by_key(|s| s.created_at.clone());
        out
    }

    pub fn rename(&self, id: Uuid, new_name: String) -> Result<()> {
        let s = self.get(id)?;
        let new_name = normalize_session_name(&new_name)?;
        s.rename(new_name.clone());
        self.bus
            .publish(ServerEvent::SessionRenamed { id, name: new_name });
        Ok(())
    }

    /// Sends SIGKILL to the child but keeps the session in the registry so
    /// callers can still inspect scrollback. Use `remove` to forget it entirely.
    pub fn kill(&self, id: Uuid) -> Result<()> {
        let s = self.get(id)?;
        s.kill()?;
        Ok(())
    }

    pub fn remove(&self, id: Uuid) -> Result<()> {
        let s = self
            .sessions
            .remove(&id)
            .map(|(_, v)| v)
            .ok_or(ApiError::NotFound)?;
        let _ = s.kill();
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
}
