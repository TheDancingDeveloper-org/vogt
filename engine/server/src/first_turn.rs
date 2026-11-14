//! A session given a brief must start a turn (WI-4068).
//!
//! A pod Codex session used to open at its welcome screen with the brief
//! unread: the pod rebuilt the launch line itself and never gave the CLI the
//! brief pointer, so with autopilot off nothing ever typed into it. With
//! autopilot on the first turn started only at the 60-second nudge, which is
//! why every pod session took 65–79 seconds to produce agent output. The
//! launch now carries the pointer (see `command_line` in `vogt-session-agent`),
//! exactly as a host launch and Claude Code already did.
//!
//! Carrying the pointer is not the same as it being submitted. A TUI can sit at
//! its composer with the text queued — the batch-122 paste-burst stall was that,
//! and any future submit path can regress the same way. The engine therefore
//! watches the first turn instead of assuming it:
//!
//! 1. **Readiness**: wait for the agent to come to rest at a prompt (the screen
//!    `is_ready` check), never a fixed sleep.
//! 2. **Confirmation**: once at rest, look for a turn actually running for
//!    [`Policy::turn_within`]. If none, press Enter once — a queued composer
//!    submits, an empty one ignores it — and look again.
//! 3. **Surface a failure**: a session that still has not started a turn is
//!    reported blocked ("the first prompt never started a turn") so it shows
//!    in the session list as needing a person instead of idling silently.
//!
//! A briefed agent given its pointer at launch often starts working without
//! ever resting at the composer first. Its first turn can then finish inside
//! the readiness window, leaving only an idle composer behind — indistinguishable
//! from a queued prompt unless the turn start is remembered. Readiness therefore
//! reports a turn already started (Codex's `working` title, or the turn start
//! the activity watcher records) and the watch returns without pressing Enter
//! or reporting blocked.
//!
//! It gives up quietly when the session exits, is blocked on a person, shows a
//! dialog, or never settles; the watchdog never answers a dialog.

use std::{sync::Arc, time::Duration};

use vogt_engine_contract::{ActivityState, BlockedReport};

use crate::{
    events::EventBus,
    pty::Session,
    screen::{self, AgentTitle},
};

/// How long the watch spends on each step. A module value so tests can make it
/// instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// How long the TUI gets to draw its composer and come to rest.
    pub ready_within: Duration,
    /// How long a submitted prompt gets to show a running turn, before the
    /// Enter retry and again after it.
    pub turn_within: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            // A pod TUI is usable within a few seconds; MCP servers may still
            // be starting behind it but do not hold the composer closed.
            ready_within: Duration::from_secs(30),
            // Codex sets its `working` title on submit, before the model
            // answers, so a started turn is visible within this window.
            turn_within: Duration::from_secs(15),
        }
    }
}

/// How often the watch looks at the session. Small enough to see a turn start,
/// large enough not to spin a runtime worker.
const POLL: Duration = Duration::from_millis(100);

/// Agents the watchdog confirms. Codex can leave the brief queued in the
/// composer, and it sets a title this engine reads, so `codex: working` is
/// proof that a turn began even if the TUI never rested at its composer first.
/// Claude Code announces no turn-state title, so a Claude turn that runs
/// without a quiet gap and ends inside the readiness window leaves no evidence
/// at all, and a retry there would type a second prompt into a session that
/// already took its first (WI-4186 review). `opencode` takes no first prompt,
/// so a retry there would be guesswork.
pub const FIRST_TURN_AGENTS: &[&str] = &["codex"];

/// Whether the engine starts the watch for a session it just spawned: only a
/// fresh one (a wake resumes a conversation that already took its brief),
/// given a brief, running an agent in [`FIRST_TURN_AGENTS`].
pub fn watches(fresh: bool, briefed: bool, agent: Option<&str>) -> bool {
    fresh && briefed && agent.is_some_and(|agent| FIRST_TURN_AGENTS.contains(&agent))
}

/// Watch a freshly spawned session's first turn. Spawn this once, at start,
/// for a fresh agent session that was given a brief.
pub fn watch(bus: EventBus, session: Arc<Session>, policy: Policy) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        match wait_until_ready(&session, policy.ready_within).await {
            Readiness::Stop => return,
            Readiness::TurnStarted => {
                // The brief was taken at launch. There is no queued prompt to
                // confirm, and pressing Enter could interrupt the turn.
                tracing::info!(
                    session = %session.id,
                    event = "first_turn.started",
                    first_turn_ms = started.elapsed().as_millis() as u64,
                    retried = false,
                    "the session's first turn started before it rested at a composer"
                );
                return;
            }
            Readiness::Ready => {}
        }
        for attempt in 0..2 {
            if wait_for_turn(&session, policy.turn_within).await {
                tracing::info!(
                    session = %session.id,
                    event = "first_turn.started",
                    first_turn_ms = started.elapsed().as_millis() as u64,
                    retried = attempt > 0,
                    "the session's first turn started"
                );
                return;
            }
            if attempt > 0 {
                break;
            }
            // Only into a TUI that is at its prompt: an Enter at a dialog
            // answers it, and a person's dialog is not the engine's to answer.
            if !at_composer(&session) {
                return;
            }
            if resubmit(&session).await.is_err() {
                return;
            }
        }
        if !session.is_alive() {
            return;
        }
        let report = BlockedReport {
            reason: "the first prompt never started a turn: the session is at its \
                     composer and no turn is running, so this session needs a person"
                .to_string(),
            items: vec![
                "read the session's screen and press Enter or send the prompt again".to_string(),
                "if the composer is empty, the agent never received its brief; stop and restart it"
                    .to_string(),
            ],
            since: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
        };
        if let Err(e) = session.set_blocked(Some(report), &bus) {
            tracing::warn!(session = %session.id, error = %e, "could not report a first turn that never started");
        } else {
            tracing::warn!(
                session = %session.id,
                event = "first_turn.failed",
                "the session's first turn never started; reported blocked"
            );
        }
    })
}

