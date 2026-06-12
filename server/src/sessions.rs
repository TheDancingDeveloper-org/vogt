use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::{
    config::Config,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    pty::{self, Session, SessionSpec, SessionSummary, SpawnDefaults},
};

pub struct SessionRegistry {
    cfg: Arc<Config>,
    bus: EventBus,
    sessions: DashMap<Uuid, Arc<Session>>,
}

impl SessionRegistry {
    pub fn new(cfg: Arc<Config>, bus: EventBus) -> Self {
        Self {
            cfg,
            bus,
            sessions: DashMap::new(),
        }
    }

    pub fn create(&self, mut spec: SessionSpec) -> Result<Arc<Session>> {
        if spec.name.trim().is_empty() {
            return Err(ApiError::BadRequest("name must not be empty".into()));
        }
        // Resolve client-supplied cwd against workspace_root. Reject anything
        // that escapes the workspace via `..` so a stray API call can't spawn
        // a shell with cwd=/etc.
        if let Some(raw) = spec.cwd.as_deref() {
            let raw = raw.trim();
            if !raw.is_empty() {
                let candidate = std::path::Path::new(raw);
                let abs = if candidate.is_absolute() {
                    candidate.to_path_buf()
                } else {
                    self.cfg.workspace_root.join(raw.trim_start_matches('/'))
                };
                let canon = abs
                    .canonicalize()
                    .map_err(|e| ApiError::BadRequest(format!("cwd {raw:?}: {e}")))?;
                let root = self
                    .cfg
                    .workspace_root
                    .canonicalize()
                    .unwrap_or_else(|_| self.cfg.workspace_root.clone());
                if !canon.starts_with(&root) {
                    return Err(ApiError::BadRequest(format!(
                        "cwd {raw:?} escapes workspace_root"
                    )));
                }
                spec.cwd = Some(canon.to_string_lossy().into_owned());
            } else {
                spec.cwd = None;
            }
        }
        // Names need not be unique — duplicates are merely confusing, not invalid.
        let spawned = pty::spawn(
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
        )?;
        let session = spawned.session;
        self.sessions.insert(session.id, Arc::clone(&session));
        self.bus.publish(ServerEvent::SessionCreated {
            id: session.id,
            name: session.name(),
        });
        Ok(session)
    }

    pub fn get(&self, id: Uuid) -> Result<Arc<Session>> {
        self.sessions
            .get(&id)
            .map(|s| Arc::clone(s.value()))
            .ok_or(ApiError::NotFound)
    }

    pub fn list(&self) -> Vec<SessionSummary> {
        let mut out: Vec<_> = self
            .sessions
            .iter()
            .map(|kv| kv.value().summary())
            .collect();
        out.sort_by_key(|s| s.created_at);
        out
    }

    pub fn rename(&self, id: Uuid, new_name: String) -> Result<()> {
        let s = self.get(id)?;
        if new_name.trim().is_empty() {
            return Err(ApiError::BadRequest("name must not be empty".into()));
        }
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
        Ok(())
    }
}
