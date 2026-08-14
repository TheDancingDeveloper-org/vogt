//! New drift, routed to a phone (FR-M2).
//!
//! FR-M2 names four things worth interrupting someone for. Three of them are
//! the engine's own — a session entering `waiting-for-input` or `errored`,
//! and the agent-task notify hook — and the engine both observes and
//! dispatches those. The fourth is not: drift is raised in vogt-core, which
//! holds no push subscriptions, while the engine holds every subscription and
//! knows nothing about drift. Something has to cross that gap.
//!
//! **Why polling rather than anything better.** The core has no outbound
//! webhook, no callback registration and no push of its own; what it has is
//! `events.list`, a cursor feed over a single monotonic `seq` with an
//! explicit contract that an empty page returns the caller's own cursor
//! rather than rewinding. That is a feed built to be polled, and polling it
//! costs one indexed `SELECT ... WHERE seq > ?` a minute against a loopback
//! SQLite database in the same container. The alternative — teaching the core
//! to call the engine — would put a new outbound dependency in the half that
//! is meant to be the quiet one, and would need a credential pointing the
//! wrong way through the front door. Polling is the smaller thing.
//!
//! **What it must not do**, in the order the failures actually hurt:
//!
//! * **Spam a phone after a restart.** The cursor is persisted, and it is
//!   persisted *before* the notification is dispatched rather than after. The
//!   two orderings each lose something: persist-first can drop a notification
//!   if the process dies in between, persist-last can send the same one
//!   twice. A dropped drift notification is recoverable — the proposal is
//!   still sitting in the inbox on the projects page, unresolved, and the
//!   next sweep will not hide it. A phone that buzzes eleven times for drift
//!   it was already told about is what makes someone turn the channel off,
//!   and then the `waiting-for-input` notification that mattered goes with
//!   it. So this loses rather than repeats, deliberately.
//!
//! * **Announce history as news.** A front door that has never run this
//!   watcher has no cursor, and an estate that has been collecting for months
//!   has thousands of events. Starting from zero would deliver every drift
//!   proposal ever raised as a fresh interruption. So a first run *seeds* —
//!   it walks to the end of the feed, notifies nobody, and records where the
//!   end was. The first notification this watcher ever sends is for drift
//!   raised after it started watching, which is the only honest reading of
//!   "new drift".
//!
//! * **Make noise about an outage.** The core being down is a supported state
//!   (FR-E9) and it is reported at `/readyz` already. A poller that logged a
//!   warning every minute would turn a known, visible outage into log spam
//!   that hides everything else. So failures are logged on the *transition*
//!   into and out of the failing state, not on every tick, and the cursor
//!   does not move while the core cannot be read.
//!
//! And it does not run at all when no core is configured — the engine without
//! vogt-core is a supported deployment, and a watcher polling a URL that was
//! never set would be a permanent error condition in a product that is
//! working exactly as intended.

use std::{path::PathBuf, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{debug, info, warn};

use crate::{app::AppState, push::NotificationKind};

/// The event kind vogt-core publishes when a drift proposal is opened. It is
/// `DRIFT_RAISED_EVENT` in `services/drift_service.py`; `drift.resolved` is
/// deliberately not watched, because somebody resolving drift is somebody
/// already looking at it.
const DRIFT_RAISED: &str = "drift.raised";

/// How often the feed is read. A minute is well inside the useful latency for
/// "come and look at this" and far outside anything that could load the core.
const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Page size. The core caps `limit` at 1000; this is the size used both for
/// steady-state polling and for walking to the tip on a first run.
const PAGE: u32 = 500;

/// Above this many drift proposals in one poll, the notification stops naming
/// them and starts counting them. Eleven separate buzzes is the failure this
/// whole module is written to avoid, so a burst collapses into one.
const NAME_AT_MOST: usize = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CursorFile {
    cursor: u64,
}

fn cursor_path(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("vogt_drift.json")
}

fn load_cursor(path: &std::path::Path) -> Option<u64> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<CursorFile>(&raw)
        .ok()
        .map(|c| c.cursor)
}

/// Write the cursor, and say so if it could not be written.
///
/// A failure here is worth a warning every time rather than on transition:
/// the consequence is that the next restart replays, which is the one outcome
/// this module exists to prevent.
fn store_cursor(path: &std::path::Path, cursor: u64) {
    match serde_json::to_vec(&CursorFile { cursor }) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(path, bytes) {
                warn!(error = %e, path = %path.display(),
                      "could not persist the drift cursor; a restart will replay from the last one written");
            }
        }
        Err(e) => warn!(error = %e, "could not serialize the drift cursor"),
    }
}

/// Build the notification for one poll's worth of drift.
///
/// Returns `None` when the page held no drift, which is the overwhelmingly
/// common case and must cost nothing.
fn notification_for(drift: &[serde_json::Value]) -> Option<(String, String, serde_json::Value)> {
    let (first, rest) = drift.split_first()?;

    let summary_of = |event: &serde_json::Value| -> Option<String> {
        event
            .get("summary")?
            .get("summary")?
            .as_str()
            .map(|s| s.to_string())
    };

    let title = if rest.is_empty() {
        "New drift".to_string()
    } else {
        format!("{} new drift proposals", drift.len())
    };

    let body = if drift.len() <= NAME_AT_MOST {
        summary_of(first)
            // The core always sends a summary, but a body that read as an
            // empty string if it ever stopped would be a notification that
            // says nothing at all.
            .unwrap_or_else(|| "The estate disagrees with what was declared about it.".to_string())
    } else {
        match summary_of(first) {
            Some(s) => format!("{s} — and {} more.", drift.len() - 1),
            None => "The estate disagrees with what was declared about it.".to_string(),
        }
    };

    let data = json!({
        "kind": "drift",
        "count": drift.len(),
        // The drift inbox lives on the projects surface, which is where an
        // accept or reject is actually possible — a notification that opens
        // somewhere you cannot act is a notification that wasted the tap.
        "url": "/#/projects",
    });
    Some((title, body, data))
}

