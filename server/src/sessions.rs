use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

use crate::{
    config::Config,
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    pty::{self, Session, SessionSpec, SessionSummary},
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

    pub fn create(&self, spec: SessionSpec) -> Result<Arc<Session>> {
        if spec.name.trim().is_empty() {
            return Err(ApiError::BadRequest("name must not be empty".into()));
        }
        // Names need not be unique — duplicates are merely confusing, not invalid.
        let spawned = pty::spawn(
            &spec,
            &self.cfg.default_shell,
            &self.cfg.default_cwd,
            self.cfg.scrollback_bytes,
            self.cfg.activity_idle_after_ms,
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
