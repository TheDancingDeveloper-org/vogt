//! Engine-enforced session budgets (WI-1245 / C7).
//!
//! A session started with a [`SessionBudget`] is watched against the economics
//! the tailer last published on its summary. Two levels:
//!
//! - **soft**, once, at `soft_pct` of a turn or context limit: a `budget`
//!   change, and — only while the session is at its prompt — one typed line
//!   telling the agent to commit, write a hand-off, and stop. Mid-turn, the
//!   line waits for the next tick that finds it ready.
//! - **hard**, once: the same change, then the session is hibernated with
//!   trigger `budget`. A turn, context, or wall-clock limit hibernates at once,
//!   without waiting for the prompt: an autopilot worker is re-prompted the
//!   moment it reaches one, so a limit that waited for idle landed late, and a
//!   turn that never paused never tripped it at all (WI-1281). The wall clock is
//!   the session's own, not one turn's: it accumulates every stretch the session
//!   is seen running, and a pause (idle, the prompt, a permission dialog, blocked
//!   on a person) stops it without clearing what already elapsed. A session the
//!   engine cannot hibernate (no conversation to resume) is not marked done: the
//!   overseer hears once that it cannot be stopped, and the watcher tries again
//!   next tick.
//!
//! Auto-split is not implemented. The overseer that reads the `budget` change
//! starts the successor itself with `session_start(from_handoff=…)`.

use std::{sync::Arc, time::Duration};

use time::OffsetDateTime;
use vogt_engine_contract::{ActivityState, SessionBudget, SessionEconomics};

use crate::{app::AppState, events::EventBus, pty::Session, screen, sessions::SessionRegistry};

/// The deployment's budget settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Percent of a turn or context limit at which the soft warning fires
    /// (`ENGINE_BUDGET_SOFT_PCT`).
    pub soft_pct: u32,
    /// How often live sessions are checked (`ENGINE_BUDGET_TICK_SECS`).
    pub tick: Duration,
    /// `false` when the tick is 0: nothing is enforced.
    pub enabled: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            soft_pct: 80,
            tick: Duration::from_secs(15),
            enabled: true,
        }
    }
}

/// What one reading of a session's budget says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    None,
    Soft,
    Hard,
}

/// Which limit a [`Level`] came from, so the warning can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dimension {
    Turns,
    Context,
    WallClock,
}

/// The line typed at the prompt on a soft warning. It names no marker, so its
/// echo can never read as the agent finishing.
pub const SOFT_LINE: &str = "[vogt] budget: {what} ({n} of {limit}). \
Commit your work in progress on your branch, write a hand-off comment \
(work_comment kind=handoff), then stop.";

/// `used` against `limit`: missing or zero is no limit, at or over the limit
/// is hard, at or over `soft_pct` of it is soft.
fn against(used: u64, limit: Option<u64>, soft_pct: u32) -> Level {
    let Some(limit) = limit.filter(|l| *l > 0) else {
        return Level::None;
    };
    if used >= limit {
        return Level::Hard;
    }
    // Percent in integer arithmetic; a limit below 100 still crosses at the
    // same fraction the operator named.
    if used.saturating_mul(100) >= limit.saturating_mul(u64::from(soft_pct)) {
        return Level::Soft;
    }
    Level::None
}

/// Fold one dimension in. Hard wins outright; the first soft is the one the
/// warning names, so a later soft does not replace it.
fn worse(
    level: Level,
    from: Option<Dimension>,
    candidate: Level,
    dim: Dimension,
) -> (Level, Option<Dimension>) {
    match (level, candidate) {
        (_, Level::Hard) => (Level::Hard, Some(dim)),
        (Level::Hard, _) => (level, from),
        (Level::None, Level::Soft) => (Level::Soft, Some(dim)),
        _ => (level, from),
    }
}

/// The decision, pure over what was read. `running` is the time the session has
/// spent actually working, accumulated across every stretch and not reset by a
/// pause: the caller adds the stretch in progress and passes the total. Time
/// spent idle, at the prompt, at a dialog, or blocked on a person is never
/// added, but what already elapsed stays.
pub fn assess(
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running: Duration,
    soft_pct: u32,
) -> Level {
    let (level, _) = assess_with(budget, economics, running, soft_pct);
    level
}

