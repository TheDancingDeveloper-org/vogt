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
//! It gives up quietly when the session exits, is blocked on a person, shows a
//! dialog, or is already running a turn; the watchdog never answers a dialog.

use std::{sync::Arc, time::Duration};

use vogt_engine_contract::{ActivityState, BlockedReport};

use crate::{events::EventBus, pty::Session, screen};

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

/// Agents whose launch line submits the brief itself. `opencode` takes no
/// first prompt, so a retry there would be guesswork.
pub const FIRST_TURN_AGENTS: &[&str] = &["claude", "codex"];

/// Watch a freshly spawned session's first turn. Spawn this once, at start,
/// for a fresh agent session that was given a brief.
pub fn watch(bus: EventBus, session: Arc<Session>, policy: Policy) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let started = std::time::Instant::now();
        if !wait_until_ready(&session, policy.ready_within).await {
            return;
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
            if session.write_input(b"\r").is_err() {
                return;
            }
            tracing::info!(
                session = %session.id,
                event = "first_turn.retry",
                "the brief was queued but no turn started; pressed Enter once"
            );
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

/// Wait for the agent's first rest at a prompt. `false` means the session
/// exited, asked for a person, or never became ready within the window — in
/// every case there is nothing to confirm.
async fn wait_until_ready(session: &Arc<Session>, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if !session.is_alive() || session.blocked().is_some() {
            return false;
        }
        match session.activity() {
            ActivityState::AwaitingApproval => return false,
            ActivityState::Idle | ActivityState::WaitingForInput if at_composer(session) => {
                return true
            }
            _ => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
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
    screen::is_ready(
        session.activity(),
        session.is_alive(),
        &rendered.lines,
        session.agent().as_deref(),
        rendered.title.as_deref(),
    )
}
