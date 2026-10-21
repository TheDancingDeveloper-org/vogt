//! Capability grants: a person-approved posture for a live session (WI-979,
//! milestone 2 of `docs/design/oversight-grants.md`).
//!
//! Claude Code cannot have its permission mode raised from outside the
//! process, so the engine applies a capability grant (`bypass`,
//! `accept-edits`) by hibernating the session and waking it with the granted
//! `permission_mode`. The conversation is resumed and the session keeps its
//! id; only its own process restarts. The record keeps the base posture, so
//! when the grant expires or is revoked the same restart in reverse returns
//! the session to it — and an engine restart, which forgets every grant,
//! wakes it at its base posture too (fails closed).
//!
//! A session in the middle of a turn is never restarted: the grant waits
//! (`applied: false`) until the session is at its prompt, and so does the
//! revert. The watcher here looks every [`TICK`]; an apply or revoke also
//! looks at once.
//!
//! Credential grants are dropped when a session hibernates (WI-973 keeps them
//! in memory only, for a running process). A restart made *for* a capability
//! grant is not the session ending, so the credential grants it held are put
//! back once it is awake.

use std::{sync::Arc, time::Duration};

use dashmap::DashMap;
use uuid::Uuid;
use vogt_engine_contract::{HibernateTrigger, SessionGrant, WakeRequest};

use crate::{app::AppState, secret_broker::GrantRefusal, sessions::SessionRegistry};

/// The postures a person may grant, as the wire names them.
pub const CAPABILITIES: &[&str] = &["bypass", "accept-edits"];

/// How often the watcher looks for a grant to apply, expire or revert.
pub const TICK: Duration = Duration::from_secs(15);

/// A capability grant as the engine holds it.
#[derive(Clone, Debug)]
pub struct HeldCapability {
    pub grant: SessionGrant,
    pub expires_at: time::OffsetDateTime,
}

impl HeldCapability {
    /// The granted posture, normalised (`accept_edits` → `accept-edits`).
    pub fn posture(&self) -> String {
        normalise(self.grant.capability.as_deref().unwrap_or_default())
    }
}

#[derive(Clone, Debug)]
enum Entry {
    /// Granted: the session should run in this posture.
    Active(HeldCapability),
    /// Ended (expired or revoked): the session should go back to its base
    /// posture, and the entry goes once it has.
    Reverting,
}

/// Every session's capability grant: at most one each.
#[derive(Debug, Default)]
pub struct CapabilityGrants {
    held: DashMap<Uuid, Entry>,
    /// Held across one look-and-restart, so two never interleave — the
    /// watcher and an apply's immediate look included.
    cycle: tokio::sync::Mutex<()>,
}

fn normalise(capability: &str) -> String {
    capability.trim().replace('_', "-")
}

impl CapabilityGrants {
    /// Hold a person-approved capability grant for `session`. The same
    /// `grant_id` sent again replaces the earlier copy (a retried approval);
    /// a different live grant for the session is refused, so one approval
    /// never silently ends another. `agent` is the session's agent CLI and
    /// `base` its recorded posture, for the checks a restart would otherwise
    /// fail on.
    pub fn apply(
        &self,
        session: Uuid,
        mut grant: SessionGrant,
        agent: Option<&str>,
        base: Option<&str>,
        now: time::OffsetDateTime,
    ) -> Result<HeldCapability, GrantRefusal> {
        let expires_at = crate::secret_broker::grant_expiry(&grant, now)?;
        let capability = normalise(grant.capability.as_deref().unwrap_or_default());
        if !CAPABILITIES.contains(&capability.as_str()) {
            return Err(GrantRefusal::Invalid(format!(
                "capability {capability:?} is not one of {}",
                CAPABILITIES.join(", ")
            )));
        }
        if agent == Some("klaudia") && capability == "accept-edits" {
            return Err(GrantRefusal::Invalid(
                "klaudia has no accept-edits posture; grant bypass instead".into(),
            ));
        }
        if base == Some(capability.as_str()) {
            return Err(GrantRefusal::Invalid(format!(
                "the session already runs in {capability}; there is nothing to grant"
            )));
        }
        if let Some(Entry::Active(held)) = self.held.get(&session).map(|e| e.clone()) {
            if held.grant.grant_id != grant.grant_id && held.expires_at > now {
                return Err(GrantRefusal::VarInUse(format!(
                    "capability (grant {} for {})",
                    held.grant.grant_id,
                    held.posture()
                )));
            }
        }
        grant.capability = Some(capability);
        grant.var.clear();
        grant.project_id.clear();
        grant.secret_name.clear();
        grant.applied = None;
        let held = HeldCapability { grant, expires_at };
        self.held.insert(session, Entry::Active(held.clone()));
        Ok(held)
    }

