//! Session changes for an overseer (WI-1246).
//!
//! Every bus event has a monotonic sequence and the bus keeps a bounded ring
//! of them. `GET /api/sessions/changes` reads that ring after a cursor, and
//! `GET /api/sessions/wait-any` blocks until the next matching one, so an
//! overseer waits for a delta instead of sweeping on a timer.
//!
//! Not every bus event is a change. Activity states an overseer acts on
//! (`waiting-for-input`, `awaiting-approval`, `exited`, `stopped`, `errored`)
//! become `ready`, `approval`, `exited` and `errored`; `running`, `idle` and
//! `hibernated` do not, because hibernation has its own events and a session
//! that went quiet is not necessarily at its prompt. A session that did come
//! to rest at a prompt publishes `SessionReady` (WI-1519), which is the other
//! way a `ready` change arrives. `budget` is emitted from the budget watcher
//! (WI-1245 / C7). `reply` and `commit` are reserved kinds and are not emitted
//! here.

use std::{collections::HashSet, time::Duration};

use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;
use vogt_engine_contract::{ActivityState, SessionChange, SessionChanges, SessionWaitAny};

use crate::{
    error::{ApiError, Result},
    events::{EventBus, Sequenced, ServerEvent},
};

/// The longest a single wait-any may block, the same ceiling as `/wait`.
pub const MAX_WAIT: Duration = Duration::from_secs(600);

/// How many changes one read returns at most.
const MAX_LIMIT: usize = 500;

/// Kinds a caller may name in `until`.
const KINDS: &[&str] = &[
    "ready",
    "approval",
    "exited",
    "errored",
    "blocked",
    "unblocked",
    "hibernated",
    "woken",
    "budget",
];

/// What an overseer waits for when it does not say.
const DEFAULT_UNTIL: &[&str] = &["ready", "blocked", "approval", "exited", "errored"];

fn clip(text: &str) -> String {
    let mut out: String = text.chars().take(120).collect();
    if text.chars().count() > 120 {
        out.pop();
        out.push('…');
    }
    out
}

/// The change one bus event is, when it is one an overseer reads.
pub fn change_of(sequenced: &Sequenced) -> Option<SessionChange> {
    let (session, kind, summary) = match &sequenced.event {
        ServerEvent::Activity { id, state, .. } => {
            let kind = match state {
                ActivityState::WaitingForInput => "ready",
                ActivityState::AwaitingApproval => "approval",
                ActivityState::Exited | ActivityState::Stopped => "exited",
                ActivityState::Errored => "errored",
                ActivityState::Running | ActivityState::Idle | ActivityState::Hibernated => {
                    return None
                }
            };
            (*id, kind, format!("activity {kind}"))
        }
        // An agent that finished its turn and rests at its prompt: `idle` is
        // not itself a change (a quiet mid-turn is not ready), so readiness is
        // its own event (WI-1519).
        ServerEvent::SessionReady { id } => (*id, "ready", "ready".to_string()),
        ServerEvent::SessionBlocked { id, blocked } => match blocked {
            Some(report) => (*id, "blocked", format!("blocked: {}", report.reason)),
            None => (*id, "unblocked", "unblocked".to_string()),
        },
        ServerEvent::SessionHibernated { id, trigger } => {
            let why = match trigger {
                vogt_engine_contract::HibernateTrigger::Manual => "manual",
                vogt_engine_contract::HibernateTrigger::Idle => "idle",
                vogt_engine_contract::HibernateTrigger::Memory => "memory",
                vogt_engine_contract::HibernateTrigger::Shutdown => "shutdown",
                vogt_engine_contract::HibernateTrigger::Recovered => "recovered",
                vogt_engine_contract::HibernateTrigger::Budget => "budget",
            };
            (*id, "hibernated", format!("hibernated ({why})"))
        }
        ServerEvent::SessionWoken { id } => (*id, "woken", "woken".to_string()),
        ServerEvent::SessionBudget { id, level, summary } => {
            (*id, "budget", format!("{level}: {summary}"))
        }
        ServerEvent::SessionKilled { id, exit_code } => (
            *id,
            "exited",
            match exit_code {
                Some(code) => format!("killed (exit {code})"),
                None => "killed".to_string(),
            },
        ),
        _ => return None,
    };
    Some(SessionChange {
        seq: sequenced.seq,
        at: sequenced.at.clone(),
        session,
        kind: kind.to_string(),
        summary: clip(&summary),
    })
}

/// `until` as a set, or the default overseer set when the caller named none.
pub fn parse_until(raw: Option<&str>) -> Result<HashSet<String>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(DEFAULT_UNTIL.iter().map(|k| (*k).to_string()).collect());
    };
    let mut out = HashSet::new();
    for part in raw.split(',') {
        let kind = part.trim();
        if !KINDS.contains(&kind) {
            return Err(ApiError::BadRequest(format!(
                "until {kind:?} is not one of {}",
                KINDS.join(", ")
            )));
        }
        out.insert(kind.to_string());
    }
    Ok(out)
}

