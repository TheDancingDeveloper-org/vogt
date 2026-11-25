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
//! - An active goal drives its own idle turns, so autopilot leaves it alone.
//! - A turn that gives up (`task_complete` with an `error`, such as a 429 the
//!   client stopped retrying) or is aborted (`turn_aborted`) ends the turn
//!   like any other, but is never done, and its reason is logged (WI-4506).
//!   Retryable errors (`error`, `stream_error`) are not written to the
//!   rollout, so they never end a turn.

use serde_json::Value;
use vogt_engine_contract::DoneSource;

use crate::{events::ServerEvent, pty::Session, sessions::SessionRegistry};

/// One turn boundary or goal change read from a rollout line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RolloutEvent {
    Started {
        turn_id: String,
    },
    Complete {
        turn_id: String,
        done: bool,
        /// Unix seconds the turn ended, when Codex recorded it.
        completed_at: Option<i64>,
    },
    Goal {
        active: bool,
    },
    /// The turn ended without finishing: it failed or was aborted (WI-4506).
    Failed {
        turn_id: Option<String>,
        reason: String,
    },
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
        "task_complete" | "turn_complete" if payload.get("error").is_some_and(|e| !e.is_null()) => {
            Some(RolloutEvent::Failed {
                turn_id: turn_id(),
                reason: payload
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("turn failed")
                    .to_string(),
            })
        }
        "task_complete" | "turn_complete" => Some(RolloutEvent::Complete {
            turn_id: turn_id()?,
            done: payload
                .get("last_agent_message")
                .and_then(Value::as_str)
                .is_some_and(says_done),
            completed_at: payload.get("completed_at").and_then(Value::as_i64),
        }),
        "thread_goal_updated" => Some(RolloutEvent::Goal {
            active: payload
                .get("goal")
                .and_then(|g| g.get("status"))
                .and_then(Value::as_str)
                == Some("active"),
        }),
        "turn_aborted" => Some(RolloutEvent::Failed {
            turn_id: turn_id(),
            reason: format!(
                "aborted: {}",
                payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            ),
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
    /// When the rollout last showed the session at rest: a turn ended and no
    /// other started. A `working` title or report older than this is stale
    /// (WI-4506).
    pub rested_at: Option<time::OffsetDateTime>,
    /// The screen path announced a `ready` before any rollout was read; the
    /// first turn the rollout completes is the one it stood for.
    screen_ready: bool,
    last_ready: Option<String>,
    last_done: Option<String>,
    /// A turn ended while a goal was active, so its `ready` was held back; it
    /// is announced if the goal ends with the session still at rest.
    held_ready: bool,
    /// The turn id of a held turn whose final message said done; announced
    /// with the held `ready` when the goal ends with the session at rest.
    held_done: Option<String>,
}

/// What applying a batch of rollout events asks the caller to publish.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub ready: bool,
    pub done: bool,
    /// Why the last turn in the batch failed or was aborted, if it did.
    pub failed: Option<String>,
}

impl RolloutTurns {
    /// Record a screen-path `ready` for a turn whose id is not yet known, unless
    /// a rollout is already read (it announces instead). True when the caller
    /// should publish. One lock covers the check and the record, so a rollout
    /// read cannot slip between them.
    pub(crate) fn try_screen_ready(&mut self) -> bool {
        if self.seen {
            return false;
        }
        self.screen_ready = true;
        true
    }