    /// End one grant: the session goes back to its base posture once it is
    /// idle. Whether the session held it.
    pub fn remove(&self, session: Uuid, grant_id: &str) -> bool {
        let Some(mut entry) = self.held.get_mut(&session) else {
            return false;
        };
        match &*entry {
            Entry::Active(held) if held.grant.grant_id == grant_id => {
                *entry = Entry::Reverting;
                true
            }
            _ => false,
        }
    }

    /// Forget a session's grant without reverting anything: it was removed.
    pub fn forget(&self, session: Uuid) {
        self.held.remove(&session);
    }

    /// The live grant for `session`, `None` once it has expired.
    pub fn get(&self, session: Uuid, now: time::OffsetDateTime) -> Option<HeldCapability> {
        match self.held.get(&session).map(|e| e.clone()) {
            Some(Entry::Active(held)) if held.expires_at > now => Some(held),
            _ => None,
        }
    }

    /// The posture a live grant gives `session`, which a wake starts it in.
    pub fn posture(&self, session: Uuid, now: time::OffsetDateTime) -> Option<String> {
        self.get(session, now).map(|h| h.posture())
    }

    /// Every session with a grant to apply, expire or revert.
    pub fn sessions(&self) -> Vec<Uuid> {
        self.held.iter().map(|e| *e.key()).collect()
    }

    /// Turn an expired grant into a revert; the grant that expired, if any.
    fn expire(&self, session: Uuid, now: time::OffsetDateTime) -> Option<HeldCapability> {
        let mut entry = self.held.get_mut(&session)?;
        match &*entry {
            Entry::Active(held) if held.expires_at <= now => {
                let held = held.clone();
                *entry = Entry::Reverting;
                Some(held)
            }
            _ => None,
        }
    }

    /// Drop a finished revert.
    fn settle(&self, session: Uuid) {
        self.held
            .remove_if(&session, |_, e| matches!(e, Entry::Reverting));
    }
}

/// The wire view of a held grant, saying whether the session runs in it now.
pub fn view(registry: &SessionRegistry, id: Uuid, held: &HeldCapability) -> SessionGrant {
    let mut grant = held.grant.clone();
    let current = registry.get(id).ok().and_then(|s| s.permission_mode());
    grant.applied = Some(current.as_deref() == Some(held.posture().as_str()));
    grant
}

/// Look at every session with a grant, every [`TICK`].
pub fn spawn_watcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            for id in state.sessions.capabilities().sessions() {
                reconcile(&state, id).await;
            }
        }
    });
}

/// Look at one session now, off the caller's request, and again each second
/// for a minute while it is mid-turn; the watcher takes over after that.
pub fn nudge(state: &Arc<AppState>, id: Uuid) {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        for _ in 0..60 {
            let step = reconcile(&state, id).await;
            if !matches!(step, Step::Waiting(_)) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}

/// What one look decided for a session.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    /// Already in the posture it should be in.
    Settled,
    /// It should change posture, but is busy; the next look tries again.
    Waiting(String),
    /// Hibernated, exited or gone; nothing to restart.
    NotRunning,
    /// Restarted into `posture` (`None`: the default one).
    Restarted(Option<String>),
    /// The restart failed; said in the log.
    Failed(String),
}

/// Bring one session to the posture its grant (or its record) says: expire
/// a grant past its time, and restart the session when it is at its prompt
/// and runs in the wrong posture.
pub async fn reconcile(state: &Arc<AppState>, id: Uuid) -> Step {
    let registry = &state.sessions;
    let grants = registry.capabilities();
    let _cycle = grants.cycle.lock().await;
    let now = time::OffsetDateTime::now_utc();
    if let Some(held) = grants.expire(id, now) {
        audit(id, &held.grant.grant_id, "expired", &held.posture());
    }
    let session = match registry.get(id) {
        Ok(session) => session,
        // Hibernated: it wakes into the right posture by itself.
        Err(_) if registry.is_hibernated(id) => return Step::NotRunning,
        Err(_) => {
            grants.forget(id);
            return Step::NotRunning;
        }
    };
    if !session.is_alive() {
        if !session.is_hibernating() {
            // It ended: the grant goes with it.
            grants.forget(id);
        }
        return Step::NotRunning;
    }
    let wanted = grants
        .posture(id, now)
        .or_else(|| registry.base_posture(id));
    if session.permission_mode() == wanted {
        grants.settle(id);
        return Step::Settled;
    }
    if let Some(why) = registry.hibernate_refusal(&session, false) {
        // Applying was refused for the same reason; reverting cannot happen
        // either, and there is nothing granted to keep.
        grants.forget(id);
        return Step::Failed(why);
    }
    if let Some(why) = crate::hibernate_policy::busy(&session) {
        tracing::debug!(session = %id, reason = %why, "capability grant waits for the session to be idle");
        return Step::Waiting(why);
    }
    drop(session);
    restart(state, id, wanted).await
}