/// Walk to the end of the feed without notifying anyone, and return where the
/// end was. Used only on a first run; see the module comment.
async fn seed_to_tip(core: &crate::vogt_core::VogtCore) -> std::result::Result<u64, String> {
    let mut cursor = 0u64;
    loop {
        let (events, next) = core.events_after(cursor, PAGE).await?;
        let drained = events.len();
        // `next_cursor` never rewinds, so `next == cursor` means the core has
        // nothing further — and it is also the guard that stops this loop if
        // the core ever answers something unexpected.
        if next <= cursor {
            return Ok(cursor);
        }
        cursor = next;
        if drained < PAGE as usize {
            return Ok(cursor);
        }
    }
}

pub fn spawn_drift_watcher(state: Arc<AppState>) {
    let Some(core) = state.vogt_core.clone() else {
        debug!("no vogt-core configured, so no drift watcher");
        return;
    };
    let path = cursor_path(&state.config.state_dir);

    tokio::spawn(async move {
        // Nothing happens for the first interval, and that is deliberate
        // rather than incidental. In the merged container both halves start
        // together and the core runs `vogt init` — a migration on a real
        // store — before it serves, so a request sent the moment the engine
        // binds is a request aimed at a process that is not listening yet.
        // Waiting also keeps this watcher out of the way of anything reading
        // the core at boot, which is what the front door's own tests assert.
        tokio::time::sleep(POLL_INTERVAL).await;

        // Seeding happens once, and it may still have to wait past the first
        // interval; the failures are expected and must not be loud.
        let mut cursor = match load_cursor(&path) {
            Some(stored) => {
                info!(cursor = stored, "drift watcher resumed");
                stored
            }
            None => loop {
                match seed_to_tip(&core).await {
                    Ok(tip) => {
                        store_cursor(&path, tip);
                        info!(
                            cursor = tip,
                            "drift watcher seeded at the end of the event feed; \
                             drift raised before now will not notify"
                        );
                        break tip;
                    }
                    Err(reason) => {
                        debug!(%reason, "cannot seed the drift cursor yet; retrying");
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                }
            },
        };

        // Whether the last poll failed, so a persistent outage is logged once
        // going in and once coming out rather than once a minute throughout.
        let mut failing = false;

        loop {
            tokio::time::sleep(POLL_INTERVAL).await;

            let (events, next) = match core.events_after(cursor, PAGE).await {
                Ok(page) => page,
                Err(reason) => {
                    if !failing {
                        warn!(%reason, "drift watcher cannot read the event feed; \
                                        it will keep trying and will not move its cursor");
                        failing = true;
                    }
                    continue;
                }
            };
            if failing {
                info!("drift watcher is reading the event feed again");
                failing = false;
            }
            if next <= cursor {
                continue;
            }

            let drift: Vec<serde_json::Value> = events
                .into_iter()
                .filter(|event| event.get("kind").and_then(|k| k.as_str()) == Some(DRIFT_RAISED))
                .collect();

            // Before dispatching, never after — the module comment argues the
            // trade. Note this advances even when the page held no drift,
            // which is most of the time and is the whole point of a cursor.
            cursor = next;
            store_cursor(&path, cursor);

            let Some((title, body, data)) = notification_for(&drift) else {
                continue;
            };
            let counts = state
                .push
                .notify(NotificationKind::Drift, &title, &body, data)
                .await;
            info!(
                drift = drift.len(),
                ok = counts.ok,
                fail = counts.fail,
                queued = counts.queued,
                "drift push dispatched"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raised(summary: &str) -> serde_json::Value {
        json!({
            "seq": 1,
            "kind": DRIFT_RAISED,
            "entity_kind": "drift_proposal",
            "entity_id": "d-1",
            "summary": { "kind": "state_sync", "summary": summary },
        })
    }

    #[test]
    fn no_drift_is_no_notification() {
        assert!(notification_for(&[]).is_none());
    }

    #[test]
    fn one_drift_is_named() {
        let (title, body, data) =
            notification_for(&[raised("vogt#12 is closed upstream")]).expect("one drift notifies");
        assert_eq!(title, "New drift");
        assert_eq!(body, "vogt#12 is closed upstream");
        assert_eq!(data["count"], 1);
        assert_eq!(data["kind"], "drift");
    }

    /// The burst case, which is the one that decides whether this channel
    /// survives contact with a sweep that raises thirty proposals at once.
    #[test]
    fn a_burst_is_one_notification_that_counts() {
        let page: Vec<_> = (0..30).map(|i| raised(&format!("thing {i}"))).collect();
        let (title, body, data) = notification_for(&page).expect("a burst notifies");
        assert_eq!(title, "30 new drift proposals");
        assert!(body.contains("and 29 more"));
        assert_eq!(data["count"], 30);
    }

    /// A proposal whose summary the core did not send still says something.
    #[test]
    fn a_summaryless_event_still_has_a_body() {
        let bare = json!({ "kind": DRIFT_RAISED, "entity_id": "d-2" });
        let (_, body, _) = notification_for(&[bare]).expect("it still notifies");
        assert!(!body.is_empty());
    }

    #[test]
    fn a_cursor_survives_a_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = cursor_path(dir.path());
        assert_eq!(load_cursor(&path), None, "nothing stored is a first run");
        store_cursor(&path, 4_211);
        assert_eq!(load_cursor(&path), Some(4_211));
    }
}