    /// Fold `events` in. `replay` is the first read of a rollout that already
    /// held turns (an engine restart, a resumed conversation): it settles the
    /// state and may announce the turn it ends on as ready, but never declares
    /// done for a turn that ended before the engine was watching.
    pub(crate) fn apply(&mut self, events: &[RolloutEvent], replay: bool, since: i64) -> Outcome {
        self.seen = true;
        let mut out = Outcome::default();
        // On the first read, every turn already complete is at or before the
        // screen's last announcement, so none of them is announced again.
        let swallow = replay && self.screen_ready;
        let mut swallowed = false;
        for event in events {
            match event {
                RolloutEvent::Started { .. } => {
                    self.running = true;
                    out.ready = false;
                }
                RolloutEvent::Complete {
                    turn_id,
                    done,
                    completed_at,
                } => {
                    self.running = false;
                    out.failed = None;
                    self.end_turn(Some(turn_id), swallow, &mut swallowed, &mut out);
                    // A short turn can end before the first poll reads the
                    // rollout; that is still this session's turn, not history,
                    // so only turns that ended before it began are held back.
                    let live = !replay || completed_at.is_some_and(|t| t >= since);
                    if self.goal_active {
                        // An active goal continues itself: this turn ending is
                        // not the session coming to rest, so neither ready nor
                        // done now. Both wait for the goal to end.
                        self.held_ready |= std::mem::take(&mut out.ready);
                        if *done && live && self.last_done.as_deref() != Some(turn_id) {
                            self.held_done = Some(turn_id.clone());
                        }
                    } else {
                        // A turn ending with no goal supersedes anything held
                        // for an earlier goal (it ended while a turn ran).
                        self.held_ready = false;
                        self.held_done = None;
                        if *done && self.last_done.as_deref() != Some(turn_id) {
                            self.last_done = Some(turn_id.clone());
                            out.done = live;
                        }
                    }
                }
                RolloutEvent::Goal { active } => {
                    self.goal_active = *active;
                    if !*active && !self.running {
                        if std::mem::take(&mut self.held_ready) {
                            out.ready = true;
                        }
                        if let Some(id) = self.held_done.take() {
                            self.last_done = Some(id);
                            out.done = true;
                        }
                    }
                }
                RolloutEvent::Failed { turn_id, reason } => {
                    self.running = false;
                    out.failed = Some(reason.clone());
                    self.end_turn(turn_id.as_ref(), swallow, &mut swallowed, &mut out);
                    // Like a completed turn: under an active goal the session
                    // is not at rest, so the ready waits for the goal to end;
                    // otherwise it supersedes anything held.
                    if self.goal_active {
                        self.held_ready |= std::mem::take(&mut out.ready);
                    } else {
                        self.held_ready = false;
                        self.held_done = None;
                    }
                }
            }
        }
        if swallowed {
            self.screen_ready = false;
        }
        // A later turn that started in the same batch is not at rest.
        if self.running {
            out.ready = false;
            out.failed = None;
            self.rested_at = None;
        } else if events.iter().any(|e| {
            matches!(
                e,
                RolloutEvent::Complete { .. } | RolloutEvent::Failed { .. }
            )
        }) {
            self.rested_at = Some(time::OffsetDateTime::now_utc());
        }
        out
    }

    /// Publish one `ready` per ended turn. The screen may already have
    /// announced it: then remember its id and publish nothing. A turn with no
    /// id is announced unless the screen stood for it.
    fn end_turn(
        &mut self,
        turn_id: Option<&String>,
        swallow: bool,
        swallowed: &mut bool,
        out: &mut Outcome,
    ) {
        if turn_id.is_some() && self.last_ready.as_ref() == turn_id {
            return;
        }
        if let Some(id) = turn_id {
            self.last_ready = Some(id.clone());
        }
        if swallow {
            *swallowed = true;
        } else {
            out.ready = !std::mem::take(&mut self.screen_ready);
        }
    }

    /// The rollout shows the session at rest, since when: no turn under way
    /// and no active goal driving more.
    pub(crate) fn at_rest_since(&self) -> Option<time::OffsetDateTime> {
        (self.seen && !self.running && !self.goal_active)
            .then_some(self.rested_at)
            .flatten()
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
    let since = session.created_at.unix_timestamp() - 1;
    let out = session.apply_rollout(events, replay, since);
    if let Some(reason) = &out.failed {
        tracing::warn!(
            session = %session.id,
            event = "codex.turn_failed",
            reason = %reason,
            "codex turn ended without finishing; session at rest"
        );
    }
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
                done: true,
                completed_at: None
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
            completed_at: None,
        };
        let mut turns = RolloutTurns::default();
        assert_eq!(turns.apply(&[started("a")], false, 0), Outcome::default());
        assert!(turns.running);
        assert_eq!(
            turns.apply(&[complete("a", false)], false, 0),
            Outcome {
                ready: true,
                done: false,
                failed: None
            }
        );
        // The same line read again (a reset tail) is not a second turn.
        assert_eq!(
            turns.apply(&[complete("a", false)], false, 0),
            Outcome::default()
        );
        // A turn that ends and the next that starts, in one read: not at rest.
        assert_eq!(
            turns.apply(
                &[started("b"), complete("b", false), started("c")],
                false,
                0
            ),
            Outcome::default()
        );
        assert_eq!(
            turns.apply(&[complete("c", true)], false, 0),
            Outcome {
                ready: true,
                done: true,
                failed: None
            }
        );
        assert_eq!(
            turns.apply(&[complete("c", true)], false, 0),
            Outcome::default()
        );

