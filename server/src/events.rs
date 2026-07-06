pub use mydevenv2_contract::ServerEvent;
use tokio::sync::broadcast;

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
