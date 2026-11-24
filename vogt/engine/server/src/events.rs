use std::{collections::BTreeMap, sync::Arc};

use parking_lot::Mutex;
use tokio::sync::broadcast;
pub use vogt_engine_contract::ServerEvent;

/// How many sequenced events the bus keeps for replay (WI-1246). A cursor
/// older than this falls off and the caller is told the gap, not given a
/// hole that looks like nothing happened.
const RING_CAP: usize = 4096;

/// One published event with its place in the bus's sequence. The sequence
/// lives here, not on `ServerEvent`: existing subscribers and the SSE payload
/// stay the raw event, and only a caller that asks for the sequence sees it.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Sequenced {
    pub seq: u64,
    /// When it was published (RFC 3339).
    pub at: String,
    pub event: ServerEvent,
}

struct Ring {
    /// The last sequence number handed out. `0` before anything is published.
    head: u64,
    events: std::collections::VecDeque<Sequenced>,
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<ServerEvent>,
    /// The same events, sequenced, for replay and for callers that wait on a
    /// cursor (WI-1246). Existing subscribers stay on `tx`.
    seq_tx: broadcast::Sender<Sequenced>,
    ring: Arc<Mutex<Ring>>,
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
        let (seq_tx, _seq_rx) = broadcast::channel(capacity);
        Self {
            tx,
            seq_tx,
            ring: Arc::new(Mutex::new(Ring {
                head: 0,
                events: std::collections::VecDeque::new(),
            })),
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
        let mut ring = self.ring.lock();
        ring.head += 1;
        let sequenced = Sequenced {
            seq: ring.head,
            at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string()),
            event: ev.clone(),
        };
        ring.events.push_back(sequenced.clone());
        while ring.events.len() > RING_CAP {
            ring.events.pop_front();
        }
        // Send while the ring lock is held. The broadcast channel preserves
        // send order, so two publishers that interleave after releasing the
        // lock would deliver a higher seq before a lower one and a cursor
        // would skip the late one. The raw channel has no seq to keep in
        // order, so it is sent after the lock.
        let _ = self.seq_tx.send(sequenced);
        drop(ring);
        // Ignore send errors: no subscribers is fine.
        let _ = self.tx.send(ev);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerEvent> {
        self.tx.subscribe()
    }

    /// The sequenced stream. Subscribe to this *before* reading [`since`], so
    /// an event published between the two is not lost; dedupe by `seq`.
    pub fn subscribe_seq(&self) -> broadcast::Receiver<Sequenced> {
        self.seq_tx.subscribe()
    }

    /// The last sequence number handed out, or `0` when nothing has been.
    pub fn head(&self) -> u64 {
        self.ring.lock().head
    }

    /// Events published after `seq`, oldest first; whether the cursor fell off
    /// the ring; and the head seen under the same lock.
    ///
    /// A cursor past that head is stale rather than in the future: the sequence
    /// restarts at 1 when the engine does, so an overseer still holding its old
    /// cursor would otherwise see an empty page and wait forever. It gets the
    /// whole ring, flagged truncated, the same as a cursor that aged out.
    /// Truncated is also set when the oldest retained sequence is past `seq + 1`.
    pub fn since(&self, seq: u64) -> (Vec<Sequenced>, bool, u64) {
        let ring = self.ring.lock();
        let stale = seq > ring.head;
        let truncated = stale
            || ring
                .events
                .front()
                .is_some_and(|oldest| oldest.seq > seq.saturating_add(1));
        let events = ring
            .events
            .iter()
            .filter(|ev| stale || ev.seq > seq)
            .cloned()
            .collect();
        (events, truncated, ring.head)
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

    #[test]
    fn every_event_gets_the_next_seq_and_since_replays_after_a_cursor() {
        let bus = EventBus::new(16);
        assert_eq!(bus.head(), 0);
        assert_eq!(bus.since(0).0.len(), 0);

        bus.publish(renamed(1));
        bus.publish(renamed(2));
        bus.publish(renamed(3));
        assert_eq!(bus.head(), 3);

        let (all, truncated, head) = bus.since(0);
        assert!(!truncated);
        assert_eq!(head, 3);
        assert_eq!(
            all.iter().map(|ev| ev.seq).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        let (tail, truncated, _) = bus.since(2);
        assert!(!truncated);
        assert_eq!(tail.len(), 1);
        assert!(matches!(&tail[0].event, ServerEvent::SessionRenamed { name, .. } if name == "3"));
        // The cursor is current: nothing new, and not a gap.
        assert_eq!(bus.since(3), (vec![], false, 3));
    }

    #[test]
    fn a_cursor_past_the_head_is_a_restart_not_the_future() {
        let bus = EventBus::new(16);
        bus.publish(renamed(1));
        bus.publish(renamed(2));
        // An overseer that watched the previous process up to seq 5000.
        let (replay, truncated, head) = bus.since(5000);
        assert!(truncated);
        assert_eq!(head, 2);
        assert_eq!(
            replay.iter().map(|ev| ev.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn a_cursor_older_than_the_ring_is_truncated() {
        let bus = EventBus::new(16);
        for n in 0..5 {
            bus.publish(renamed(n));
        }
        // Shrink the ring to what a long-quiet cursor would find.
        bus.ring.lock().events.pop_front();
        bus.ring.lock().events.pop_front();
        let (kept, truncated, _) = bus.since(0);
        assert!(truncated, "seq 1 and 2 are gone; the cursor must be told");
        assert_eq!(
            kept.iter().map(|ev| ev.seq).collect::<Vec<_>>(),
            vec![3, 4, 5]
        );
        // Exactly at the oldest retained sequence: the next one is still there.
        let (_, truncated, _) = bus.since(2);
        assert!(!truncated);
    }

    #[tokio::test]
    async fn the_sequenced_channel_carries_the_same_seq_as_the_ring() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe_seq();
        bus.publish(renamed(7));
        let live = rx.recv().await.unwrap();
        let (replay, _, _) = bus.since(0);
        assert_eq!(live.seq, replay[0].seq);
        assert_eq!(live.seq, 1);
    }
}