fn assess_with(
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running: Duration,
    soft_pct: u32,
) -> (Level, Option<Dimension>) {
    let turns = against(
        economics.map(|e| e.turns.max(0) as u64).unwrap_or(0),
        budget.turns.map(u64::from),
        soft_pct,
    );
    let (mut level, mut from) = worse(Level::None, None, turns, Dimension::Turns);
    let context = against(
        economics
            .map(|e| e.context_tokens.max(0) as u64)
            .unwrap_or(0),
        budget.context_tokens,
        soft_pct,
    );
    (level, from) = worse(level, from, context, Dimension::Context);
    if level == Level::Hard {
        return (level, from);
    }
    // Wall clock is cumulative across the whole session, not one turn: an
    // autopilot worker that pauses between turns would otherwise reset it, and
    // one that never pauses would only trip if a single stretch outlasted it.
    // Hard the moment the accumulated running time reaches the limit, with no
    // soft warning ahead of it.
    if let Some(limit) = budget.wall_clock_minutes.filter(|m| *m > 0) {
        if running.as_secs() >= u64::from(limit) * 60 {
            return (Level::Hard, Some(Dimension::WallClock));
        }
    }
    (level, from)
}

/// What the warning says, and the numbers in it.
fn describe(
    dimension: Dimension,
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running: Duration,
) -> (String, String, String) {
    match dimension {
        Dimension::Turns => (
            "turns".to_string(),
            economics
                .map(|e| e.turns.max(0).to_string())
                .unwrap_or_else(|| "0".into()),
            budget.turns.unwrap_or(0).to_string(),
        ),
        Dimension::Context => (
            "context".to_string(),
            economics
                .map(|e| e.context_tokens.max(0).to_string())
                .unwrap_or_else(|| "0".into()),
            budget.context_tokens.unwrap_or(0).to_string(),
        ),
        Dimension::WallClock => (
            "wall clock".to_string(),
            format!("{} min", running.as_secs() / 60),
            format!("{} min", budget.wall_clock_minutes.unwrap_or(0)),
        ),
    }
}

fn summary_line(what: &str, n: &str, limit: &str) -> String {
    SOFT_LINE
        .replace("{what}", what)
        .replace("{n}", n)
        .replace("{limit}", limit)
}

/// What one tick does with one session, once the reading is in hand.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing, or a warning deferred because the session is mid-turn.
    Leave,
    /// Publish the soft change and, because it is at its prompt, type the line.
    Soft { summary: String },
    /// Publish the hard change, then hibernate. Every hard limit fires at once:
    /// a turn limit that waited for the prompt never landed on a worker that is
    /// re-prompted the moment it reaches one (WI-1281).
    Hard { summary: String },
}

/// The decision for one session this tick. `ready` is the screen's own: at its
/// prompt and not mid-turn. A soft warning that is not ready is
/// [`Action::Leave`], so the flag stays unset and the next tick retries. A hard
/// limit does not wait: the session is hibernated wherever the tick finds it.
fn decide(
    level: Level,
    dimension: Option<Dimension>,
    ready: bool,
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running: Duration,
) -> Action {
    let Some(dimension) = dimension else {
        return Action::Leave;
    };
    let (what, n, limit) = describe(dimension, budget, economics, running);
    match level {
        Level::None => Action::Leave,
        Level::Soft => {
            if !ready {
                return Action::Leave;
            }
            Action::Soft {
                summary: summary_line(&what, &n, &limit),
            }
        }
        Level::Hard => Action::Hard {
            summary: summary_line(&what, &n, &limit),
        },
    }
}

/// Whether the session is sitting at its prompt, ready to be typed into.
///
/// The guard is autopilot's: a blocked session is left alone, the agent must be
/// one the engine re-drives, and the prompt must have been quiet for the same
/// settle time a nudge waits. Anything else defers the warning or the hibernate
/// to the next tick.
async fn at_prompt(session: &Arc<Session>, settle: Duration) -> bool {
    if session.blocked().is_some() {
        return false;
    }
    let Some(agent) = session.agent() else {
        return false;
    };
    if !crate::autopilot::REDRIVEN.contains(&agent.as_str()) {
        return false;
    }
    if session.quiet_for() < settle {
        return false;
    }
    let activity = session.activity();
    if !matches!(
        activity,
        ActivityState::Idle | ActivityState::WaitingForInput
    ) {
        return false;
    }
    screen::session_screen(Arc::clone(session), 0)
        .await
        .map(|view| view.ready)
        .unwrap_or(false)
}