/// Press Enter once: a queued composer submits, an empty one ignores it.
async fn resubmit(session: &Arc<Session>) -> std::io::Result<()> {
    session.write_input(b"\r")?;
    tracing::info!(
        session = %session.id,
        event = "first_turn.retry",
        "the brief was queued but no turn started; pressed Enter once"
    );
    Ok(())
}

/// What a fresh session was doing when the watch last looked at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readiness {
    /// At rest at its composer: a brief may be queued there and needs confirming.
    Ready,
    /// A turn is running, or already ran. The prompt was taken, so there is
    /// nothing to press and nothing to report.
    TurnStarted,
    /// Exited, asked for a person, or never settled: nothing to confirm.
    Stop,
}

/// Wait for the agent's first rest at a prompt. A turn already under way —
/// running now, or started and finished between two polls — is reported as
/// [`Readiness::TurnStarted`] rather than mistaken for a prompt still queued at
/// the composer.
async fn wait_until_ready(session: &Arc<Session>, within: Duration) -> Readiness {
    let baseline = session.turn_started_at();
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if !session.is_alive() || session.blocked().is_some() {
            return Readiness::Stop;
        }
        let (rendered, _) = session.render_screen(0);
        // Codex's own word outranks the activity poll: it sets `working` on
        // submit, before the model answers, and it forces the activity to
        // running even while the TUI has gone quiet.
        if matches!(
            screen::agent_title(rendered.title.as_deref()),
            Some(AgentTitle::Working)
        ) {
            return Readiness::TurnStarted;
        }
        // A turn that began and ended between two polls leaves the stamp
        // advanced (see `begins_turn`) even though the activity is back at rest.
        if turn_started_since(session, baseline) {
            return Readiness::TurnStarted;
        }
        match session.activity() {
            ActivityState::AwaitingApproval => return Readiness::Stop,
            ActivityState::Idle | ActivityState::WaitingForInput
                if is_at_composer(session, &rendered) =>
            {
                return Readiness::Ready
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Readiness::Stop;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Whether a turn began after the watch started. A fresh session carries its
/// spawn time in `turn_started_at`, so only a later stamp can be a real turn.
///
/// Nor is every later stamp one (WI-4186). A session starts `running`; one that
/// stays silent for the quiet window goes `idle`, and its first output then
/// moves it back to `running`, which stamps a "turn start". A pod session is
/// exactly that: nothing reaches its terminal while the pod is scheduled and
/// the entrypoint boots, so the CLI's first draw is stamped as a turn. Taking
/// that for a started turn ended the watch at once, so a pod Codex whose launch
/// prompt did not start a turn was never given its Enter, never reported
/// blocked, and with autopilot off sat at its composer for good (with autopilot
/// on, the 60-second nudge started it, which is the 65–79 s first turn every
/// pod session showed). A turn starts from rest, and rest needs a quiet window
/// after some output, so a stamp counts only when it falls at least the quiet
/// window after the session's first output.
fn turn_started_since(session: &Arc<Session>, baseline: Option<time::OffsetDateTime>) -> bool {
    let Some(stamp) = session.turn_started_at() else {
        return false;
    };
    if baseline.is_some_and(|before| stamp <= before) {
        return false;
    }
    session
        .first_output_at()
        .is_some_and(|first| stamp >= first + session.idle_after())
}

/// Whether a turn starts within `within`. A running turn means the prompt was
/// taken; a dialog, a blocked report or an exit ends the watch.
async fn wait_for_turn(session: &Arc<Session>, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if !session.is_alive() || session.blocked().is_some() {
            return false;
        }
        match session.activity() {
            ActivityState::Running => return true,
            ActivityState::AwaitingApproval => return false,
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Whether the agent is at rest with a composer a driver can type into, the
/// same test `wait` uses for `ready`.
fn at_composer(session: &Arc<Session>) -> bool {
    let (rendered, _) = session.render_screen(0);
    is_at_composer(session, &rendered)
}

/// [`at_composer`] against a screen the caller already rendered.
fn is_at_composer(session: &Arc<Session>, rendered: &screen::Rendered) -> bool {
    screen::is_ready(
        session.activity(),
        session.is_alive(),
        &rendered.lines,
        session.agent().as_deref(),
        rendered.title.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::watches;

    /// WI-4186 review: the gate `sessions.rs` applies. Codex only, fresh and
    /// briefed only; Claude Code (with or without autopilot, host or pod) is
    /// never watched, so nothing is ever typed into it.
    #[test]
    fn only_a_fresh_briefed_codex_is_watched() {
        assert!(watches(true, true, Some("codex")));
        assert!(!watches(true, true, Some("claude")));
        assert!(!watches(true, true, Some("opencode")));
        assert!(!watches(true, true, None));
        assert!(
            !watches(false, true, Some("codex")),
            "a wake already took its brief"
        );
        assert!(
            !watches(true, false, Some("codex")),
            "no brief, no first prompt"
        );
    }
}
