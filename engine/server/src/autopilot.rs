//! Keeping an autopilot session working through its backlog (WI-949).
//!
//! `session.start` with `autopilot` tells the agent, in its brief, to carry
//! straight on to the next item instead of ending its turn. Claude Code
//! mostly does; opencode ends a *run* at every natural stop of the model, and
//! any agent can stop to announce what it will do next. Nothing then
//! re-prompts it, the session sits at its prompt, and — before this — the idle
//! policy hibernated it in exactly that pause.
//!
//! So the engine re-drives it. Every tick it looks at each live session on
//! autopilot, and one that is at its prompt (`ready`), has been quiet for
//! `nudge_after`, and is not blocked on a person is told to carry on — one
//! line of text, then Enter. It stops for good, and turns the session's
//! autopilot off, when:
//!
//! - the agent says there is nothing left: a line reading exactly
//!   `AUTOPILOT: DONE` near the bottom of its screen (the brief asks for it);
//! - the deployment's cap on nudges is reached, so a confused agent that
//!   answers every nudge with "nothing to do" in other words cannot be
//!   driven forever.
//!
//! What is never nudged: a session blocked on a person (it said so; a person
//! acts next), one at a permission dialog or mid-turn, a plain shell, and
//! Klaudia, which runs its own goal loop (`/goal run`) — two drivers of one
//! loop would interleave prompts into its turns (WI-950).
//!
//! A nudge is input, so it resets the session's quiet clock: the next one
//! cannot come sooner than `nudge_after` after the agent last stopped.

use std::{sync::Arc, time::Duration};

use vogt_engine_contract::ActivityState;

use crate::{app::AppState, pty::Session, screen, sessions::SessionRegistry};

/// The deployment's autopilot settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// How long an autopilot session must sit at its prompt before it is
    /// told to carry on (`ENGINE_AUTOPILOT_NUDGE_AFTER`).
    pub nudge_after: Duration,
    /// The most nudges one session gets; 0 never nudges, leaving autopilot as
    /// the idle-policy exemption alone (`ENGINE_AUTOPILOT_MAX_NUDGES`).
    pub max_nudges: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            nudge_after: Duration::from_secs(60),
            max_nudges: 100,
        }
    }
}

/// The line an agent prints when no unblocked work is left.
pub const DONE_MARKER: &str = "AUTOPILOT: DONE";

/// What the engine types. It names the marker without spelling it, so the
/// echo of a nudge on the screen can never read as the agent being done.
pub const NUDGE: &str = "Autopilot: carry on with the next unblocked item in scope. \
If you are blocked on a person, report it with session_report_blocked and stop. \
If no unblocked work is left, say so and end your reply with the autopilot done \
line your brief describes.";

/// Agents the engine knows how to re-drive at their prompt.
const REDRIVEN: &[&str] = &["claude", "codex", "opencode"];

/// Whether the visible screen ends with the agent saying it is done: a line,
/// among the last ten that are not blank, that reads exactly the marker once
/// any leading decoration (a bullet, a box border, indentation) is dropped.
pub fn says_done(lines: &[String]) -> bool {
    lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(10)
        .any(|l| l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()) == DONE_MARKER)
}

/// The agent CLI a live session runs, from its conversation or its command.
fn agent_of(session: &Session) -> Option<String> {
    if let Some(conversation) = session.conversation() {
        return Some(conversation.agent);
    }
    let command = session.summary().command?;
    let argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    crate::agent_cli::agent_name(&argv)
}

/// What one tick decides for one session.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Leave,
    Nudge,
    Done,
    Capped,
}

/// The decision, pure over what was read.
pub fn decide(
    policy: &Policy,
    activity: ActivityState,
    blocked: bool,
    quiet_for: Duration,
    ready: bool,
    done: bool,
    nudges: u32,
) -> Step {
    if done {
        return Step::Done;
    }
    if blocked
        || !matches!(
            activity,
            ActivityState::Idle | ActivityState::WaitingForInput
        )
    {
        return Step::Leave;
    }
    if !ready || quiet_for < policy.nudge_after {
        return Step::Leave;
    }
    if nudges >= policy.max_nudges {
        return Step::Capped;
    }
    Step::Nudge
}

