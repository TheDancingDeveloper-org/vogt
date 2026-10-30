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
//!   driven forever. Hitting the cap also records a blocked report, so the
//!   session surfaces as needing a person instead of sitting quiet with
//!   autopilot simply switched off (WI-1313).
//!
//! What is never nudged: a session blocked on a person (it said so; a person
//! acts next), one at a permission dialog or mid-turn, a plain shell, and a
//! Klaudia session whose own goal loop (`/goal run`) is driving it — two
//! drivers of one loop would interleave prompts into its turns (WI-950). A
//! Klaudia session working an ordinary task is an interactive turn like any
//! other, and is nudged like one (WI-1090): it says it is in its goal loop in
//! its title (`klaudia: goal-loop`) or, on older builds, in its status bar.
//!
//! Nor is a session whose model API refuses every turn (WI-1852): an agent
//! whose provider answers a client error — `400 Invalid model name`, a
//! rejected key — sits at its prompt exactly like one that finished a turn,
//! and nudging it only repeats the refusal. When a nudged session's screen
//! still ends on a 4xx error, it is reported blocked and its autopilot
//! switched off, so it surfaces as failed rather than idle. A 408 or 429 is
//! transient and is nudged as usual.
//!
//! Nor is a session whose budget watcher holds it: a warning or a hard stop is
//! due and waits for the prompt, and a nudge would start the next turn first.
//!
//! A nudge is input, so it resets the session's quiet clock: the next one
//! cannot come sooner than `nudge_after` after the agent last stopped.

use std::{sync::Arc, time::Duration};

use vogt_engine_contract::{ActivityState, DoneSource};

use crate::{app::AppState, events::ServerEvent, pty::Session, screen, sessions::SessionRegistry};

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
pub const REDRIVEN: &[&str] = &["claude", "codex", "opencode", "klaudia"];

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

/// A model API client error near the bottom of the screen: a line, among
/// the last ten that are not blank, naming an error with a 4xx status, or
/// litellm's own name for one. 408 (timeout) and 429 (rate limit) pass by
/// and are not counted: waiting fixes them, a person does not need to.
pub fn api_client_error(lines: &[String]) -> Option<String> {
    static STATUS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?:api\s*error|error|status(?:\s*code)?|http)\b\D{0,24}\b(4\d\d)\b",
        )
        .expect("static regex")
    });
    static NAMED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b(?:BadRequestError|AuthenticationError|PermissionDeniedError|NotFoundError|Invalid model name)\b",
        )
        .expect("static regex")
    });
    lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .take(10)
        .find(|l| {
            let status = STATUS
                .captures(l)
                .and_then(|c| c.get(1))
                .is_some_and(|m| !matches!(m.as_str(), "408" | "429"));
            status || NAMED.is_match(l)
        })
        .map(|l| l.chars().take(200).collect())
}

/// What one tick decides for one session.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Leave,
    Nudge,
    Done,
    Capped,
    /// Nudged, and its model API still refused the turn (WI-1852).
    Refused,
}

/// What one session showed, as [`decide`] weighs it.
#[derive(Debug, Clone, Copy)]
pub struct Seen {
    pub activity: ActivityState,
    pub blocked: bool,
    pub quiet_for: Duration,
    pub ready: bool,
    pub done: bool,
    /// The agent's own loop is driving it (Klaudia's goal loop).
    pub own_loop: bool,
    /// The screen ends on a model API client error ([`api_client_error`]).
    pub api_error: bool,
    pub nudges: u32,
}

/// The decision, pure over what was read.
pub fn decide(policy: &Policy, seen: Seen) -> Step {
    let Seen {
        activity,
        blocked,
        quiet_for,
        ready,
        done,
        own_loop,
        api_error,
        nudges,
    } = seen;
    if done {
        return Step::Done;
    }
    if blocked
        || own_loop
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
    // The first error may be the brief's own turn going wrong once; after a
    // nudge it is the same refusal again, and another nudge would be a third.
    if api_error && nudges >= 1 {
        return Step::Refused;
    }
    if nudges >= policy.max_nudges {
        return Step::Capped;
    }
    Step::Nudge
}