/// Session ids to keep, or every session when the caller named none.
fn parse_ids(raw: Option<&str>) -> Result<Option<HashSet<Uuid>>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let mut out = HashSet::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        out.insert(Uuid::parse_str(part).map_err(|_| {
            ApiError::BadRequest(format!("ids contains {part:?}, which is not a session id"))
        })?);
    }
    Ok((!out.is_empty()).then_some(out))
}

fn matches(change: &SessionChange, ids: Option<&HashSet<Uuid>>, until: &HashSet<String>) -> bool {
    ids.is_none_or(|ids| ids.contains(&change.session)) && until.contains(&change.kind)
}

/// Changes published after `since`, filtered, oldest first.
pub fn changes(
    bus: &EventBus,
    since: u64,
    ids: Option<&str>,
    limit: Option<usize>,
) -> Result<SessionChanges> {
    let ids = parse_ids(ids)?;
    let limit = limit.unwrap_or(100).clamp(1, MAX_LIMIT);
    let (replay, truncated, head) = bus.since(since);
    let mut out = Vec::new();
    for sequenced in &replay {
        if out.len() == limit {
            break;
        }
        if let Some(change) =
            change_of(sequenced).filter(|c| matches(c, ids.as_ref(), &all_kinds()))
        {
            out.push(change);
        }
    }
    // A page the limit cut short resumes at its last row. Otherwise the cursor
    // is the head `since` saw, not one read afterwards: an event published
    // between the two would be skipped by the next read forever.
    let cut = replay.len() > out.len();
    let next_cursor = if cut {
        out.last().map(|c| c.seq).unwrap_or(head)
    } else {
        head
    };
    Ok(SessionChanges {
        changes: out,
        next_cursor,
        truncated,
    })
}

fn all_kinds() -> HashSet<String> {
    KINDS.iter().map(|k| (*k).to_string()).collect()
}

/// Block until a matching change lands after `cursor`, or the timeout passes.
///
/// Subscribes before reading the ring, the same order as `/wait`, so a change
/// published between the two is caught on the live stream and deduped by seq.
pub async fn wait_any(
    bus: &EventBus,
    ids: Option<&str>,
    until: &HashSet<String>,
    cursor: u64,
    timeout: Duration,
) -> Result<SessionWaitAny> {
    if timeout > MAX_WAIT {
        return Err(ApiError::BadRequest(format!(
            "timeout_s is at most {}",
            MAX_WAIT.as_secs()
        )));
    }
    let ids = parse_ids(ids)?;
    let mut rx = bus.subscribe_seq();
    // A cursor past the head belongs to a process that has since restarted
    // (the sequence begins again at 1). Treating it as current would filter
    // every new event out; treat it as the start instead.
    let cursor = if cursor > bus.head() { 0 } else { cursor };
    let (replay, _, _) = bus.since(cursor);
    if let Some(change) = replay
        .iter()
        .filter_map(change_of)
        .find(|c| matches(c, ids.as_ref(), until))
    {
        return Ok(SessionWaitAny {
            outcome: "changed".to_string(),
            next_cursor: change.seq,
            change: Some(change),
        });
    }
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Err(_) => {
                return Ok(SessionWaitAny {
                    outcome: "timeout".to_string(),
                    change: None,
                    next_cursor: bus.head(),
                });
            }
            Ok(Err(RecvError::Closed)) => {
                return Err(ApiError::Internal("the event bus closed".into()));
            }
            // Missed events: the ring still has them, unless the cursor fell
            // off it, which the next read reports as truncated.
            Ok(Err(RecvError::Lagged(skipped))) => {
                bus.note_lag("session-wait-any", skipped);
                let (replay, _, _) = bus.since(cursor);
                if let Some(change) = replay
                    .iter()
                    .filter_map(change_of)
                    .find(|c| matches(c, ids.as_ref(), until))
                {
                    return Ok(SessionWaitAny {
                        outcome: "changed".to_string(),
                        next_cursor: change.seq,
                        change: Some(change),
                    });
                }
            }
            Ok(Ok(sequenced)) => {
                if sequenced.seq <= cursor {
                    continue;
                }
                if let Some(change) =
                    change_of(&sequenced).filter(|c| matches(c, ids.as_ref(), until))
                {
                    return Ok(SessionWaitAny {
                        outcome: "changed".to_string(),
                        next_cursor: change.seq,
                        change: Some(change),
                    });
                }
            }
        }
    }
}