/// The running time to judge this tick against: stretches already finished,
/// plus the one in progress when the session is running now.
///
/// A pause ends the stretch and banks it, so the next one starts from nothing
/// while the total keeps what came before. Time spent idle, at the prompt, at a
/// dialog, or blocked on a person is never added.
fn account_running(session: &Session, running: bool, now: OffsetDateTime) -> Duration {
    let since = session.budget_running_since();
    if running {
        let started = since.unwrap_or(now);
        if since.is_none() {
            session.set_budget_running_since(Some(started));
        }
        let elapsed = now
            .unix_timestamp()
            .saturating_sub(started.unix_timestamp())
            .max(0) as u64;
        session.budget_running_total() + Duration::from_secs(elapsed)
    } else {
        if let Some(started) = since {
            let elapsed = now
                .unix_timestamp()
                .saturating_sub(started.unix_timestamp())
                .max(0) as u64;
            session.add_budget_running(Duration::from_secs(elapsed));
            session.set_budget_running_since(None);
        }
        session.budget_running_total()
    }
}

/// Whether this tick is a stretch of uninterrupted running: not waiting on a
/// person (a dialog or a blocked report) and not back at the prompt.
fn is_running(activity: ActivityState, blocked: bool) -> bool {
    let waiting_on_person = activity == ActivityState::AwaitingApproval || blocked;
    !waiting_on_person
        && !matches!(
            activity,
            ActivityState::Idle | ActivityState::WaitingForInput
        )
}

/// What one refused hard limit publishes. The notice has its own latch, so a
/// soft warning already sent does not suppress it, and a wall-clock hard that
/// fired before any soft does not suppress the soft that follows. `hard_done`
/// stays unset: a conversation pinned later is still hibernated.
#[derive(Debug, PartialEq, Eq)]
struct RefusalNotice {
    /// Publish the one hard notice and set the latch.
    publish: bool,
}

fn refusal_notice(already_noted: bool) -> RefusalNotice {
    RefusalNotice {
        publish: !already_noted,
    }
}

/// Type one line, then Enter on its own: a TUI that reads a burst ending in a
/// carriage return as a paste would keep the newline as text.
async fn type_line(session: &Session, line: &str) -> std::io::Result<()> {
    session.write_input(line.as_bytes())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    session.write_input(b"\r")
}