async fn restart(state: &Arc<AppState>, id: Uuid, wanted: Option<String>) -> Step {
    let registry = &state.sessions;
    let label = wanted.clone().unwrap_or_else(|| "default".into());
    let now = time::OffsetDateTime::now_utc();
    // Hibernating drops the credential grants; this restart is not the
    // session ending, so they are put back once it is awake.
    let kept = registry.secret_broker().grants_for(id, now);
    let why = format!("restarting into the {label} posture for a capability grant");
    if let Err(e) = registry
        .hibernate(id, Some(why), HibernateTrigger::Manual, false, None)
        .await
    {
        tracing::warn!(session = %id, error = %e, "could not hibernate a session for its capability grant");
        return Step::Failed(e.to_string());
    }
    let woken = match wake(state, id).await {
        Ok(()) => Ok(()),
        Err(e) if wanted != registry.base_posture(id) => {
            // The granted posture would not start. Leave the session where a
            // person left it rather than hibernated: end the grant and wake
            // it as it was.
            tracing::warn!(session = %id, error = %e, posture = %label, "could not wake a session into its granted posture; waking it at its base posture");
            if let Some(held) = registry.capabilities().get(id, now) {
                registry.capabilities().remove(id, &held.grant.grant_id);
                audit(id, &held.grant.grant_id, "failed", &label);
            }
            wake(state, id)
                .await
                .map_err(|again| format!("{e}; then {again}"))
        }
        Err(e) => Err(e),
    };
    if let Err(e) = woken {
        tracing::warn!(session = %id, error = %e, "could not wake a session restarted for a capability grant; it stays hibernated");
        return Step::Failed(e);
    }
    let now = time::OffsetDateTime::now_utc();
    for held in kept {
        if let Err(e) = registry
            .secret_broker()
            .apply_grant(id, held.grant.clone(), now)
        {
            tracing::warn!(session = %id, grant_id = %held.grant.grant_id, error = %e, "a credential grant could not be put back after a capability restart");
        }
    }
    let applied = registry.get(id).ok().and_then(|s| s.permission_mode());
    match registry.capabilities().get(id, now) {
        Some(held) => audit(id, &held.grant.grant_id, "applied", &label),
        None => {
            registry.capabilities().settle(id);
            tracing::info!(
                target: "vogt::audit",
                event = "session.grant",
                action = "reverted",
                session_id = %id,
                posture = %label,
                "session restarted into its base posture"
            );
        }
    }
    Step::Restarted(applied)
}