/// Replay events after a `Last-Event-ID`, then the live sequenced stream,
/// deduped by seq. Used by the server-wide SSE stream: the payload stays the
/// raw event, and the sequence rides on the SSE `id`.
pub fn replay_then_live(
    bus: &EventBus,
    last_event_id: Option<u64>,
) -> (
    Vec<Sequenced>,
    bool,
    tokio::sync::broadcast::Receiver<Sequenced>,
) {
    let rx = bus.subscribe_seq();
    let (replay, truncated, _) =
        last_event_id
            .map(|seq| bus.since(seq))
            .unwrap_or((vec![], false, 0));
    (replay, truncated, rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vogt_engine_contract::BlockedReport;

    fn activity(state: ActivityState) -> ServerEvent {
        ServerEvent::Activity {
            id: Uuid::nil(),
            state,
            activity_changed_at: String::new(),
            approval: None,
            turn_started_at: None,
        }
    }

    #[test]
    fn activity_states_map_to_overseer_kinds_and_the_rest_are_dropped() {
        let bus = EventBus::new(16);
        for state in [
            ActivityState::WaitingForInput,
            ActivityState::AwaitingApproval,
            ActivityState::Exited,
            ActivityState::Stopped,
            ActivityState::Errored,
            ActivityState::Running,
            ActivityState::Idle,
            ActivityState::Hibernated,
        ] {
            bus.publish(activity(state));
        }
        let got = changes(&bus, 0, None, None).unwrap();
        assert_eq!(
            got.changes
                .iter()
                .map(|c| c.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["ready", "approval", "exited", "exited", "errored"]
        );
        assert!(!got.truncated);
        assert_eq!(got.next_cursor, 5);
    }

    #[test]
    fn blocked_hibernate_and_kill_map_and_a_long_reason_is_clipped() {
        let bus = EventBus::new(16);
        let id = Uuid::from_u128(7);
        bus.publish(ServerEvent::SessionBlocked {
            id,
            blocked: Some(BlockedReport {
                reason: "x".repeat(200),
                items: vec![],
                since: String::new(),
            }),
        });
        bus.publish(ServerEvent::SessionBlocked { id, blocked: None });
        bus.publish(ServerEvent::SessionWoken { id });
        bus.publish(ServerEvent::SessionKilled {
            id,
            exit_code: Some(9),
        });
        let got = changes(&bus, 0, None, None).unwrap();
        assert_eq!(
            got.changes
                .iter()
                .map(|c| c.kind.as_str())
                .collect::<Vec<_>>(),
            vec!["blocked", "unblocked", "woken", "exited"]
        );
        assert_eq!(got.changes[0].summary.chars().count(), 120);
        assert!(got.changes[0].summary.ends_with('…'));
        assert_eq!(got.changes[3].summary, "killed (exit 9)");
    }

    #[test]
    fn ids_filter_and_limit_and_an_empty_read_advances_to_the_head() {
        let bus = EventBus::new(16);
        let mine = Uuid::from_u128(1);
        let other = Uuid::from_u128(2);
        bus.publish(ServerEvent::SessionWoken { id: other });
        bus.publish(ServerEvent::SessionWoken { id: mine });
        bus.publish(ServerEvent::SessionWoken { id: mine });
        let got = changes(&bus, 0, Some(&mine.to_string()), Some(1)).unwrap();
        assert_eq!(got.changes.len(), 1);
        assert_eq!(got.changes[0].seq, 2);
        assert_eq!(got.next_cursor, 2);
        let none = changes(&bus, 3, None, None).unwrap();
        assert!(none.changes.is_empty());
        assert_eq!(none.next_cursor, 3);
    }

    #[test]
    fn until_rejects_an_unknown_kind_and_ids_reject_a_bad_uuid() {
        assert!(parse_until(Some("ready,nope")).is_err());
        assert!(parse_until(None).unwrap().contains("ready"));
        let bus = EventBus::new(8);
        assert!(changes(&bus, 0, Some("not-a-uuid"), None).is_err());
    }

    #[tokio::test]
    async fn wait_any_returns_a_buffered_change_then_blocks_then_times_out() {
        let bus = EventBus::new(16);
        bus.publish(activity(ActivityState::WaitingForInput));
        let until = parse_until(Some("ready")).unwrap();
        let hit = wait_any(&bus, None, &until, 0, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(hit.outcome, "changed");
        assert_eq!(hit.change.unwrap().seq, 1);

        let bus2 = bus.clone();
        let pending = tokio::spawn(async move {
            let until = parse_until(Some("exited")).unwrap();
            wait_any(&bus2, None, &until, 1, Duration::from_secs(5)).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        bus.publish(activity(ActivityState::Exited));
        let hit = pending.await.unwrap().unwrap();
        assert_eq!(hit.outcome, "changed");
        assert_eq!(hit.change.unwrap().kind, "exited");

        let timed = wait_any(&bus, None, &until, bus.head(), Duration::from_millis(20))
            .await
            .unwrap();
        assert_eq!(timed.outcome, "timeout");
        assert!(timed.change.is_none());

        let too_long = wait_any(&bus, None, &until, 0, Duration::from_secs(601)).await;
        assert!(too_long.is_err());
    }
}