/// Look at every live session once.
pub async fn run_once(
    registry: &SessionRegistry,
    bus: &EventBus,
    policy: &Policy,
    settle: Duration,
    now: OffsetDateTime,
) {
    for session in registry.live_sessions() {
        if !session.is_alive() || session.budget_hard_done() {
            continue;
        }
        let Some(budget) = session.budget() else {
            continue;
        };
        // The wall clock is the session's own: every stretch of uninterrupted
        // running is added, and a dialog, a blocked report, or a return to the
        // prompt stops the clock without clearing what already elapsed.
        let running = account_running(
            &session,
            is_running(session.activity(), session.blocked().is_some()),
            now,
        );
        let economics = session.summary().economics;
        let (level, dimension) = assess_with(&budget, economics.as_ref(), running, policy.soft_pct);
        if level == Level::None {
            continue;
        }
        // Soft is sent once. A later hard is its own level, not a repeat of
        // the soft warning, so the flag only suppresses a second soft.
        if level == Level::Soft && session.budget_soft_sent() {
            continue;
        }
        let ready = at_prompt(&session, settle).await;
        let action = decide(
            level,
            dimension,
            ready,
            &budget,
            economics.as_ref(),
            running,
        );
        match action {
            Action::Leave => {}
            Action::Soft { summary } => {
                session.set_budget_soft_sent(true);
                bus.publish(vogt_engine_contract::ServerEvent::SessionBudget {
                    id: session.id,
                    level: "soft".to_string(),
                    summary: summary.clone(),
                });
                if let Err(e) = type_line(&session, &summary).await {
                    tracing::warn!(
                        session = %session.id,
                        error = %e,
                        event = "session.budget",
                        "could not type the soft budget warning"
                    );
                    continue;
                }
                tracing::info!(
                    session = %session.id,
                    level = "soft",
                    event = "session.budget",
                    "{summary}"
                );
            }
            Action::Hard { summary } => {
                let id = session.id;
                // The refusal is read against the session the registry holds
                // now, not the snapshot this tick started with: the session
                // may have been replaced since the tick began.
                let Some(current) = registry.live_sessions().into_iter().find(|s| s.id == id)
                else {
                    continue;
                };
                if let Some(refusal) = registry.hibernate_refusal(&current, false) {
                    // Once, on its own latch: a soft warning has usually gone out
                    // already, and a wall-clock hard can fire before any soft, so
                    // budget_soft_sent cannot carry this. It is not hard_done, so
                    // a conversation pinned later is still hibernated.
                    if refusal_notice(current.budget_refusal_noted()).publish {
                        current.set_budget_refusal_noted(true);
                        bus.publish(vogt_engine_contract::ServerEvent::SessionBudget {
                            id,
                            level: "hard".to_string(),
                            summary: "budget exceeded; session cannot be hibernated \
                                      (no resumable conversation)"
                                .to_string(),
                        });
                        tracing::warn!(
                            session = %id,
                            refusal = %refusal,
                            event = "session.budget",
                            "a session over its budget cannot be hibernated"
                        );
                    }
                    continue;
                }
                // Set before the call: hibernate copies the flag into the
                // record it writes, and a wake must not come back unenforced.
                // Cleared again if the call fails, so the next tick retries.
                current.set_budget_hard_done(true);
                match registry
                    .hibernate(
                        id,
                        Some(summary.clone()),
                        vogt_engine_contract::HibernateTrigger::Budget,
                        false,
                        None,
                    )
                    .await
                {
                    Ok(_) => {
                        bus.publish(vogt_engine_contract::ServerEvent::SessionBudget {
                            id,
                            level: "hard".to_string(),
                            summary: summary.clone(),
                        });
                        tracing::info!(
                            session = %id,
                            level = "hard",
                            event = "session.budget",
                            "{summary}"
                        );
                    }
                    Err(e) => {
                        if let Some(kept) =
                            registry.live_sessions().into_iter().find(|s| s.id == id)
                        {
                            kept.set_budget_hard_done(false);
                        }
                        tracing::warn!(
                            session = %id,
                            error = %e,
                            event = "session.budget",
                            "could not hibernate a session over its budget"
                        );
                    }
                }
            }
        }
    }
}

