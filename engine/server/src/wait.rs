//! Waiting for a session, server-side, instead of polling it.
//!
//! A program driving another session used to poll `/screen` on a timer, and a
//! ten-minute poll missed a 90-second permission dialog (WI-874). `GET
//! /api/sessions/{id}/wait` blocks on the same event bus the SSE stream
//! carries and answers once the session reaches what the caller is waiting
//! for — or needs a person, or exits, or the timeout runs out — with the
//! screen at that moment.
//!
//! The bus is subscribed to *before* the session is first checked, so a
//! change that lands between the check and the first `recv` is not lost; a
//! lagging receiver re-checks rather than trusting the events it missed.

use std::{sync::Arc, time::Duration};

use tokio::sync::broadcast::error::RecvError;
use vogt_engine_contract::{ActivityState, SessionWait, WaitUntil};

use crate::{
    error::{ApiError, Result},
    events::{EventBus, ServerEvent},
    pty::Session,
    screen,
};

/// The longest a single wait may block.
pub const MAX_WAIT: Duration = Duration::from_secs(600);

/// Why a wait ends, before the screen is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ready,
    AwaitingApproval,
    Blocked,
    Exited,
    Changed,
    Timeout,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ready => "ready",
            Outcome::AwaitingApproval => "awaiting-approval",
            Outcome::Blocked => "blocked",
            Outcome::Exited => "exited",
            Outcome::Changed => "changed",
            Outcome::Timeout => "timeout",
        }
    }

    fn matches(self, until: WaitUntil) -> bool {
        match until {
            WaitUntil::Ready => self == Outcome::Ready,
            WaitUntil::Exited => self == Outcome::Exited,
            WaitUntil::AnyChange => self != Outcome::Timeout,
        }
    }
}

/// Whether the session, as it is now, ends a wait for `until`.
async fn check(session: &Arc<Session>, until: WaitUntil) -> Result<Option<Outcome>> {
    if !session.is_alive() {
        return Ok(Some(Outcome::Exited));
    }
    if until != WaitUntil::Ready {
        return Ok(None);
    }
    // These end a wait for `ready` because no amount of waiting makes the
    // session ready: a person has to act first.
    if session.blocked().is_some() {
        return Ok(Some(Outcome::Blocked));
    }
    match session.activity() {
        ActivityState::AwaitingApproval => Ok(Some(Outcome::AwaitingApproval)),
        ActivityState::WaitingForInput => Ok(Some(Outcome::Ready)),
        ActivityState::Idle => {
            let screen = screen::session_screen(Arc::clone(session), 0).await?;
            Ok(screen.ready.then_some(Outcome::Ready))
        }
        _ => Ok(None),
    }
}

/// Whether `event` is about this session's state.
fn concerns(event: &ServerEvent, id: uuid::Uuid) -> bool {
    match event {
        ServerEvent::Activity { id: e, .. }
        | ServerEvent::SessionKilled { id: e, .. }
        | ServerEvent::SessionBlocked { id: e, .. } => *e == id,
        _ => false,
    }
}

/// Block until `session` reaches `until` (or needs a person, or exits), or
/// `timeout` passes; then answer with the screen.
pub async fn wait(
    bus: &EventBus,
    session: Arc<Session>,
    until: WaitUntil,
    timeout: Duration,
) -> Result<SessionWait> {
    if timeout > MAX_WAIT {
        return Err(ApiError::BadRequest(format!(
            "timeout_s is at most {}",
            MAX_WAIT.as_secs()
        )));
    }
    let mut rx = bus.subscribe();
    let started = tokio::time::Instant::now();
    let deadline = started + timeout;
    let mut outcome = if until == WaitUntil::AnyChange {
        // A change is something that happens after the call, but an exited
        // session will never change again.
        (!session.is_alive()).then_some(Outcome::Exited)
    } else {
        check(&session, until).await?
    };
    while outcome.is_none() {
        let event = tokio::time::timeout_at(deadline, rx.recv()).await;
        outcome = match event {
            Err(_) => Some(Outcome::Timeout),
            Ok(Err(RecvError::Closed)) => {
                return Err(ApiError::Internal("the event bus closed".into()));
            }
            // Missed events: the session's state is the truth, not the gap.
            Ok(Err(RecvError::Lagged(skipped))) => {
                bus.note_lag("session-wait", skipped);
                match until {
                    WaitUntil::AnyChange => Some(Outcome::Changed),
                    _ => check(&session, until).await?,
                }
            }
            Ok(Ok(event)) if concerns(&event, session.id) => match until {
                WaitUntil::AnyChange => Some(if session.is_alive() {
                    Outcome::Changed
                } else {
                    Outcome::Exited
                }),
                _ => check(&session, until).await?,
            },
            Ok(Ok(_)) => None,
        };
    }
    let outcome = outcome.unwrap_or(Outcome::Timeout);
    let screen = screen::session_screen(Arc::clone(&session), 0).await?;
    Ok(SessionWait {
        outcome: outcome.as_str().to_string(),
        matched: outcome.matches(until),
        waited_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        screen,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outcome_matches_only_what_was_asked_for() {
        assert!(Outcome::Ready.matches(WaitUntil::Ready));
        assert!(!Outcome::AwaitingApproval.matches(WaitUntil::Ready));
        assert!(!Outcome::Blocked.matches(WaitUntil::Ready));
        assert!(!Outcome::Exited.matches(WaitUntil::Ready));
        assert!(Outcome::Exited.matches(WaitUntil::Exited));
        assert!(Outcome::Changed.matches(WaitUntil::AnyChange));
        assert!(Outcome::Exited.matches(WaitUntil::AnyChange));
        assert!(!Outcome::Timeout.matches(WaitUntil::AnyChange));
        assert_eq!(Outcome::AwaitingApproval.as_str(), "awaiting-approval");
    }
}
