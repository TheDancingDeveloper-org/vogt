//! The human fallback ladder for escalations (WI-996 E3).
//!
//! An escalation a live session holds open (a blocked report, or a permission
//! dialog) climbs a ladder while nobody resolves it:
//!
//! 1. `needs: overseer` is promoted to the person after `promote_after`
//!    (10 min): a push under the `waiting_for_input` preference.
//! 2. A person-facing escalation still open after `page_after` (15 min) is
//!    posted to `ENGINE_ESCALATION_WEBHOOK_URL`, then again every
//!    `remind_every` (6 h), at most `max_reminders` (4) more times.
//! 3. When a paged escalation closes, a resolved post follows.
//!
//! The post is an Alertmanager v2 alert list (`POST /api/v2/alerts`), so the
//! estate points the URL straight at Alertmanager and its Telegram route and
//! silences apply. Each escalation is one alert, its labels keyed by the
//! escalation's content key, so a re-post updates rather than duplicates.
//!
//! Nothing the worker wrote leaves the box: not the blocked reason, not the
//! dialog's question or command, not the session's name. The alert and the
//! promotion push carry only the boundary, who it needs and the content key,
//! and the person reads the ask in the Inbox.
//!
//! "Unacknowledged" is "still open" here: the engine does not see the Inbox
//! archive. Clocks are the engine's own and start when the ladder first sees
//! an escalation, so an engine restart restarts them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tracing::{info, warn};
use uuid::Uuid;
use vogt_engine_contract::{ActivityState, ChangeEscalation, EscalationNeeds};

use crate::app::AppState;
use crate::changes::{approval_escalation, blocked_escalation};
use crate::push::NotificationKind;

/// The ladder's timings. Defaults are the operator's (2026-10-07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LadderConfig {
    pub promote_after: Duration,
    pub page_after: Duration,
    pub remind_every: Duration,
    pub max_reminders: u32,
}

impl Default for LadderConfig {
    fn default() -> Self {
        Self {
            promote_after: Duration::from_secs(10 * 60),
            page_after: Duration::from_secs(15 * 60),
            remind_every: Duration::from_secs(6 * 60 * 60),
            max_reminders: 4,
        }
    }
}

impl LadderConfig {
    /// Defaults, overridden by `ENGINE_ESCALATION_PROMOTE_AFTER_SECS`,
    /// `ENGINE_ESCALATION_PAGE_AFTER_SECS`, `ENGINE_ESCALATION_REMIND_EVERY_SECS`
    /// and `ENGINE_ESCALATION_MAX_REMINDERS`. An unparsable value keeps the
    /// default and is logged.
    pub fn from_env() -> Self {
        fn secs(name: &str, default: Duration) -> Duration {
            read(name).map(Duration::from_secs).unwrap_or(default)
        }
        fn read(name: &str) -> Option<u64> {
            let raw = std::env::var(name).ok()?;
            match raw.trim().parse() {
                Ok(v) => Some(v),
                Err(_) => {
                    warn!(
                        var = name,
                        "ignoring an unparsable escalation ladder setting"
                    );
                    None
                }
            }
        }
        let d = Self::default();
        Self {
            promote_after: secs("ENGINE_ESCALATION_PROMOTE_AFTER_SECS", d.promote_after),
            page_after: secs("ENGINE_ESCALATION_PAGE_AFTER_SECS", d.page_after),
            remind_every: secs("ENGINE_ESCALATION_REMIND_EVERY_SECS", d.remind_every),
            max_reminders: read("ENGINE_ESCALATION_MAX_REMINDERS")
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(d.max_reminders),
        }
    }
}

/// An escalation a live session holds open right now.
#[derive(Debug, Clone)]
pub struct OpenEscalation {
    pub session_id: Uuid,
    pub session_name: String,
    pub escalation: ChangeEscalation,
}

/// What the ladder asks its runner to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Push to the person: an overseer did not resolve it in time.
    Promote(Rung),
    /// Post a firing alert. `reminder` is 0 for the first page.
    Page { rung: Rung, reminder: u32 },
    /// Post a resolved alert for an escalation that was paged.
    Resolve(Rung),
}