/// Run the watcher, unless the deployment turned it off.
pub fn spawn_watcher(state: Arc<AppState>) {
    let policy = state.config.budget.clone();
    if !policy.enabled {
        return;
    }
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(policy.tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            run_once(
                &state.sessions,
                &state.bus,
                &policy,
                state.config.autopilot.nudge_after,
                OffsetDateTime::now_utc(),
            )
            .await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget(turns: Option<u32>, context: Option<u64>, wall: Option<u32>) -> SessionBudget {
        SessionBudget {
            turns,
            context_tokens: context,
            wall_clock_minutes: wall,
        }
    }

    fn economics(turns: i64, context: i64) -> SessionEconomics {
        SessionEconomics {
            turns,
            context_tokens: context,
            ..SessionEconomics::default()
        }
    }

    fn mins(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    #[test]
    fn each_dimension_crosses_soft_then_hard_and_zero_is_no_limit() {
        // Turns: 80 of 100 is soft, 100 of 100 is hard, and no economics at
        // all is nothing.
        assert_eq!(
            assess(
                &budget(Some(100), None, None),
                Some(&economics(79, 0)),
                mins(0),
                80
            ),
            Level::None
        );
        assert_eq!(
            assess(
                &budget(Some(100), None, None),
                Some(&economics(80, 0)),
                mins(0),
                80
            ),
            Level::Soft
        );
        assert_eq!(
            assess(
                &budget(Some(100), None, None),
                Some(&economics(100, 0)),
                mins(0),
                80
            ),
            Level::Hard
        );
        assert_eq!(
            assess(&budget(Some(100), None, None), None, mins(0), 80),
            Level::None
        );
        // A zero ceiling, like a missing one, is not a limit.
        assert_eq!(
            assess(
                &budget(Some(0), None, None),
                Some(&economics(5, 0)),
                mins(0),
                80
            ),
            Level::None
        );
        // Context, the same shape.
        assert_eq!(
            assess(
                &budget(None, Some(1_000), None),
                Some(&economics(0, 800)),
                mins(0),
                80
            ),
            Level::Soft
        );
        assert_eq!(
            assess(
                &budget(None, Some(1_000), None),
                Some(&economics(0, 1_000)),
                mins(0),
                80
            ),
            Level::Hard
        );
        assert_eq!(
            assess(
                &budget(None, Some(0), None),
                Some(&economics(0, 5_000)),
                mins(0),
                80
            ),
            Level::None
        );
    }

    #[test]
    fn hard_beats_soft_when_both_apply() {
        // Turns are merely soft; context is already hard.
        assert_eq!(
            assess(
                &budget(Some(100), Some(1_000), None),
                Some(&economics(80, 1_000)),
                mins(0),
                80
            ),
            Level::Hard
        );
    }

    #[test]
    fn wall_clock_is_cumulative_running_time() {
        // 90 minutes of running against a 60 minute ceiling is hard, whether it
        // came as one stretch or as several that a pause separated.
        assert_eq!(
            assess(&budget(None, None, Some(60)), None, mins(90), 80),
            Level::Hard
        );
        // Under the ceiling.
        assert_eq!(
            assess(&budget(None, None, Some(60)), None, mins(30), 80),
            Level::None
        );
        // A pause adds nothing, so time spent waiting never trips it.
        assert_eq!(
            assess(&budget(None, None, Some(60)), None, mins(0), 80),
            Level::None
        );
        // A zero ceiling is not a limit, however long the session has run.
        assert_eq!(
            assess(&budget(None, None, Some(0)), None, mins(90), 80),
            Level::None
        );
    }

    #[test]
    fn a_pause_keeps_the_wall_clock_it_already_counted() {
        let limit = budget(None, None, Some(90));
        // 80 minutes, then a pause, then 20 more. The pause adds nothing, but
        // the 80 stays, so the total crosses the ceiling where a per-turn clock
        // would have started again at nothing.
        assert_eq!(assess(&limit, None, mins(80), 80), Level::None);
        assert_eq!(assess(&limit, None, mins(80) + mins(20), 80), Level::Hard);
    }

    #[test]
    fn the_refusal_notice_is_its_own_latch() {
        // Unpublished until noted, then never again. The soft latch is not an
        // input: a soft warning already sent, or a soft that has not fired yet,
        // cannot suppress this or be suppressed by it.
        assert!(refusal_notice(false).publish);
        assert!(!refusal_notice(true).publish);
    }

    #[test]
    fn only_uninterrupted_work_counts_as_running() {
        assert!(is_running(ActivityState::Running, false));
        assert!(is_running(ActivityState::Errored, false));
        assert!(!is_running(ActivityState::AwaitingApproval, false));
        assert!(!is_running(ActivityState::Running, true));
        assert!(!is_running(ActivityState::Idle, false));
        assert!(!is_running(ActivityState::WaitingForInput, false));
    }

    #[test]
    fn a_hard_limit_hibernates_without_waiting_for_the_prompt() {
        let b = budget(Some(100), None, None);
        // Soft still waits: mid-turn there is nothing to type into.
        let econ = economics(80, 0);
        let soft = assess_with(&b, Some(&econ), mins(0), 80);
        assert_eq!(
            decide(soft.0, soft.1, false, &b, Some(&econ), mins(0)),
            Action::Leave
        );
        assert!(matches!(
            decide(soft.0, soft.1, true, &b, Some(&econ), mins(0)),
            Action::Soft { .. }
        ));
        // A turn limit that is already over does not. An autopilot worker is
        // re-prompted the moment it reaches its prompt, so waiting for one let
        // it run on past the limit (WI-1281).
        let hard_econ = economics(120, 0);
        let hard = assess_with(&b, Some(&hard_econ), mins(0), 80);
        assert!(matches!(
            decide(hard.0, hard.1, false, &b, Some(&hard_econ), mins(0)),
            Action::Hard { .. }
        ));
        // The wall clock, the same: hard at once, mid-turn.
        let wall = assess_with(&budget(None, None, Some(60)), None, mins(90), 80);
        assert!(matches!(
            decide(
                wall.0,
                wall.1,
                false,
                &budget(None, None, Some(60)),
                None,
                mins(90)
            ),
            Action::Hard { .. }
        ));
    }
}
