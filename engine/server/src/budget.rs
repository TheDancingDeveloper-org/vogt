//! Engine-enforced session budgets (WI-1245 / C7).
//!
//! A session started with a [`SessionBudget`] is watched against the economics
//! the tailer last published on its summary. Two levels:
//!
//! - **soft**, once, at `soft_pct` of a turn, context or whole-session clock
//!   limit: a `budget` change, and — only while the session is at its prompt —
//!   one typed line telling the agent to commit, write a hand-off, and stop.
//!   Mid-turn, the line waits for the next tick that finds it ready.
//! - **hard**, once: the same change, then the session is hibernated with
//!   trigger `budget`. A turn, context or whole-session clock limit waits for
//!   the prompt first, so the stop lands at a turn boundary and never in the
//!   middle of a tool call or a write; a per-turn wall-clock limit (one turn
//!   running longer than `wall_clock_minutes`, the looping signature)
//!   hibernates at once, because that turn may never end. A session the engine
//!   cannot hibernate (no conversation to resume) is not marked done: the
//!   overseer hears once that it cannot be stopped, and the watcher tries again
//!   next tick.
//!
//! Waiting for the prompt only works if something lets the session reach it.
//! An autopilot worker is re-prompted the moment it gets there, so a limit that
//! merely waited landed many turns late or never (WI-1281). While a warning or
//! a hard stop is due and the session is not ready, the watcher *holds* it:
//! autopilot does not nudge a held session, the turn in progress ends, the
//! session rests at its prompt, and the next tick acts.
//!
//! Two clocks count only time the session is seen running — not idle, at its
//! prompt, at a permission dialog, or blocked on a person:
//!
//! - the per-turn clock (`wall_clock_minutes`) starts when the session is seen
//!   working and is reset to nothing by any pause;
//! - the whole-session clock (`session_wall_clock_minutes`, WI-1281) banks each
//!   stretch when it ends, so a pause stops it without clearing it. The bank
//!   is kept on the hibernation record, so a wake (and the redeploy that
//!   hibernates and wakes every session) resumes it. An engine that dies
//!   without hibernating loses the stretches since the last hibernation. It is
//!   unset unless a session's or project's budget names it.
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
    /// The whole session's accumulated running time (WI-1281).
    SessionClock,
    /// One turn's running time: the looping signature.
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

/// The decision, pure over what was read. `running_since` is when the session
/// was last seen actually running and has been running continuously since: the
/// caller passes it only while the session is working, and passes `None` after
/// any idle, prompt, dialog, or blocked stretch, so time spent waiting on a
/// person never counts toward the per-turn wall clock and does not resume
/// afterwards. `session_running` is the whole session's running time, banked
/// stretches plus the one in progress.
pub fn assess(
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running_since: Option<OffsetDateTime>,
    session_running: Duration,
    now: OffsetDateTime,
    soft_pct: u32,
) -> Level {
    let (level, _) = assess_with(
        budget,
        economics,
        running_since,
        session_running,
        now,
        soft_pct,
    );
    level
}

fn assess_with(
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running_since: Option<OffsetDateTime>,
    session_running: Duration,
    now: OffsetDateTime,
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
    // The whole-session clock is a budget like turns: soft at `soft_pct`, hard
    // at the limit, and both wait for the prompt.
    let session_clock = against(
        session_running.as_secs(),
        budget.session_wall_clock_minutes.map(|m| u64::from(m) * 60),
        soft_pct,
    );
    (level, from) = worse(level, from, session_clock, Dimension::SessionClock);
    if level == Level::Hard {
        return (level, from);
    }
    // Wall clock is the looping signature: hard the moment one turn has run
    // longer than the limit, with no soft warning ahead of it.
    if let (Some(limit), Some(started)) = (budget.wall_clock_minutes, running_since) {
        if limit > 0 {
            let elapsed = now
                .unix_timestamp()
                .saturating_sub(started.unix_timestamp());
            if elapsed >= i64::from(limit) * 60 {
                return (Level::Hard, Some(Dimension::WallClock));
            }
        }
    }
    (level, from)
}