/// Why a session whose model API keeps refusing needs a person.
fn refused_report(line: &str) -> vogt_engine_contract::BlockedReport {
    vogt_engine_contract::BlockedReport {
        reason: format!(
            "the agent's model API refused the turn again after an autopilot nudge: {line}"
        ),
        items: vec![
            "check the session's template and model (a Claude alias on a non-Claude \
             template is refused), then restart it or stop it"
                .to_string(),
        ],
        since: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    }
}

/// Why a session that has used its nudges needs a person.
fn cap_report(nudges: u32) -> vogt_engine_contract::BlockedReport {
    vogt_engine_contract::BlockedReport {
        reason: format!(
            "autopilot nudged this session {nudges} times and it never said it \
             was done or that it was blocked, so it needs a person"
        ),
        items: vec![
            "read the session and either give it the next instruction or stop it".to_string(),
        ],
        since: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
    }
}

/// Look at every autopilot session once.
pub async fn run_once(registry: &SessionRegistry, bus: &crate::events::EventBus, policy: &Policy) {
    for session in registry.live_sessions() {
        // A budget action is waiting for the prompt (WI-1281): a nudge now
        // would start another turn and push the action past it again.
        if !session.autopilot() || !session.is_alive() || session.budget_hold() {
            continue;
        }
        let Some(agent) = session.agent() else {
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
        let own_loop =
            agent == "klaudia" && screen::klaudia_goal_loop(view.title.as_deref(), &view.lines);
        let api_error = api_client_error(&view.lines);
        let step = decide(
            policy,
            Seen {
                activity: view.activity,
                blocked,
                quiet_for,
                ready: view.ready,
                done: says_done(&view.lines),
                own_loop,
                api_error: api_error.is_some(),
                nudges: session.autopilot_nudges(),
            },
        );
        match step {
            Step::Leave => {}
            Step::Done => {
                // Published once per turn: a screen that still shows the marker
                // on the next tick finds the declaration already there (WI-1518).
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
                    nudges = session.autopilot_nudges(),
                    event = "autopilot.done",
                    "autopilot session says no unblocked work is left; autopilot off"
                );
            }
            Step::Capped => {
                let nudges = session.autopilot_nudges();
                let _ = registry.set_autopilot(session.id, false);
                // A blocked report is what the inbox reads as needing a person,
                // so the cap escalates instead of only dropping a log line.
                if let Err(e) = session.set_blocked(Some(cap_report(nudges)), bus) {
                    tracing::warn!(
                        session = %session.id,
                        error = %e,
                        "could not report an autopilot session blocked at its nudge cap"
                    );
                }
                tracing::warn!(
                    session = %session.id,
                    nudges,
                    event = "autopilot.capped",
                    "autopilot nudge cap reached; autopilot off, needs a person"
                );
            }
            Step::Refused => {
                let line = api_error.unwrap_or_default();
                let _ = registry.set_autopilot(session.id, false);
                if let Err(e) = session.set_blocked(Some(refused_report(&line)), bus) {
                    tracing::warn!(
                        session = %session.id,
                        error = %e,
                        "could not report an autopilot session blocked on a refused model call"
                    );
                }
                tracing::warn!(
                    session = %session.id,
                    agent = %agent,
                    event = "autopilot.refused",
                    "model API refused the turn again after a nudge; autopilot off, reported blocked"
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
            run_once(&state.sessions, &state.bus, &policy).await;
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
            decide(
                &policy,
                Seen {
                    activity,
                    blocked,
                    quiet_for,
                    ready,
                    done,
                    own_loop: false,
                    api_error: false,
                    nudges,
                },
            )
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

    /// WI-1090: Klaudia's screens, from readiness to the decision. An idle
    /// Klaudia is nudged, by its title or (older builds) by its box; one at
    /// work, at a permission ask or in its own goal loop is left alone.
    #[test]
    fn klaudia_is_nudged_at_its_prompt_and_never_in_its_goal_loop() {
        use crate::screen::{is_ready, klaudia_frames as frames, klaudia_goal_loop};
        let policy = Policy::default();
        let quiet = policy.nudge_after + Duration::from_secs(1);
        let step = |activity, lines: &[String], title: Option<&str>| {
            decide(
                &policy,
                Seen {
                    activity,
                    blocked: false,
                    quiet_for: quiet,
                    ready: is_ready(activity, true, lines, Some("klaudia"), title),
                    done: says_done(lines),
                    own_loop: klaudia_goal_loop(title, lines),
                    api_error: false,
                    nudges: 0,
                },
            )
        };
        // Idle: an old build by its box, a new one by its title.
        assert_eq!(
            step(ActivityState::Idle, &frames::idle(), None),
            Step::Nudge
        );
        assert_eq!(
            step(ActivityState::Idle, &frames::idle(), Some("klaudia: ready")),
            Step::Nudge
        );
        // Mid-turn: running, and its box does not make it ready even if the
        // activity were to read idle.
        assert_eq!(
            step(ActivityState::Running, &frames::mid_turn(), None),
            Step::Leave
        );
        assert_eq!(
            step(ActivityState::Idle, &frames::mid_turn(), None),
            Step::Leave
        );
        assert_eq!(
            step(
                ActivityState::Idle,
                &frames::idle(),
                Some("klaudia: working")
            ),
            Step::Leave
        );
        // A permission ask, by its caption or its title.
        assert_eq!(
            step(ActivityState::Idle, &frames::approval(), None),
            Step::Leave
        );
        assert_eq!(
            step(
                ActivityState::Idle,
                &frames::idle(),
                Some("klaudia: awaiting approval")
            ),
            Step::Leave
        );
        // Its own goal loop, by its status bar or its title, even at rest.
        assert_eq!(
            step(ActivityState::Idle, &frames::goal_loop(), None),
            Step::Leave
        );
        assert_eq!(
            step(
                ActivityState::Idle,
                &frames::idle(),
                Some("klaudia: goal-loop")
            ),
            Step::Leave
        );
        // And it is done when it says so, as any agent is.
        let mut done = frames::idle();
        done.insert(3, "AUTOPILOT: DONE".to_string());
        assert_eq!(step(ActivityState::Idle, &done, None), Step::Done);
    }

    /// WI-1852: a provider refusing every turn is reported, not nudged forever.
    #[test]
    fn a_repeated_model_api_client_error_is_refused_not_nudged() {
        let klaudia = lines(&[
            "▣ Build · opus",
            "litellm.BadRequestError: Invalid model name passed in model=opus",
            "",
            "┃ > ",
        ]);
        assert!(api_client_error(&klaudia).is_some());
        let claude = lines(&["⏺ API Error: 400 {\"type\":\"error\"}", "│ > "]);
        assert!(api_client_error(&claude).unwrap().contains("400"));
        assert!(api_client_error(&lines(&["API Error: 429 rate limited"])).is_none());
        assert!(api_client_error(&lines(&["Error: 408 request timeout"])).is_none());
        assert!(api_client_error(&lines(&["Fixed the 404 page in 3 files"])).is_none());
        assert!(api_client_error(&lines(&["Wrote 412 lines", "│ > "])).is_none());

        let policy = Policy::default();
        let seen = |api_error, nudges| Seen {
            activity: ActivityState::Idle,
            blocked: false,
            quiet_for: policy.nudge_after + Duration::from_secs(1),
            ready: true,
            done: false,
            own_loop: false,
            api_error,
            nudges,
        };
        // The first refusal earns one nudge; the second is reported.
        assert_eq!(decide(&policy, seen(true, 0)), Step::Nudge);
        assert_eq!(decide(&policy, seen(true, 1)), Step::Refused);
        assert_eq!(decide(&policy, seen(false, 1)), Step::Nudge);
    }
}