/// The escalation a step is about, as the ladder last saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rung {
    pub key: String,
    pub session_id: Uuid,
    pub session_name: String,
    pub needs: EscalationNeeds,
    pub boundary: String,
    pub promoted: bool,
    /// Wall-clock time the ladder first saw it (RFC 3339).
    pub started_at: String,
}

#[derive(Debug)]
struct Tracked {
    rung: Rung,
    first_seen: Instant,
    /// When it became the person's: at once unless `needs: overseer`.
    person_since: Option<Instant>,
    pages: u32,
    last_page: Option<Instant>,
}

/// The ladder's state: every open escalation it has seen, by content key.
#[derive(Debug, Default)]
pub struct Ladder {
    config: LadderConfig,
    open: HashMap<String, Tracked>,
}

impl Ladder {
    pub fn new(config: LadderConfig) -> Self {
        Self {
            config,
            open: HashMap::new(),
        }
    }

    /// Advance to `now` given the escalations open right now, and return what
    /// to do. `wall_now` stamps a newly seen escalation's start.
    pub fn step(
        &mut self,
        now: Instant,
        wall_now: &str,
        current: Vec<OpenEscalation>,
    ) -> Vec<Step> {
        let mut steps = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for open in current {
            let esc = open.escalation;
            if esc.key.is_empty() {
                continue;
            }
            seen.insert(esc.key.clone());
            let tracked = self.open.entry(esc.key.clone()).or_insert_with(|| Tracked {
                rung: Rung {
                    key: esc.key.clone(),
                    session_id: open.session_id,
                    session_name: open.session_name.clone(),
                    needs: esc.needs,
                    boundary: boundary_name(&esc),
                    promoted: false,
                    started_at: wall_now.to_string(),
                },
                first_seen: now,
                person_since: (esc.needs != EscalationNeeds::Overseer).then_some(now),
                pages: 0,
                last_page: None,
            });
            tracked.rung.session_name = open.session_name;
            if tracked.person_since.is_none()
                && now.duration_since(tracked.first_seen) >= self.config.promote_after
            {
                tracked.person_since = Some(now);
                tracked.rung.promoted = true;
                steps.push(Step::Promote(tracked.rung.clone()));
            }
            let Some(person_since) = tracked.person_since else {
                continue;
            };
            let due = match tracked.last_page {
                None => now.duration_since(person_since) >= self.config.page_after,
                Some(last) => {
                    tracked.pages <= self.config.max_reminders
                        && now.duration_since(last) >= self.config.remind_every
                }
            };
            if due {
                steps.push(Step::Page {
                    rung: tracked.rung.clone(),
                    reminder: tracked.pages,
                });
                tracked.pages += 1;
                tracked.last_page = Some(now);
            }
        }
        let closed: Vec<String> = self
            .open
            .keys()
            .filter(|k| !seen.contains(*k))
            .cloned()
            .collect();
        for key in closed {
            match self.open.remove(&key) {
                Some(tracked) if tracked.pages > 0 => steps.push(Step::Resolve(tracked.rung)),
                _ => {}
            }
        }
        steps
    }
}

fn boundary_name(esc: &ChangeEscalation) -> String {
    serde_json::to_value(esc.boundary)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "other".to_string())
}

fn needs_name(needs: EscalationNeeds) -> &'static str {
    match needs {
        EscalationNeeds::Person => "person",
        EscalationNeeds::Overseer => "overseer",
        EscalationNeeds::Either => "either",
    }
}

/// The Alertmanager v2 body for one escalation. `ends_at` is set on a
/// resolved post. Only the engine's own fields: boundary, needs, the content
/// key and the session's id. No worker text (ask, question, session name),
/// since the post goes on to Telegram unredacted.
pub fn alert_body(rung: &Rung, reminder: u32, ends_at: Option<&str>) -> serde_json::Value {
    let mut alert = json!({
        "labels": {
            "alertname": "VogtEscalation",
            "source": "vogt-engine",
            "session_id": rung.session_id.to_string(),
            "boundary": rung.boundary,
            "needs": needs_name(rung.needs),
            "key": rung.key,
        },
        "annotations": {
            "summary": format!(
                "A Vogt session is blocked on a {} and needs the {}; open the Inbox",
                rung.boundary.replace('_', " "),
                needs_name(rung.needs),
            ),
            "promoted": rung.promoted.to_string(),
            "reminder": reminder.to_string(),
        },
        "startsAt": rung.started_at,
    });
    if let Some(ends) = ends_at {
        alert["endsAt"] = json!(ends);
    }
    json!([alert])
}