/// What the warning says, and the numbers in it.
fn describe(
    dimension: Dimension,
    budget: &SessionBudget,
    economics: Option<&SessionEconomics>,
    running_since: Option<OffsetDateTime>,
    session_running: Duration,
    now: OffsetDateTime,
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
        Dimension::SessionClock => (
            "session wall clock".to_string(),
            format!("{} min", session_running.as_secs() / 60),
            format!("{} min", budget.session_wall_clock_minutes.unwrap_or(0)),
        ),
        Dimension::WallClock => {
            let minutes = running_since
                .map(|s| now.unix_timestamp().saturating_sub(s.unix_timestamp()) / 60)
                .unwrap_or(0);
            (
                "wall clock".to_string(),
                format!("{minutes} min"),
                format!("{} min", budget.wall_clock_minutes.unwrap_or(0)),
            )
        }
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
    /// Publish the hard change, then hibernate. `at_once` is the per-turn
    /// wall-clock case, which does not wait for the prompt.
    Hard { summary: String, at_once: bool },
}

/// The reading this tick acts on: the budget, the economics, and both clocks.
struct Reading<'a> {
    budget: &'a SessionBudget,
    economics: Option<&'a SessionEconomics>,
    running_since: Option<OffsetDateTime>,
    session_running: Duration,
    now: OffsetDateTime,
}

/// The decision for one session this tick. `ready` is the screen's own: at its
/// prompt and not mid-turn. A soft, or a hard that is not the per-turn wall
/// clock, that is not ready is [`Action::Leave`], so the flags stay unset and
/// the next tick retries.
fn decide(level: Level, dimension: Option<Dimension>, ready: bool, r: &Reading<'_>) -> Action {
    let Some(dimension) = dimension else {
        return Action::Leave;
    };
    let (what, n, limit) = describe(
        dimension,
        r.budget,
        r.economics,
        r.running_since,
        r.session_running,
        r.now,
    );
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
        Level::Hard => {
            let at_once = dimension == Dimension::WallClock;
            if !at_once && !ready {
                return Action::Leave;
            }
            Action::Hard {
                summary: summary_line(&what, &n, &limit),
                at_once,
            }
        }
    }
}