/// Wake through vogt-core when there is one, so a linked session gets a
/// fresh token (the engine never stores one); directly otherwise.
async fn wake(state: &Arc<AppState>, id: Uuid) -> Result<(), String> {
    match state.vogt_core.as_ref() {
        Some(core) => {
            core.post_json(
                "/sessions/wake",
                &serde_json::json!({
                    "id": id.to_string(),
                    "reason": "restarted for a capability grant",
                    "force": true,
                }),
            )
            .await
        }
        None => state
            .sessions
            .wake(
                id,
                WakeRequest {
                    force: true,
                    ..WakeRequest::default()
                },
            )
            .await
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

fn audit(id: Uuid, grant_id: &str, action: &str, posture: &str) {
    tracing::info!(
        target: "vogt::audit",
        event = "session.grant",
        action,
        session_id = %id,
        grant_id,
        capability = posture,
        "capability grant {action}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(id: &str, capability: &str, expires_at: time::OffsetDateTime) -> SessionGrant {
        serde_json::from_value(serde_json::json!({
            "grant_id": id,
            "capability": capability,
            "uses": "ttl",
            "expires_at": expires_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            "reason": "needs to run the e2e suite unattended",
        }))
        .unwrap()
    }

    fn now() -> time::OffsetDateTime {
        time::OffsetDateTime::now_utc()
    }

    #[test]
    fn a_grant_gives_its_posture_until_revoked() {
        let grants = CapabilityGrants::default();
        let id = Uuid::new_v4();
        let later = now() + time::Duration::hours(1);
        let held = grants
            .apply(
                id,
                grant("grt_1", "bypass", later),
                Some("claude"),
                None,
                now(),
            )
            .unwrap();
        assert_eq!(held.posture(), "bypass");
        assert_eq!(grants.posture(id, now()).as_deref(), Some("bypass"));
        assert!(grants.remove(id, "grt_1"));
        assert_eq!(grants.posture(id, now()), None);
        // The revert is still owed until a look settles it.
        assert_eq!(grants.sessions(), vec![id]);
        grants.settle(id);
        assert!(grants.sessions().is_empty());
    }

    #[test]
    fn an_expired_grant_gives_nothing_and_turns_into_a_revert() {
        let grants = CapabilityGrants::default();
        let id = Uuid::new_v4();
        let soon = now() + time::Duration::seconds(30);
        grants
            .apply(
                id,
                grant("grt_1", "accept_edits", soon),
                Some("claude"),
                None,
                now(),
            )
            .unwrap();
        let after = soon + time::Duration::seconds(1);
        assert_eq!(grants.posture(id, after), None);
        let expired = grants.expire(id, after).unwrap();
        assert_eq!(expired.posture(), "accept-edits");
        assert!(!grants.remove(id, "grt_1"), "already ended");
        assert_eq!(grants.sessions(), vec![id]);
    }

    #[test]
    fn refuses_what_a_restart_could_not_honour() {
        let grants = CapabilityGrants::default();
        let id = Uuid::new_v4();
        let later = now() + time::Duration::hours(1);
        let refused = |g, agent, base| grants.apply(id, g, agent, base, now()).unwrap_err();
        assert!(matches!(
            refused(grant("grt_1", "plan", later), Some("claude"), None),
            GrantRefusal::Invalid(_)
        ));
        assert!(matches!(
            refused(grant("grt_1", "accept-edits", later), Some("klaudia"), None),
            GrantRefusal::Invalid(_)
        ));
        assert!(matches!(
            refused(
                grant("grt_1", "bypass", later),
                Some("claude"),
                Some("bypass")
            ),
            GrantRefusal::Invalid(_)
        ));
        assert!(matches!(
            refused(
                grant("grt_1", "bypass", now() - time::Duration::seconds(1)),
                Some("claude"),
                None
            ),
            GrantRefusal::Invalid(_)
        ));
        assert!(matches!(
            refused(
                grant("grt_1", "bypass", now() + time::Duration::hours(25)),
                Some("claude"),
                None
            ),
            GrantRefusal::Invalid(_)
        ));
        assert!(grants.sessions().is_empty());
    }

    #[test]
    fn a_second_grant_does_not_end_the_first_but_a_retry_replaces_it() {
        let grants = CapabilityGrants::default();
        let id = Uuid::new_v4();
        let later = now() + time::Duration::hours(1);
        grants
            .apply(
                id,
                grant("grt_1", "accept-edits", later),
                Some("claude"),
                None,
                now(),
            )
            .unwrap();
        assert!(matches!(
            grants
                .apply(
                    id,
                    grant("grt_2", "bypass", later),
                    Some("claude"),
                    None,
                    now()
                )
                .unwrap_err(),
            GrantRefusal::VarInUse(_)
        ));
        grants
            .apply(
                id,
                grant("grt_1", "bypass", later),
                Some("claude"),
                None,
                now(),
            )
            .unwrap();
        assert_eq!(grants.posture(id, now()).as_deref(), Some("bypass"));
        // Once the first ended, another may be granted.
        grants.remove(id, "grt_1");
        grants
            .apply(
                id,
                grant("grt_2", "bypass", later),
                Some("claude"),
                None,
                now(),
            )
            .unwrap();
        assert_eq!(grants.posture(id, now()).as_deref(), Some("bypass"));
    }

    #[test]
    fn the_held_copy_names_no_credential() {
        let grants = CapabilityGrants::default();
        let id = Uuid::new_v4();
        let mut g = grant("grt_1", "bypass", now() + time::Duration::hours(1));
        g.var = "SOMETHING".into();
        g.secret_name = "a-secret".into();
        let held = grants.apply(id, g, Some("claude"), None, now()).unwrap();
        assert!(held.grant.var.is_empty() && held.grant.secret_name.is_empty());
    }
}