/// Write the audit line for a ladder action: `promote`, `page`, `reminder`,
/// `resolve` or `webhook_failed`. Ids and the content key only, never the
/// ask or any other worker text.
pub fn audit(action: &str, rung: &Rung, reminder: u32, detail: &str) {
    tracing::info!(
        target: "vogt::audit",
        event = "escalation.ladder",
        action,
        session_id = %rung.session_id,
        key = %rung.key,
        boundary = %rung.boundary,
        needs = needs_name(rung.needs),
        promoted = rung.promoted,
        reminder,
        detail,
        "escalation ladder"
    );
}

/// The audit action for a step.
fn step_action(step: &Step) -> (&'static str, &Rung, u32) {
    match step {
        Step::Promote(rung) => ("promote", rung, 0),
        Step::Page { rung, reminder: 0 } => ("page", rung, 0),
        Step::Page { rung, reminder } => ("reminder", rung, *reminder),
        Step::Resolve(rung) => ("resolve", rung, 0),
    }
}

/// Every escalation the live sessions hold open now.
fn open_escalations(state: &AppState) -> Vec<OpenEscalation> {
    let mut out = Vec::new();
    for session in state.sessions.live_sessions() {
        if session.exit_code().is_some() {
            continue;
        }
        let name = session.name();
        if let Some(report) = session.blocked() {
            out.push(OpenEscalation {
                session_id: session.id,
                session_name: name.clone(),
                escalation: blocked_escalation(&report),
            });
        }
        if session.activity() == ActivityState::AwaitingApproval {
            let approval = session.approval_prompt();
            out.push(OpenEscalation {
                session_id: session.id,
                session_name: name,
                escalation: approval_escalation(session.id, approval.as_ref()),
            });
        }
    }
    out
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The person's push when an overseer escalation is promoted. A push leaves
/// the box and the session's name is worker-set, so the title is fixed; the
/// ask stays in the Inbox.
fn promotion_push(rung: &Rung) -> (String, String, serde_json::Value) {
    let title = "A Vogt session is blocked on you".to_string();
    let body = format!(
        "Its overseer did not resolve a {} in time; open the Inbox.",
        rung.boundary.replace('_', " ")
    );
    let data = json!({
        "kind": "escalation-promoted",
        "session_id": rung.session_id.to_string(),
        "key": rung.key,
        "needs": "person",
        "url": format!("/#/t/{}", rung.session_id),
    });
    (title, body, data)
}

/// Spawn the ladder. It polls every 30 s; without a webhook URL it still
/// promotes overseer escalations to the person's push.
pub fn spawn_escalation_ladder(state: Arc<AppState>) {
    let config = LadderConfig::from_env();
    let webhook = std::env::var("ENGINE_ESCALATION_WEBHOOK_URL")
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default();
    tokio::spawn(async move {
        let mut ladder = Ladder::new(config);
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let wall_now = rfc3339_now();
            let steps = ladder.step(Instant::now(), &wall_now, open_escalations(&state));
            for step in steps {
                let (action, rung, reminder) = step_action(&step);
                audit(action, rung, reminder, "");
                let (body, what) = match &step {
                    Step::Promote(rung) => {
                        let (title, body, data) = promotion_push(rung);
                        let counts = state
                            .push
                            .notify(NotificationKind::WaitingForInput, &title, &body, data)
                            .await;
                        info!(session = %rung.session_id, ok = counts.ok, fail = counts.fail, "escalation promoted to the person");
                        continue;
                    }
                    Step::Page { rung, reminder } => (alert_body(rung, *reminder, None), "page"),
                    Step::Resolve(rung) => (alert_body(rung, 0, Some(&wall_now)), "resolved"),
                };
                let Some(url) = webhook.as_deref() else {
                    continue;
                };
                match client.post(url).json(&body).send().await {
                    Ok(resp) if resp.status().is_success() => {
                        info!(step = what, "escalation webhook posted");
                    }
                    Ok(resp) => {
                        warn!(step = what, status = %resp.status(), "escalation webhook refused");
                        audit("webhook_failed", rung, reminder, resp.status().as_str());
                    }
                    Err(error) => {
                        let kind = if error.is_timeout() {
                            "timeout"
                        } else {
                            "send"
                        };
                        warn!(step = what, error = %error.without_url(), "escalation webhook failed");
                        audit("webhook_failed", rung, reminder, kind);
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use vogt_engine_contract::EscalationBoundary;

    const MIN: Duration = Duration::from_secs(60);

    fn open(key: &str, needs: EscalationNeeds) -> OpenEscalation {
        OpenEscalation {
            session_id: Uuid::nil(),
            session_name: "worker".into(),
            escalation: ChangeEscalation {
                needs,
                boundary: EscalationBoundary::DeniedAction,
                ask: "merge the PR".into(),
                key: key.into(),
                resolve: vec![],
            },
        }
    }

    fn kinds(steps: &[Step]) -> Vec<String> {
        steps
            .iter()
            .map(|s| match s {
                Step::Promote(_) => "promote".to_string(),
                Step::Page { reminder, .. } => format!("page{reminder}"),
                Step::Resolve(_) => "resolve".to_string(),
            })
            .collect()
    }

    #[test]
    fn a_person_escalation_pages_after_15_minutes_reminds_4_times_then_stops() {
        let mut ladder = Ladder::new(LadderConfig::default());
        let t0 = Instant::now();
        let cur = || vec![open("k", EscalationNeeds::Person)];
        assert!(ladder.step(t0, "t", cur()).is_empty());
        assert!(ladder.step(t0 + 14 * MIN, "t", cur()).is_empty());
        assert_eq!(kinds(&ladder.step(t0 + 15 * MIN, "t", cur())), ["page0"]);
        assert!(ladder.step(t0 + 16 * MIN, "t", cur()).is_empty());
        let mut got = Vec::new();
        for h in 1..=40 {
            got.extend(kinds(&ladder.step(
                t0 + 15 * MIN + h * 60 * MIN,
                "t",
                cur(),
            )));
        }
        assert_eq!(got, ["page1", "page2", "page3", "page4"]);
    }

    #[test]
    fn an_overseer_escalation_is_promoted_after_10_minutes_then_paged_15_later() {
        let mut ladder = Ladder::new(LadderConfig::default());
        let t0 = Instant::now();
        let cur = || vec![open("k", EscalationNeeds::Overseer)];
        assert!(ladder.step(t0, "t", cur()).is_empty());
        assert!(ladder.step(t0 + 9 * MIN, "t", cur()).is_empty());
        let steps = ladder.step(t0 + 10 * MIN, "t", cur());
        assert_eq!(kinds(&steps), ["promote"]);
        assert!(ladder.step(t0 + 24 * MIN, "t", cur()).is_empty());
        let steps = ladder.step(t0 + 25 * MIN, "t", cur());
        assert_eq!(kinds(&steps), ["page0"]);
        let Step::Page { rung, .. } = &steps[0] else {
            unreachable!()
        };
        assert!(rung.promoted);
    }

    #[test]
    fn a_paged_escalation_resolves_and_an_unpaged_one_closes_quietly() {
        let mut ladder = Ladder::new(LadderConfig::default());
        let t0 = Instant::now();
        ladder.step(
            t0,
            "t",
            vec![
                open("a", EscalationNeeds::Person),
                open("b", EscalationNeeds::Person),
            ],
        );
        let _ = ladder.step(t0 + 15 * MIN, "t", vec![open("a", EscalationNeeds::Person)]);
        // b closed before its page: nothing. a was paged on this step.
        let steps = ladder.step(t0 + 16 * MIN, "t", vec![]);
        assert_eq!(kinds(&steps), ["resolve"]);
        let Step::Resolve(rung) = &steps[0] else {
            unreachable!()
        };
        assert_eq!(rung.key, "a");
        // The same ask raised again starts a fresh ladder.
        assert!(ladder
            .step(t0 + 17 * MIN, "t", vec![open("a", EscalationNeeds::Person)])
            .is_empty());
    }

    #[test]
    fn the_alert_body_is_an_alertmanager_v2_list_with_the_content_key() {
        let rung = Rung {
            key: "s:denied_action:abc".into(),
            session_id: Uuid::nil(),
            session_name: "worker".into(),
            needs: EscalationNeeds::Person,
            boundary: "denied_action".into(),
            promoted: false,
            started_at: "2026-10-31T00:00:00Z".into(),
        };
        let firing = alert_body(&rung, 2, None);
        assert_eq!(firing[0]["labels"]["alertname"], "VogtEscalation");
        assert_eq!(firing[0]["labels"]["key"], "s:denied_action:abc");
        assert_eq!(firing[0]["annotations"]["reminder"], "2");
        assert!(firing[0].get("endsAt").is_none());
        let resolved = alert_body(&rung, 0, Some("2026-10-31T01:00:00Z"));
        assert_eq!(resolved[0]["endsAt"], "2026-10-31T01:00:00Z");
    }

    #[test]
    fn no_worker_text_reaches_the_webhook() {
        // A blocked reason carrying a token-shaped string climbs the whole
        // ladder; neither the alert nor the session's name carries it.
        let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
        let mut esc = open("s:credential:abc", EscalationNeeds::Overseer);
        esc.escalation.ask = format!("need a token, tried {token}");
        esc.session_name = format!("worker {token}");
        let mut ladder = Ladder::new(LadderConfig::default());
        let t0 = Instant::now();
        let mut steps = Vec::new();
        for m in [0, 10, 25] {
            steps.extend(ladder.step(t0 + m * MIN, "t", vec![esc.clone()]));
        }
        steps.extend(ladder.step(t0 + 26 * MIN, "t", vec![]));
        assert_eq!(kinds(&steps), ["promote", "page0", "resolve"]);
        for step in &steps {
            let (Step::Promote(rung) | Step::Page { rung, .. } | Step::Resolve(rung)) = step;
            let body = alert_body(rung, 0, Some("t")).to_string();
            assert!(!body.contains(token), "{body}");
            assert!(!body.contains("need a token"), "{body}");
            assert!(body.contains("s:credential:abc"));
            assert!(body.contains("\"needs\":\"overseer\""));
            if let Step::Promote(rung) = step {
                let (title, body, data) = promotion_push(rung);
                for text in [title, body, data.to_string()] {
                    assert!(!text.contains(token), "{text}");
                    assert!(!text.contains("worker"), "{text}");
                    assert!(!text.contains("need a token"), "{text}");
                }
            }
        }
    }

    /// Captures what the subscriber writes, for the audit test.
    #[derive(Clone, Default)]
    struct Captured(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn every_ladder_action_is_audited_without_the_ask() {
        let token = "ghp_0123456789abcdefABCDEF0123456789abcd";
        let mut esc = open("s:credential:abc", EscalationNeeds::Overseer);
        esc.escalation.ask = format!("need a token, tried {token}");
        let config = LadderConfig {
            remind_every: MIN,
            ..LadderConfig::default()
        };
        let mut ladder = Ladder::new(config);
        let t0 = Instant::now();
        let mut steps = Vec::new();
        for m in [0, 10, 25, 26] {
            steps.extend(ladder.step(t0 + m * MIN, "t", vec![esc.clone()]));
        }
        steps.extend(ladder.step(t0 + 27 * MIN, "t", vec![]));

        let out = Captured::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            for step in &steps {
                let (action, rung, reminder) = step_action(step);
                audit(action, rung, reminder, "");
            }
            if let Step::Page { rung, .. } = &steps[1] {
                audit("webhook_failed", rung, 0, "503 Service Unavailable");
            }
        });
        let text = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let actions: Vec<&str> = lines
            .iter()
            .map(|l| {
                assert!(l.contains("vogt::audit"), "{l}");
                assert!(l.contains("event=\"escalation.ladder\""), "{l}");
                assert!(l.contains("key=s:credential:abc"), "{l}");
                assert!(!l.contains(token) && !l.contains("need a token"), "{l}");
                l.split("action=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap()
            })
            .collect();
        assert_eq!(
            actions,
            ["promote", "page", "reminder", "resolve", "webhook_failed"]
        );
    }
}