/// Whether the watcher holds the session this tick: an action is due (the
/// level is soft or hard) but [`decide`] deferred it to the prompt. A held
/// autopilot session is not re-prompted, so it comes to rest and the action
/// lands at the turn boundary.
fn holds(level: Level, action: &Action) -> bool {
    level != Level::None && *action == Action::Leave
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

/// The next value of the running stretch, and the time to bank from the one
/// that just ended. Running with nothing stored starts it at `now`; running
/// with a stored start keeps it; not running clears it — so the per-turn clock
/// begins from nothing after a dialog — and banks what it had counted for the
/// whole-session clock.
fn observe_running(
    stored: Option<OffsetDateTime>,
    running: bool,
    now: OffsetDateTime,
) -> (Option<OffsetDateTime>, Duration) {
    if running {
        return (stored.or(Some(now)), Duration::ZERO);
    }
    let ended = stored
        .map(|since| {
            Duration::from_secs(
                now.unix_timestamp()
                    .saturating_sub(since.unix_timestamp())
                    .max(0) as u64,
            )
        })
        .unwrap_or_default();
    (None, ended)
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

/// Advance a session's clocks to `now`: the per-turn stretch, and the bank the
/// whole-session clock adds it to when it ends. Returns the stretch in
/// progress.
fn account_running(session: &Session, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let (since, ended) = observe_running(
        session.budget_running_since(),
        is_running(session.activity(), session.blocked().is_some()),
        now,
    );
    if !ended.is_zero() {
        session.set_budget_session_banked(session.budget_session_banked() + ended);
    }
    session.set_budget_running_since(since);
    since
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

/// Tell the overseer, once, that a session over its budget cannot be
/// hibernated. Never holds it: a session that cannot be stopped is left to
/// work rather than parked at its prompt.
fn note_refusal(session: &Session, bus: &EventBus, refusal: &str) {
    session.set_budget_hold(false);
    if !refusal_notice(session.budget_refusal_noted()).publish {
        return;
    }
    session.set_budget_refusal_noted(true);
    bus.publish(vogt_engine_contract::ServerEvent::SessionBudget {
        id: session.id,
        level: "hard".to_string(),
        summary: "budget exceeded; session cannot be hibernated \
                  (no resumable conversation)"
            .to_string(),
    });
    tracing::warn!(
        session = %session.id,
        refusal = %refusal,
        event = "session.budget",
        "a session over its budget cannot be hibernated"
    );
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
        // The per-turn clock counts only uninterrupted running. A dialog, a
        // blocked report, or a return to the prompt ends the stretch, so the
        // next one starts from nothing rather than resuming the turn the
        // dialog paused; the whole-session clock banks it instead.
        let running_since = account_running(&session, now);
        let session_running = session.budget_session_running(now);
        let economics = session.summary().economics;
        let reading = Reading {
            budget: &budget,
            economics: economics.as_ref(),
            running_since,
            session_running,
            now,
        };
        let (level, dimension) = assess_with(
            &budget,
            economics.as_ref(),
            running_since,
            session_running,
            now,
            policy.soft_pct,
        );
        if level == Level::None {
            session.set_budget_hold(false);
            continue;
        }
        // Soft is sent once. A later hard is its own level, not a repeat of
        // the soft warning, so the flag only suppresses a second soft.
        if level == Level::Soft && session.budget_soft_sent() {
            session.set_budget_hold(false);
            continue;
        }
        // A hard stop the engine could never carry out must not park the
        // session at its prompt waiting for it.
        if level == Level::Hard {
            if let Some(refusal) = registry.hibernate_refusal(&session, false) {
                note_refusal(&session, bus, &refusal.to_string());
                continue;
            }
        }
        let ready = at_prompt(&session, settle).await;
        let action = decide(level, dimension, ready, &reading);
        session.set_budget_hold(holds(level, &action));
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
            Action::Hard { summary, .. } => {
                let id = session.id;
                // The refusal is read against the session the registry holds
                // now, not the snapshot this tick started with: the session
                // may have been replaced since the tick began.
                let Some(current) = registry.live_sessions().into_iter().find(|s| s.id == id)
                else {
                    continue;
                };
                if let Some(refusal) = registry.hibernate_refusal(&current, false) {
                    note_refusal(&current, bus, &refusal.to_string());
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

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    }

    fn budget(turns: Option<u32>, context: Option<u64>, wall: Option<u32>) -> SessionBudget {
        SessionBudget {
            turns,
            context_tokens: context,
            wall_clock_minutes: wall,
            session_wall_clock_minutes: None,
        }
    }

    fn session_budget(minutes: u32) -> SessionBudget {
        SessionBudget {
            session_wall_clock_minutes: Some(minutes),
            ..SessionBudget::default()
        }
    }

    fn economics(turns: i64, context: i64) -> SessionEconomics {
        SessionEconomics {
            turns,
            context_tokens: context,
            ..SessionEconomics::default()
        }
    }

    const NONE: Duration = Duration::ZERO;

    fn reading<'a>(
        b: &'a SessionBudget,
        econ: Option<&'a SessionEconomics>,
        since: Option<OffsetDateTime>,
        session_running: Duration,
    ) -> Reading<'a> {
        Reading {
            budget: b,
            economics: econ,
            running_since: since,
            session_running,
            now: now(),
        }
    }

    #[test]
    fn each_dimension_crosses_soft_then_hard_and_zero_is_no_limit() {
        let at = now();
        // Turns: 80 of 100 is soft, 100 of 100 is hard, and no economics at
        // all is nothing.
        let turns = budget(Some(100), None, None);
        let level =
            |b: &SessionBudget, e: Option<&SessionEconomics>| assess(b, e, None, NONE, at, 80);
        assert_eq!(level(&turns, Some(&economics(79, 0))), Level::None);
        assert_eq!(level(&turns, Some(&economics(80, 0))), Level::Soft);
        assert_eq!(level(&turns, Some(&economics(100, 0))), Level::Hard);
        assert_eq!(level(&turns, None), Level::None);
        // A zero ceiling, like a missing one, is not a limit.
        assert_eq!(
            level(&budget(Some(0), None, None), Some(&economics(5, 0))),
            Level::None
        );
        // Context, the same shape.
        let context = budget(None, Some(1_000), None);
        assert_eq!(level(&context, Some(&economics(0, 800))), Level::Soft);
        assert_eq!(level(&context, Some(&economics(0, 1_000))), Level::Hard);
        assert_eq!(
            level(&budget(None, Some(0), None), Some(&economics(0, 5_000))),
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
                None,
                NONE,
                now(),
                80
            ),
            Level::Hard
        );
    }

    #[test]
    fn wall_clock_is_hard_only_while_a_turn_is_running() {
        let at = now();
        let started = at - time::Duration::minutes(90);
        let level = |b: &SessionBudget, since| assess(b, None, since, NONE, at, 80);
        // 90 minutes into a turn, against a 60 minute ceiling.
        assert_eq!(
            level(&budget(None, None, Some(60)), Some(started)),
            Level::Hard
        );
        // The same elapsed time, but no turn is running: the caller passes
        // None, and nothing fires.
        assert_eq!(level(&budget(None, None, Some(60)), None), Level::None);
        // Under the ceiling.
        let recent = at - time::Duration::minutes(30);
        assert_eq!(
            level(&budget(None, None, Some(60)), Some(recent)),
            Level::None
        );
        // A zero ceiling is not a limit.
        assert_eq!(
            level(&budget(None, None, Some(0)), Some(started)),
            Level::None
        );
    }

    #[test]
    fn a_dialog_resets_the_wall_clock_instead_of_pausing_it() {
        let at = now();
        let limit = budget(None, None, Some(90));
        let level = |since| assess(&limit, None, since, NONE, at, 80);
        // 80 minutes of running is inside a 90 minute ceiling.
        assert_eq!(level(Some(at - time::Duration::minutes(80))), Level::None);
        // A dialog, a blocked report, or a return to the prompt clears the
        // clock: the caller passes None, however long the pause lasted.
        assert_eq!(level(None), Level::None);
        // Back to work. Only the new stretch counts, so 5 minutes of it does
        // not inherit the 80 that came before the dialog.
        assert_eq!(level(Some(at - time::Duration::minutes(5))), Level::None);
        // The new stretch on its own still trips once it passes the ceiling.
        assert_eq!(level(Some(at - time::Duration::minutes(90))), Level::Hard);
    }

    /// WI-1281: the per-turn clock is not cumulative. A session that has run
    /// for hours in short turns is untouched by `wall_clock_minutes`, and a
    /// budget that names only it (the 120 turn / 90 minute default) never sets
    /// a whole-session limit.
    #[test]
    fn the_per_turn_clock_ignores_the_whole_sessions_running_time() {
        let at = now();
        let b = budget(Some(120), None, Some(90));
        assert_eq!(b.session_wall_clock_minutes, None);
        let hours = Duration::from_secs(10 * 3600);
        let short_turn = Some(at - time::Duration::minutes(5));
        assert_eq!(assess(&b, None, short_turn, hours, at, 80), Level::None);
    }

    #[test]
    fn the_session_clock_is_soft_then_hard_and_unset_is_no_limit() {
        let at = now();
        let b = session_budget(100);
        let level = |b: &SessionBudget, minutes: u64| {
            assess(b, None, None, Duration::from_secs(minutes * 60), at, 80)
        };
        assert_eq!(level(&b, 79), Level::None);
        assert_eq!(level(&b, 80), Level::Soft);
        assert_eq!(level(&b, 100), Level::Hard);
        assert_eq!(level(&session_budget(0), 1_000), Level::None);
        assert_eq!(level(&SessionBudget::default(), 1_000), Level::None);
        let (_, dim) = assess_with(&b, None, None, Duration::from_secs(6_000), at, 80);
        assert_eq!(dim, Some(Dimension::SessionClock));
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
    fn a_pause_clears_the_stretch_and_banks_it() {
        let at = now();
        let earlier = at - time::Duration::minutes(80);
        // Nothing stored and running: the stretch starts now, nothing banked.
        assert_eq!(observe_running(None, true, at), (Some(at), NONE));
        // Already running: the stored start is kept, not moved forward.
        assert_eq!(
            observe_running(Some(earlier), true, at),
            (Some(earlier), NONE)
        );
        // A dialog, a blocked report, or the prompt: the stretch is cleared
        // however long it had been running, and its 80 minutes are banked.
        assert_eq!(
            observe_running(Some(earlier), false, at),
            (None, Duration::from_secs(80 * 60))
        );
        // Paused with nothing running: nothing to bank.
        assert_eq!(observe_running(None, false, at), (None, NONE));
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
    fn a_warning_waits_for_the_prompt_but_wall_clock_does_not() {
        let at = now();
        let b = budget(Some(100), None, None);
        let econ = economics(80, 0);
        let r = reading(&b, Some(&econ), None, NONE);
        let soft = assess_with(&b, Some(&econ), None, NONE, at, 80);
        assert_eq!(decide(soft.0, soft.1, false, &r), Action::Leave);
        assert!(holds(soft.0, &Action::Leave));
        assert!(matches!(
            decide(soft.0, soft.1, true, &r),
            Action::Soft { .. }
        ));
        let hard_econ = economics(100, 0);
        let r = reading(&b, Some(&hard_econ), None, NONE);
        let hard = assess_with(&b, Some(&hard_econ), None, NONE, at, 80);
        assert_eq!(decide(hard.0, hard.1, false, &r), Action::Leave);
        assert!(matches!(
            decide(hard.0, hard.1, true, &r),
            Action::Hard { at_once: false, .. }
        ));
        // Wall clock: hard at once, even though the session is not ready.
        let started = at - time::Duration::minutes(90);
        let wb = budget(None, None, Some(60));
        let wall = assess_with(&wb, None, Some(started), NONE, at, 80);
        let action = decide(
            wall.0,
            wall.1,
            false,
            &reading(&wb, None, Some(started), NONE),
        );
        assert!(matches!(action, Action::Hard { at_once: true, .. }));
        assert!(!holds(wall.0, &action));
        // Nothing due holds nothing.
        assert!(!holds(Level::None, &Action::Leave));
    }

    /// WI-1281: the whole-session clock is a safe-point stop. Mid-turn it waits
    /// (and holds autopilot off); at the prompt it hibernates.
    #[test]
    fn the_session_clock_hard_stop_waits_for_the_prompt() {
        let at = now();
        let b = session_budget(60);
        let spent = Duration::from_secs(61 * 60);
        let r = reading(&b, None, Some(at), spent);
        let hard = assess_with(&b, None, Some(at), spent, at, 80);
        assert_eq!(hard.0, Level::Hard);
        let mid_turn = decide(hard.0, hard.1, false, &r);
        assert_eq!(mid_turn, Action::Leave);
        assert!(holds(hard.0, &mid_turn));
        match decide(hard.0, hard.1, true, &r) {
            Action::Hard { summary, at_once } => {
                assert!(!at_once);
                assert!(
                    summary.contains("session wall clock (61 min of 60 min)"),
                    "{summary}"
                );
            }
            other => panic!("expected a hard stop, got {other:?}"),
        }
    }
}