/// Look at every autopilot session once.
pub async fn run_once(registry: &SessionRegistry, policy: &Policy) {
    for session in registry.live_sessions() {
        if !session.autopilot() || !session.is_alive() {
            continue;
        }
        let Some(agent) = agent_of(&session) else {
            continue;
        };
        if !REDRIVEN.contains(&agent.as_str()) {
            continue;
        }
        // Cheap checks first: a render is a replay of up to 1 MiB.
        let activity = session.activity();
        let blocked = session.blocked().is_some();
        let quiet_for = session.quiet_for();
        if blocked
            || !matches!(
                activity,
                ActivityState::Idle | ActivityState::WaitingForInput
            )
            || quiet_for < policy.nudge_after
        {
            continue;
        }
        let Ok(view) = screen::session_screen(Arc::clone(&session), 0).await else {
            continue;
        };
        let step = decide(
            policy,
            view.activity,
            blocked,
            quiet_for,
            view.ready,
            says_done(&view.lines),
            session.autopilot_nudges(),
        );
        match step {
            Step::Leave => {}
            Step::Done => {
                let _ = registry.set_autopilot(session.id, false);
                tracing::info!(
                    session = %session.id,
                    nudges = session.autopilot_nudges(),
                    event = "autopilot.done",
                    "autopilot session says no unblocked work is left; autopilot off"
                );
            }
            Step::Capped => {
                let _ = registry.set_autopilot(session.id, false);
                tracing::warn!(
                    session = %session.id,
                    nudges = session.autopilot_nudges(),
                    event = "autopilot.capped",
                    "autopilot nudge cap reached; autopilot off"
                );
            }
            Step::Nudge => {
                if let Err(e) = nudge(&session).await {
                    tracing::warn!(session = %session.id, error = %e, "could not nudge an autopilot session");
                    continue;
                }
                let count = session.count_autopilot_nudge();
                tracing::info!(
                    session = %session.id,
                    agent = %agent,
                    nudges = count,
                    event = "autopilot.nudge",
                    "told an autopilot session to carry on"
                );
            }
        }
    }
}

/// Type the nudge, then Enter on its own: a TUI that reads a burst ending in
/// a carriage return as a paste would keep the newline as text.
async fn nudge(session: &Session) -> std::io::Result<()> {
    session.write_input(NUDGE.as_bytes())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    session.write_input(b"\r")
}

/// Run the re-driver, unless the deployment turned nudging off.
pub fn spawn_watcher(state: Arc<AppState>) {
    let policy = state.config.autopilot.clone();
    if policy.max_nudges == 0 {
        return;
    }
    let tick = (policy.nudge_after / 4).clamp(Duration::from_millis(250), Duration::from_secs(15));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            run_once(&state.sessions, &policy).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &[&str]) -> Vec<String> {
        text.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn done_is_the_marker_on_a_line_of_its_own_and_never_the_nudge() {
        assert!(says_done(&lines(&[
            "⏺ Backlog empty.",
            "  AUTOPILOT: DONE",
            "",
            "│ > "
        ])));
        assert!(says_done(&lines(&[
            "     AUTOPILOT: DONE",
            "  ┃",
            "  ╹▀▀▀"
        ])));
        assert!(!says_done(&lines(&["AUTOPILOT: DONE soon, maybe"])));
        assert!(!says_done(&lines(&["say AUTOPILOT: DONE when finished"])));
        // The echo of a nudge, however it wraps, does not spell the marker.
        assert!(!NUDGE.contains(DONE_MARKER));
        for width in [20, 40, 60, 80] {
            let wrapped: Vec<String> = NUDGE
                .as_bytes()
                .chunks(width)
                .map(|c| String::from_utf8_lossy(c).into_owned())
                .collect();
            assert!(!says_done(&wrapped), "wrapped at {width}");
        }
    }

    #[test]
    fn a_ready_quiet_unblocked_session_is_nudged_until_done_or_capped() {
        let policy = Policy {
            nudge_after: Duration::from_secs(60),
            max_nudges: 3,
        };
        let quiet = Duration::from_secs(61);
        let go = |activity, blocked, quiet_for, ready, done, nudges| {
            decide(&policy, activity, blocked, quiet_for, ready, done, nudges)
        };
        assert_eq!(
            go(ActivityState::Idle, false, quiet, true, false, 0),
            Step::Nudge
        );
        assert_eq!(
            go(ActivityState::WaitingForInput, false, quiet, true, false, 2),
            Step::Nudge
        );
        assert_eq!(
            go(ActivityState::Idle, false, quiet, true, false, 3),
            Step::Capped
        );
        assert_eq!(
            go(ActivityState::Idle, false, quiet, true, true, 0),
            Step::Done
        );
        // Each of these leaves it alone.
        assert_eq!(
            go(ActivityState::Idle, true, quiet, true, false, 0),
            Step::Leave
        );
        assert_eq!(
            go(ActivityState::Running, false, quiet, true, false, 0),
            Step::Leave
        );
        assert_eq!(
            go(
                ActivityState::AwaitingApproval,
                false,
                quiet,
                true,
                false,
                0
            ),
            Step::Leave
        );
        assert_eq!(
            go(ActivityState::Idle, false, quiet, false, false, 0),
            Step::Leave
        );
        assert_eq!(
            go(
                ActivityState::Idle,
                false,
                Duration::from_secs(5),
                true,
                false,
                0
            ),
            Step::Leave
        );
    }
}
