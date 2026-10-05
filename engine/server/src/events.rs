use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::broadcast;
pub use vogt_engine_contract::ServerEvent;

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
    /// Events each named subscriber lost by falling behind, since start
    /// (WI-920). A subscriber that lags misses events but must keep
    /// receiving — this is how an operator sees that it happened.
    lags: Arc<Mutex<BTreeMap<&'static str, LagCount>>>,
}

/// How often, and how much, one subscriber fell behind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct LagCount {
    pub episodes: u64,
    pub events_skipped: u64,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity);
        Self {
            tx,
            lags: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Record that `subscriber` fell `skipped` events behind. Logged, and
    /// counted for `/api/status`.
    pub fn note_lag(&self, subscriber: &'static str, skipped: u64) {
        tracing::warn!(
            subscriber,
            skipped,
            "event subscriber lagged; it missed events and carries on"
        );
        let mut lags = self.lags.lock();
        let count = lags.entry(subscriber).or_default();
        count.episodes += 1;
        count.events_skipped += skipped;
    }

    /// The next event for a long-lived subscriber: a lag is recorded under
    /// `subscriber` and skipped, never the end of the subscription. `None`
    /// only when the bus itself is gone.
    pub async fn recv_or_skip(
        &self,
        rx: &mut broadcast::Receiver<ServerEvent>,
        subscriber: &'static str,
    ) -> Option<ServerEvent> {
        loop {
            match rx.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    self.note_lag(subscriber, skipped);
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Every subscriber that has lagged, by name.
    pub fn lags(&self) -> BTreeMap<&'static str, LagCount> {
        self.lags.lock().clone()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn renamed(n: u32) -> ServerEvent {
        ServerEvent::SessionRenamed {
            id: uuid::Uuid::nil(),
            name: n.to_string(),
        }
    }

    #[tokio::test]
    async fn a_lagging_subscriber_is_told_counted_and_keeps_receiving() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        for n in 0..5 {
            bus.publish(renamed(n));
        }
        // Three were overwritten before it read: it gets the newest it can,
        // and the lag is on record.
        let next = bus.recv_or_skip(&mut rx, "test").await.unwrap();
        assert!(matches!(next, ServerEvent::SessionRenamed { ref name, .. } if name == "3"));
        assert_eq!(
            bus.lags()["test"],
            LagCount {
                episodes: 1,
                events_skipped: 3
            }
        );
        // And it is still subscribed afterwards.
        bus.recv_or_skip(&mut rx, "test").await.unwrap();
        bus.publish(renamed(9));
        let later = bus.recv_or_skip(&mut rx, "test").await.unwrap();
        assert!(matches!(later, ServerEvent::SessionRenamed { ref name, .. } if name == "9"));
    }
}
