//! A Codex session's turn boundaries, read from its rollout (WI-1576).
//!
//! Codex persists an `event_msg` entry at every turn boundary: `task_started`
//! when a turn begins and `task_complete` {turn_id, last_agent_message} when
//! it ends, plus `thread_goal_updated` whenever its `/goal` changes state. The
//! economics tailer already follows the rollout, so it hands these lines here
//! instead of the engine guessing the same facts from the window title and a
//! screen render. A Codex session with no rollout keeps the title and screen
//! path.
//!
//! What follows from them:
//! - `task_complete` publishes one `ready` change per turn id (WI-1519).
//! - A final message whose last non-blank line is exactly `AUTOPILOT: DONE`
//!   takes the done path (WI-1518, source `marker`) without a render.
//! - `task_started` marks the turn running, so autopilot does not nudge it.
//! - An active goal drives its own idle turns, so autopilot leaves it alone,
//!   as it does Klaudia's goal loop.

use serde_json::Value;
use vogt_engine_contract::DoneSource;

use crate::{events::ServerEvent, pty::Session, sessions::SessionRegistry};

/// One turn boundary or goal change read from a rollout line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RolloutEvent {
    Started { turn_id: String },
    Complete { turn_id: String, done: bool },
    Goal { active: bool },
}

/// The rollout event a JSONL line records, if it records one this module
/// reads. Anything else, malformed lines included, is `None`.
pub(crate) fn parse_line(line: &str) -> Option<RolloutEvent> {
    // Cheap pre-filter: most rollout lines are responses and tool output.
    if !line.contains("\"event_msg\"") {
        return None;
    }
    let entry: Value = serde_json::from_str(line.trim()).ok()?;
    if entry.get("type").and_then(Value::as_str) != Some("event_msg") {
        return None;
    }
    let payload = entry.get("payload")?;
    let turn_id = || {
        payload
            .get("turn_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
    };
    // Codex writes the v1 names and accepts the v2 aliases; read both.
    match payload.get("type").and_then(Value::as_str)? {
        "task_started" | "turn_started" => Some(RolloutEvent::Started {
            turn_id: turn_id()?,
        }),
        "task_complete" | "turn_complete" => Some(RolloutEvent::Complete {
            turn_id: turn_id()?,
            done: payload
                .get("last_agent_message")
                .and_then(Value::as_str)
                .is_some_and(says_done),
        }),
        "thread_goal_updated" => Some(RolloutEvent::Goal {
            active: payload
                .get("goal")
                .and_then(|g| g.get("status"))
                .and_then(Value::as_str)
                == Some("active"),
        }),
        _ => None,
    }
}

/// Whether the last non-blank line of a final message is exactly the marker.
fn says_done(message: &str) -> bool {
    message
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .is_some_and(|l| l == crate::autopilot::DONE_MARKER)
}

/// What the rollout has said about one session's turns.
#[derive(Debug, Default)]
pub(crate) struct RolloutTurns {
    /// A rollout has been read for this session. While set, readiness and
    /// done come from it rather than from the screen.
    pub seen: bool,
    /// A turn has started and not yet completed.
    pub running: bool,
    /// The thread's goal is active, so Codex continues idle turns itself.
    pub goal_active: bool,
    last_ready: Option<String>,
    last_done: Option<String>,
}

/// What applying a batch of rollout events asks the caller to publish.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub ready: bool,
    pub done: bool,
}

impl RolloutTurns {
    /// Fold `events` in. `replay` is the first read of a rollout that already
    /// held turns (an engine restart, a resumed conversation): it settles the
    /// state and may announce the turn it ends on as ready, but never declares
    /// done for a turn that ended before the engine was watching.
    pub(crate) fn apply(&mut self, events: &[RolloutEvent], replay: bool) -> Outcome {
        self.seen = true;
        let mut out = Outcome::default();
        for event in events {
            match event {
                RolloutEvent::Started { .. } => {
                    self.running = true;
                    out.ready = false;
                }
                RolloutEvent::Complete { turn_id, done } => {
                    self.running = false;
                    if self.last_ready.as_deref() != Some(turn_id) {
                        self.last_ready = Some(turn_id.clone());
                        out.ready = true;
                    }
                    if *done && self.last_done.as_deref() != Some(turn_id) {
                        self.last_done = Some(turn_id.clone());
                        out.done = !replay;
                    }
                }
                RolloutEvent::Goal { active } => self.goal_active = *active,
            }
        }
        // A later turn that started in the same batch is not at rest.
        if self.running {
            out.ready = false;
        }
        out
    }
}

