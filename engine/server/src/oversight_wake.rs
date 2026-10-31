//! Overseer wake-on-event (WI-1521, docs/design/oversight-sessions.md §4).
//!
//! An oversight session that names change kinds in `wake_on` is typed one
//! line when a session it started changes in one of those ways, so an idle
//! overseer does not sleep through its workers finishing. Changes are
//! coalesced: the first opens a window, and when the window closes the engine
//! types one line naming every worker that changed in it, e.g.
//!
//! `[vogt] 2 workers changed (done: vogt-1513, blocked: vogt-1514) — session_changes(since=41)`
//!
//! The line is typed only while the overseer rests at its prompt (idle or
//! waiting for input, not blocked), no sooner than [`MIN_GAP`] after its last
//! wake, and not while a person is typing into it (input within
//! [`TYPING_QUIET`]). A pending batch waits for those to clear. A hibernated
//! or exited overseer is never woken: its batch is dropped, and the inbox and
//! push reach it as before. `wake_on` empty (the default) types nothing.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;
use vogt_engine_contract::{ActivityState, SessionChange, SessionRole};

use crate::{
    app::AppState,
    changes::change_of,
    error::{ApiError, Result},
    pty::Session,
    sessions::SessionRegistry,
};

/// How long the first change waits for others before the line is typed.
pub const WINDOW: Duration = Duration::from_secs(60);
/// The least time between two wakes of one overseer.
pub const MIN_GAP: Duration = Duration::from_secs(60);
/// Input this recent means a person is typing; the wake waits.
pub const TYPING_QUIET: Duration = Duration::from_secs(30);
/// How often pending batches are checked.
const TICK: Duration = Duration::from_secs(5);

/// Kinds an overseer may wake on. `reported` is the `session_report` change
/// (WI-1520) and matches nothing until the engine emits it. `escalated`
/// names no change of its own: it matches any change that carries an
/// escalation (a blocked report, a permission dialog, a grant request;
/// WI-996), and the line names it `escalated`.
pub const WAKE_KINDS: &[&str] = &[
    "done",
    "reported",
    "blocked",
    "approval",
    "errored",
    "budget",
    "escalated",
];

/// `wake_on` as the engine keeps it: trimmed, deduplicated, each a known kind.
pub fn normalize_wake_on(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for kind in raw {
        let kind = kind.trim();
        if !WAKE_KINDS.contains(&kind) {
            return Err(ApiError::BadRequest(format!(
                "wake_on {kind:?} is not one of {}",
                WAKE_KINDS.join(", ")
            )));
        }
        if !out.iter().any(|k| k == kind) {
            out.push(kind.to_string());
        }
    }
    Ok(out)
}

#[derive(Debug)]
struct Batch {
    opened: Instant,
    /// The seq just before the first change, for `session_changes(since=…)`.
    since: u64,
    /// (worker label, kind) in arrival order; a worker appears once per kind.
    changes: Vec<(String, String)>,
}

/// Pending batches and last wakes, per overseer. Pure: the watcher feeds it
/// changes and asks it what is due, so the timing rules are testable.
#[derive(Debug, Default)]
pub struct Coalescer {
    pending: HashMap<Uuid, Batch>,
    last_wake: HashMap<Uuid, Instant>,
}

/// What the overseer looks like when a batch is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overseer {
    /// At its prompt, and nobody typed into it within `TYPING_QUIET`.
    Ready,
    /// Mid-turn, blocked or being typed into: try again next tick.
    Busy,
    /// Hibernated, exited or gone: drop the batch.
    Gone,
}

impl Coalescer {
    /// Record a change for `overseer`.
    pub fn add(&mut self, overseer: Uuid, worker: String, change: &SessionChange, now: Instant) {
        let batch = self.pending.entry(overseer).or_insert_with(|| Batch {
            opened: now,
            since: change.seq.saturating_sub(1),
            changes: Vec::new(),
        });
        if !batch
            .changes
            .iter()
            .any(|(w, k)| *w == worker && *k == change.kind)
        {
            batch.changes.push((worker, change.kind.clone()));
        }
    }

