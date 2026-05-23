use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::activity::ActivityState;

/// Server-wide events broadcast: session lifecycle + activity transitions.
/// Per-session PTY output uses its own channel; this is the lightweight
/// "what's happening overall" stream that backs the SSE /api/events endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ServerEvent {
    SessionCreated { id: Uuid, name: String },
    SessionRenamed { id: Uuid, name: String },
    SessionKilled { id: Uuid, exit_code: Option<i32> },
    Activity { id: Uuid, state: ActivityState },
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, ev: ServerEvent) {
        // Ignore send errors: no subscribers is fine.
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(256)
    }
}