/// Publish what a batch of rollout events says about `session`.
pub(crate) fn apply(
    session: &Session,
    registry: &SessionRegistry,
    bus: &crate::events::EventBus,
    events: &[RolloutEvent],
    replay: bool,
) {
    let out = session.apply_rollout(events, replay);
    if out.ready {
        session.announce_rollout_ready(bus);
    }
    if out.done && session.autopilot() {
        let at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        if registry.mark_done(session.id, at.clone()) {
            bus.publish(ServerEvent::SessionDone {
                id: session.id,
                at,
                source: DoneSource::Marker,
            });
        }
        let _ = registry.set_autopilot(session.id, false);
        tracing::info!(
            session = %session.id,
            event = "autopilot.done",
            "codex rollout says no unblocked work is left; autopilot off"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(payload: Value) -> String {
        serde_json::json!({"timestamp": "t", "type": "event_msg", "payload": payload}).to_string()
    }

    #[test]
    fn reads_turn_boundaries_and_goal_changes() {
        assert_eq!(
            parse_line(&line(
                serde_json::json!({"type": "task_started", "turn_id": "t1"})
            )),
            Some(RolloutEvent::Started {
                turn_id: "t1".into()
            })
        );
        assert_eq!(
            parse_line(&line(serde_json::json!({
                "type": "task_complete", "turn_id": "t1",
                "last_agent_message": "all landed\n\nAUTOPILOT: DONE\n"
            }))),
            Some(RolloutEvent::Complete {
                turn_id: "t1".into(),
                done: true
            })
        );
        assert_eq!(
            parse_line(&line(
                serde_json::json!({"type": "thread_goal_updated", "goal": {"status": "active"}})
            )),
            Some(RolloutEvent::Goal { active: true })
        );
        assert_eq!(
            parse_line(&line(
                serde_json::json!({"type": "thread_goal_updated", "goal": {"status": "complete"}})
            )),
            Some(RolloutEvent::Goal { active: false })
        );
        assert_eq!(
            parse_line(&line(serde_json::json!({"type": "agent_message"}))),
            None
        );
        assert_eq!(parse_line("{\"type\":\"event_msg\""), None);
        assert_eq!(
            parse_line(&line(serde_json::json!({"type": "task_complete"}))),
            None
        );
    }

    #[test]
    fn done_is_the_exact_last_non_blank_line() {
        assert!(says_done("work\nAUTOPILOT: DONE"));
        assert!(says_done("AUTOPILOT: DONE  \n\n"));
        assert!(!says_done("AUTOPILOT: DONE\nthen one more thing"));
        assert!(!says_done("not AUTOPILOT: DONE"));
        assert!(!says_done(""));
    }

    #[test]
    fn one_ready_per_turn_and_done_only_when_live() {
        let started = |t: &str| RolloutEvent::Started { turn_id: t.into() };
        let complete = |t: &str, done| RolloutEvent::Complete {
            turn_id: t.into(),
            done,
        };
        let mut turns = RolloutTurns::default();
        assert_eq!(turns.apply(&[started("a")], false), Outcome::default());
        assert!(turns.running);
        assert_eq!(
            turns.apply(&[complete("a", false)], false),
            Outcome {
                ready: true,
                done: false
            }
        );
        // The same line read again (a reset tail) is not a second turn.
        assert_eq!(
            turns.apply(&[complete("a", false)], false),
            Outcome::default()
        );
        // A turn that ends and the next that starts, in one read: not at rest.
        assert_eq!(
            turns.apply(&[started("b"), complete("b", false), started("c")], false),
            Outcome::default()
        );
        assert_eq!(
            turns.apply(&[complete("c", true)], false),
            Outcome {
                ready: true,
                done: true
            }
        );
        assert_eq!(
            turns.apply(&[complete("c", true)], false),
            Outcome::default()
        );

        let mut resumed = RolloutTurns::default();
        assert_eq!(
            resumed.apply(&[started("x"), complete("x", true)], true),
            Outcome {
                ready: true,
                done: false
            }
        );
    }
}