    /// Lines due now, each with its overseer. `state` says whether one can
    /// take a line; a busy overseer keeps its batch for the next call.
    pub fn due(
        &mut self,
        now: Instant,
        mut state: impl FnMut(Uuid) -> Overseer,
    ) -> Vec<(Uuid, String)> {
        let mut out = Vec::new();
        let ids: Vec<Uuid> = self.pending.keys().copied().collect();
        for id in ids {
            let opened = self.pending[&id].opened;
            if now.duration_since(opened) < WINDOW {
                continue;
            }
            if self
                .last_wake
                .get(&id)
                .is_some_and(|at| now.duration_since(*at) < MIN_GAP)
            {
                continue;
            }
            match state(id) {
                Overseer::Busy => continue,
                Overseer::Gone => {
                    self.pending.remove(&id);
                }
                Overseer::Ready => {
                    let batch = self.pending.remove(&id).expect("listed above");
                    self.last_wake.insert(id, now);
                    out.push((id, line(&batch)));
                }
            }
        }
        out
    }
}

/// The one line typed for a batch.
fn line(batch: &Batch) -> String {
    let mut workers: Vec<&str> = Vec::new();
    let mut kinds: Vec<(&str, Vec<&str>)> = Vec::new();
    for (worker, kind) in &batch.changes {
        if !workers.contains(&worker.as_str()) {
            workers.push(worker);
        }
        match kinds.iter_mut().find(|(k, _)| k == kind) {
            Some((_, names)) => names.push(worker),
            None => kinds.push((kind, vec![worker])),
        }
    }
    let detail = kinds
        .iter()
        .map(|(kind, names)| format!("{kind}: {}", names.join(", ")))
        .collect::<Vec<_>>()
        .join("; ");
    let count = workers.len();
    let noun = if count == 1 { "worker" } else { "workers" };
    format!(
        "[vogt] {count} {noun} changed ({detail}) — session_changes(since={})",
        batch.since
    )
}

/// The worker's name as typed into the overseer's terminal: a strict
/// `[A-Za-z0-9._-]` slug, never the raw name. The line is typed and then
/// Entered, so a name carrying CR, LF or an escape would inject input into the
/// overseer. Falls back to the id prefix when nothing safe is left.
fn label(session: &Session) -> String {
    slug(&session.name()).unwrap_or_else(|| session.id.to_string()[..8].to_string())
}

const MAX_LABEL_CHARS: usize = 48;

fn slug(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.chars() {
        let safe = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if safe {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= MAX_LABEL_CHARS {
            break;
        }
    }
    let out = out.trim_matches('-');
    (!out.is_empty()).then(|| out.to_string())
}

/// The `wake_on` kind `change` wakes on: its own kind when asked for, else
/// `escalated` when it carries an escalation and that was asked for.
fn wake_kind(wake_on: &[String], change: &SessionChange) -> Option<&'static str> {
    let asked = |kind: &str| wake_on.iter().any(|k| k == kind);
    if let Some(kind) = WAKE_KINDS.iter().find(|k| **k == change.kind) {
        if asked(kind) {
            return Some(kind);
        }
    }
    (change.escalation.is_some() && asked("escalated")).then_some("escalated")
}

/// The overseer to tell about `change`, the worker's label and the kind to
/// name, when there is one that asked for this kind.
fn route(
    registry: &SessionRegistry,
    change: &SessionChange,
) -> Option<(Uuid, String, &'static str)> {
    let worker = registry.get(change.session).ok()?;
    let overseer_id = worker.supervisor()?;
    let overseer = registry.get(overseer_id).ok()?;
    if overseer.role() != SessionRole::Oversight {
        return None;
    }
    let kind = wake_kind(&overseer.wake_on(), change)?;
    Some((overseer_id, label(&worker), kind))
}