        let mut resumed = RolloutTurns::default();
        assert_eq!(
            resumed.apply(&[started("x"), complete("x", true)], true, 0),
            Outcome {
                ready: true,
                done: false,
                failed: None
            }
        );
    }

    #[test]
    fn an_active_goal_holds_ready_and_done_until_it_ends() {
        let goal = |active| RolloutEvent::Goal { active };
        let complete = |t: &str, done| RolloutEvent::Complete {
            turn_id: t.into(),
            done,
            completed_at: None,
        };
        let mut turns = RolloutTurns::default();
        assert_eq!(
            turns.apply(&[goal(true), complete("a", true)], false, 0),
            Outcome::default()
        );
        assert_eq!(
            turns.apply(&[goal(false)], false, 0),
            Outcome {
                ready: true,
                done: true,
                failed: None
            }
        );
        // Nothing further is held, and the marker fires once.
        assert_eq!(turns.apply(&[goal(false)], false, 0), Outcome::default());
        assert_eq!(
            turns.apply(&[complete("a", true), goal(true), goal(false)], false, 0),
            Outcome::default()
        );
    }

    #[test]
    fn a_goal_that_ends_mid_turn_leaves_nothing_held_for_the_next_goal() {
        let goal = |active| RolloutEvent::Goal { active };
        let started = |t: &str| RolloutEvent::Started { turn_id: t.into() };
        let complete = |t: &str, done| RolloutEvent::Complete {
            turn_id: t.into(),
            done,
            completed_at: None,
        };
        let mut turns = RolloutTurns::default();
        // Goal on, turn A completes (held), the next turn runs, the goal ends
        // while it runs, and B completes normally: one ready, and B's done.
        assert_eq!(
            turns.apply(&[goal(true), complete("a", true), started("b")], false, 0),
            Outcome::default()
        );
        assert_eq!(turns.apply(&[goal(false)], false, 0), Outcome::default());
        assert_eq!(
            turns.apply(&[complete("b", false)], false, 0),
            Outcome {
                ready: true,
                done: false,
                failed: None
            }
        );
        // A later goal that starts and ends at rest announces nothing.
        assert_eq!(
            turns.apply(&[goal(true), goal(false)], false, 0),
            Outcome::default()
        );
    }

    #[test]
    fn a_failed_turn_under_an_active_goal_is_held_like_a_completed_one() {
        let goal = |active| RolloutEvent::Goal { active };
        let mut turns = RolloutTurns::default();
        let out = turns.apply(
            &[
                goal(true),
                RolloutEvent::Failed {
                    turn_id: Some("a".into()),
                    reason: "429".into(),
                },
            ],
            false,
            0,
        );
        assert!(!out.ready && !out.done);
        let out = turns.apply(&[goal(false)], false, 0);
        assert!(out.ready && !out.done);
    }

    #[test]
    fn a_turn_finished_since_the_session_began_is_done_even_on_the_first_read() {
        let complete = |at| RolloutEvent::Complete {
            turn_id: "t".into(),
            done: true,
            completed_at: Some(at),
        };
        let mut live = RolloutTurns::default();
        assert!(live.apply(&[complete(150)], true, 100).done);
        let mut history = RolloutTurns::default();
        assert!(!history.apply(&[complete(50)], true, 100).done);
    }

    #[test]
    fn a_screen_announced_turn_is_not_announced_again_by_the_rollout() {
        let mut turns = RolloutTurns::default();
        assert!(turns.try_screen_ready());
        let first = [
            RolloutEvent::Started {
                turn_id: "1".into(),
            },
            RolloutEvent::Complete {
                turn_id: "1".into(),
                done: false,
                completed_at: None,
            },
        ];
        assert_eq!(turns.apply(&first, true, 0), Outcome::default());
        // The same history read again is still nothing new.
        assert_eq!(turns.apply(&first[1..], false, 0), Outcome::default());
    }

    #[test]
    fn a_new_turn_in_the_first_read_after_a_screen_ready_is_announced() {
        let mut turns = RolloutTurns::default();
        assert!(turns.try_screen_ready());
        let history = [
            RolloutEvent::Started {
                turn_id: "1".into(),
            },
            RolloutEvent::Complete {
                turn_id: "1".into(),
                done: false,
                completed_at: None,
            },
            RolloutEvent::Started {
                turn_id: "2".into(),
            },
        ];
        assert_eq!(turns.apply(&history, true, 0), Outcome::default());
        let end = [RolloutEvent::Complete {
            turn_id: "2".into(),
            done: false,
            completed_at: None,
        }];
        assert_eq!(
            turns.apply(&end, false, 0),
            Outcome {
                ready: true,
                done: false,
                failed: None
            }
        );
    }

    #[test]
    fn two_screen_announced_turns_complete_in_the_first_read_are_two_readies_total() {
        let mut turns = RolloutTurns::default();
        // The screen announced turn 1, then turn 2: two readies published.
        assert!(turns.try_screen_ready());
        assert!(turns.try_screen_ready());
        let history = [
            RolloutEvent::Started {
                turn_id: "1".into(),
            },
            RolloutEvent::Complete {
                turn_id: "1".into(),
                done: false,
                completed_at: None,
            },
            RolloutEvent::Started {
                turn_id: "2".into(),
            },
            RolloutEvent::Complete {
                turn_id: "2".into(),
                done: false,
                completed_at: None,
            },
        ];
        assert_eq!(turns.apply(&history, true, 0), Outcome::default());
        let next = [
            RolloutEvent::Started {
                turn_id: "3".into(),
            },
            RolloutEvent::Complete {
                turn_id: "3".into(),
                done: false,
                completed_at: None,
            },
        ];
        assert!(turns.apply(&next, false, 0).ready);
    }

    #[test]
    fn a_screen_ready_is_not_recorded_once_a_rollout_is_read() {
        let mut turns = RolloutTurns::default();
        turns.apply(&[], true, 0);
        assert!(!turns.try_screen_ready());
        let c = [RolloutEvent::Complete {
            turn_id: "1".into(),
            done: false,
            completed_at: None,
        }];
        assert!(turns.apply(&c, false, 0).ready);
    }

    #[test]
    fn a_turn_that_gives_up_or_is_aborted_ends_the_turn() {
        // The fork's shapes: a 429 it stopped retrying is `task_complete`
        // with an `error`; an interrupt is `turn_aborted` with a `reason`.
        let failed = parse_line(&line(serde_json::json!({
            "type": "task_complete", "turn_id": "t1",
            "last_agent_message": "AUTOPILOT: DONE",
            "error": {"message": "429 Too Many Requests", "codex_error_info": null}
        })));
        assert_eq!(
            failed,
            Some(RolloutEvent::Failed {
                turn_id: Some("t1".into()),
                reason: "429 Too Many Requests".into()
            })
        );
        let aborted = parse_line(&line(serde_json::json!({
            "type": "turn_aborted", "turn_id": "t2", "reason": "interrupted"
        })));
        assert_eq!(
            aborted,
            Some(RolloutEvent::Failed {
                turn_id: Some("t2".into()),
                reason: "aborted: interrupted".into()
            })
        );
        let started = |t: &str| RolloutEvent::Started { turn_id: t.into() };

        let mut turns = RolloutTurns::default();
        turns.apply(&[started("t1")], false, 0);
        assert!(turns.at_rest_since().is_none());
        let out = turns.apply(&[failed.unwrap()], false, 0);
        assert!(!turns.running);
        assert!(turns.at_rest_since().is_some());
        // At rest and announced once, but a failed turn is never done.
        assert_eq!(
            out,
            Outcome {
                ready: true,
                done: false,
                failed: Some("429 Too Many Requests".into())
            }
        );

        turns.apply(&[started("t2")], false, 0);
        assert!(turns.at_rest_since().is_none());
        assert!(turns.apply(&[aborted.unwrap()], false, 0).ready);
        assert!(!turns.running);
        assert!(turns.at_rest_since().is_some());
    }
}