fn overseer_state(registry: &SessionRegistry, id: Uuid, now: Instant) -> Overseer {
    let Ok(session) = registry.get(id) else {
        return Overseer::Gone;
    };
    if !session.is_alive() || session.role() != SessionRole::Oversight {
        return Overseer::Gone;
    }
    let activity = session.activity();
    if activity == ActivityState::Hibernated {
        return Overseer::Gone;
    }
    let at_prompt = matches!(
        activity,
        ActivityState::Idle | ActivityState::WaitingForInput
    );
    let typing = session
        .last_input()
        .is_some_and(|at| now.duration_since(at) < TYPING_QUIET);
    if !at_prompt || typing || session.blocked().is_some() {
        Overseer::Busy
    } else {
        Overseer::Ready
    }
}

/// Watch the bus for supervised changes and type due lines.
pub fn spawn_watcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut rx = state.bus.subscribe_seq();
        let mut coalescer = Coalescer::default();
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                got = rx.recv() => match got {
                    Ok(sequenced) => {
                        if let Some(change) = change_of(&sequenced) {
                            if let Some((overseer, worker, kind)) = route(&state.sessions, &change) {
                                // The typed line names only a WAKE_KINDS constant.
                                let change = SessionChange { kind: kind.to_string(), ..change };
                                coalescer.add(overseer, worker, &change, Instant::now());
                            }
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        state.bus.note_lag("oversight-wake", skipped);
                    }
                    Err(RecvError::Closed) => return,
                },
                _ = ticker.tick() => {
                    let now = Instant::now();
                    let due = coalescer.due(now, |id| overseer_state(&state.sessions, id, now));
                    for (id, text) in due {
                        let Ok(session) = state.sessions.get(id) else { continue };
                        match crate::autopilot::type_line(&session, &text).await {
                            Ok(()) => tracing::info!(
                                session = %id,
                                event = "oversight.wake",
                                line = %text,
                                "typed a wake line into an idle overseer"
                            ),
                            Err(e) => tracing::warn!(
                                session = %id,
                                error = %e,
                                "could not type a wake line into an overseer"
                            ),
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(seq: u64, kind: &str) -> SessionChange {
        SessionChange {
            seq,
            at: String::new(),
            session: Uuid::nil(),
            kind: kind.to_string(),
            summary: kind.to_string(),
            escalation: None,
        }
    }

    #[test]
    fn a_label_is_a_strict_slug_of_the_name() {
        assert_eq!(slug("vogt-1513").as_deref(), Some("vogt-1513"));
        assert_eq!(
            slug("fix the_bug v2.1").as_deref(),
            Some("fix-the_bug-v2.1")
        );
        assert_eq!(slug("x\r/approve all\n").as_deref(), Some("x-approve-all"));
        assert_eq!(slug("\u{1b}[2J\u{9b}").as_deref(), Some("2J"));
        assert_eq!(slug("\r\n\u{1b}"), None);
        assert_eq!(
            slug(&"a".repeat(500)).map(|s| s.len()),
            Some(MAX_LABEL_CHARS)
        );
    }

    #[test]
    fn a_wake_line_from_a_hostile_name_types_only_safe_characters() {
        let overseer = Uuid::new_v4();
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        let hostile = slug("x\r\u{1b}[31mrm -rf /\n").unwrap();
        c.add(overseer, hostile, &change(42, "done"), t0);
        let due = c.due(t0 + WINDOW, |_| Overseer::Ready);
        let (_, text) = &due[0];
        assert!(
            !text.chars().any(char::is_control),
            "control character in {text:?}"
        );
        assert!(text.contains("done: x-31mrm--rf"), "{text}");
    }

    #[test]
    fn two_workers_in_one_window_are_one_line_naming_both() {
        let overseer = Uuid::new_v4();
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.add(overseer, "vogt-1513".into(), &change(42, "done"), t0);
        c.add(
            overseer,
            "vogt-1514".into(),
            &change(43, "blocked"),
            t0 + Duration::from_secs(10),
        );
        // A duplicate of the same worker and kind is not repeated.
        c.add(
            overseer,
            "vogt-1513".into(),
            &change(44, "done"),
            t0 + Duration::from_secs(20),
        );
        assert!(c
            .due(t0 + Duration::from_secs(59), |_| Overseer::Ready)
            .is_empty());
        let due = c.due(t0 + WINDOW, |_| Overseer::Ready);
        assert_eq!(
            due,
            vec![(
                overseer,
                "[vogt] 2 workers changed (done: vogt-1513; blocked: vogt-1514) — session_changes(since=41)"
                    .to_string()
            )]
        );
        // Nothing left: exactly one line.
        assert!(c.due(t0 + WINDOW * 3, |_| Overseer::Ready).is_empty());
    }

    #[test]
    fn a_busy_overseer_keeps_its_batch_and_a_gone_one_drops_it() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.add(a, "w1".into(), &change(5, "done"), t0);
        c.add(b, "w2".into(), &change(6, "errored"), t0);
        let due = c.due(t0 + WINDOW, |id| {
            if id == a {
                Overseer::Busy
            } else {
                Overseer::Gone
            }
        });
        assert!(due.is_empty());
        let due = c.due(t0 + WINDOW + TICK, |_| Overseer::Ready);
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].0, a);
        assert!(due[0].1.starts_with("[vogt] 1 worker changed (done: w1)"));
    }

    #[test]
    fn at_most_one_wake_per_gap() {
        let a = Uuid::new_v4();
        let t0 = Instant::now();
        let mut c = Coalescer::default();
        c.add(a, "w1".into(), &change(1, "done"), t0);
        assert_eq!(c.due(t0 + WINDOW, |_| Overseer::Ready).len(), 1);
        // A change right after the wake opens a batch, but its window closing
        // earlier than MIN_GAP after the wake must still wait for the gap.
        let t1 = t0 + WINDOW + Duration::from_secs(1);
        c.add(a, "w2".into(), &change(2, "done"), t1);
        assert!(c
            .due(t1 + WINDOW - Duration::from_secs(2), |_| Overseer::Ready)
            .is_empty());
        assert_eq!(c.due(t1 + WINDOW, |_| Overseer::Ready).len(), 1);
    }

    #[test]
    fn escalated_wakes_on_any_change_carrying_an_escalation() {
        let escalated = vec!["escalated".to_string()];
        let mut blocked = change(3, "blocked");
        assert_eq!(wake_kind(&escalated, &blocked), None);
        blocked.escalation = Some(vogt_engine_contract::ChangeEscalation {
            needs: Default::default(),
            boundary: Default::default(),
            ask: "merge".into(),
            key: "k".into(),
            resolve: vec![],
        });
        assert_eq!(wake_kind(&escalated, &blocked), Some("escalated"));
        // Its own kind wins when that was asked for too.
        let both = vec!["blocked".to_string(), "escalated".to_string()];
        assert_eq!(wake_kind(&both, &blocked), Some("blocked"));
        assert_eq!(wake_kind(&escalated, &change(4, "done")), None);
        let mut c = Coalescer::default();
        let t0 = Instant::now();
        let esc = SessionChange {
            kind: "escalated".into(),
            ..blocked
        };
        c.add(Uuid::nil(), "vogt-1".into(), &esc, t0);
        let due = c.due(t0 + WINDOW, |_| Overseer::Ready);
        assert!(due[0].1.contains("(escalated: vogt-1)"), "{}", due[0].1);
    }

    #[test]
    fn wake_on_accepts_known_kinds_once_and_refuses_others() {
        assert_eq!(normalize_wake_on(&[]).unwrap(), Vec::<String>::new());
        assert_eq!(
            normalize_wake_on(&[" done".into(), "done".into(), "blocked".into()]).unwrap(),
            vec!["done".to_string(), "blocked".to_string()]
        );
        assert!(normalize_wake_on(&["ready".into()]).is_err());
    }
}
