//! Person-approved grants to a live session. Ports
//! `src/vogt/application/services/grants.py`.
//!
//! Three rules shape this module:
//!
//! - Only a person decides. `decide` refuses every agent principal before
//!   anything else.
//! - The engine applies before the row says approved. The engine and SQLite
//!   cannot share a transaction, so the approval is sent first and recorded
//!   only once the engine has said yes. If the record then fails, the grant is
//!   taken back — unless another person's approval of the same grant landed
//!   first, which must not be undone.
//! - No value ever comes here. The core records names.

use serde_json::{json, Value};

use crate::application::context::{write_of, AppContext, Built};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, GrantKind, GrantState, IdFactory, Moment, SessionGrant};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const GRANT_REQUESTED_EVENT: &str = "session.grant_requested";
const GRANT_DECIDED_EVENT: &str = "session.grant_decided";
const GRANT_REVOKED_EVENT: &str = "session.grant_revoked";

pub fn grant_request_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let reason = field(&params, "session.grant_request", "reason")?;
    let target = field(&params, "session.grant_request", "target")?;
    let kind = field(&params, "session.grant_request", "kind")?;
    let uses = field(&params, "session.grant_request", "uses")?;
    let ttl_seconds = params
        .get("ttl_seconds")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            VogtError::InvalidRequest("session.grant_request needs a ttl_seconds".to_string())
        })?;
    let secret_name = text(&params, "secret_name");
    let project_id = text(&params, "project_id");
    let var = text(&params, "var");
    crate::with_ctx!(ctx, |ctx| request(
        ctx,
        &Request {
            reason: &reason,
            target: &target,
            kind: &kind,
            uses: &uses,
            ttl_seconds,
            secret_name: secret_name.as_deref(),
            project_id: project_id.as_deref(),
            var: var.as_deref(),
        },
    ))
}

pub fn grant_decide_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = field(&params, "session.grant_decide", "id")?;
    let decision = field(&params, "session.grant_decide", "decision")?;
    let reason = field(&params, "session.grant_decide", "reason")?;
    crate::with_ctx!(ctx, |ctx| decide(ctx, &id, &decision, &reason))
}

pub fn grant_revoke_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let id = field(&params, "session.grant_revoke", "id")?;
    let reason = field(&params, "session.grant_revoke", "reason")?;
    crate::with_ctx!(ctx, |ctx| revoke(ctx, &id, &reason))
}

pub fn grant_list_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    let state = text(&params, "state");
    let target = text(&params, "target");
    let limit = params
        .get("limit")
        .and_then(Value::as_i64)
        .ok_or_else(|| VogtError::InvalidRequest("session.grant_list needs a limit".to_string()))?;
    crate::with_ctx!(ctx, |ctx| list(
        ctx,
        state.as_deref(),
        target.as_deref(),
        limit
    ))
}

struct Request<'a> {
    reason: &'a str,
    target: &'a str,
    kind: &'a str,
    uses: &'a str,
    ttl_seconds: i64,
    secret_name: Option<&'a str>,
    project_id: Option<&'a str>,
    var: Option<&'a str>,
}

fn request<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    asked: &Request<'_>,
) -> Result<Value, VogtError> {
    if asked.kind != "credential" {
        return Err(VogtError::InvalidRequest(
            "capability grants are not available yet (WI-973 milestone 2); \
             ask for a named credential, or report the denied action as blocked"
                .to_string(),
        ));
    }
    let secret = asked.secret_name.unwrap_or("").trim().to_string();
    let project = asked.project_id.unwrap_or("").trim().to_string();
    if secret.is_empty() || project.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a credential grant names secret_name and project_id".to_string(),
        ));
    }
    if secret.starts_with('-')
        || project.starts_with('-')
        || !plain_name(&secret)
        || !plain_name(&project)
    {
        return Err(VogtError::InvalidRequest(
            "secret_name and project_id must be plain names (letters, digits, . _ - /)".to_string(),
        ));
    }
    let var = asked
        .var
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or_else(|| default_var(&secret), str::to_string);
    if !env_name(&var) {
        return Err(VogtError::InvalidRequest(format!(
            "var {} is not a valid environment variable name",
            crate::core::py_repr(&var)
        )));
    }
    let uses = parse_uses(asked.uses)?;

    let engine = engine_of(ctx)?;
    let target_engine_id = crate::application::services::resolve_session(ctx, asked.target)?;
    let live = engine.get_session(&target_engine_id)?;
    if live.as_ref().is_none_or(|session| !session.alive) {
        return Err(VogtError::Conflict(format!(
            "session {} is not running; a grant is for a live session",
            crate::core::py_repr(asked.target)
        )));
    }
    check_requester(ctx, &target_engine_id)?;

    let reason = asked.reason.to_string();
    let mut write = write_of(ctx);
    let ids = std::sync::Arc::clone(write.ids());
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        &mut write,
        "session.grant_request",
        &reason,
        |txn, actor| {
            let grant = SessionGrant {
                id: ids
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .next("grt"),
                target_engine_session_id: target_engine_id.clone(),
                kind: GrantKind::Credential,
                var: Some(var.clone()),
                project_id: Some(project.clone()),
                secret_name: Some(secret.clone()),
                capability: None,
                uses,
                ttl_seconds: asked.ttl_seconds,
                reason: reason.clone(),
                requested_by: actor.id.clone(),
                requested_at: clock_now(&clock),
                state: GrantState::Pending,
                decided_by: None,
                decided_at: None,
                decision_reason: None,
                expires_at: None,
                revoked_by: None,
                revoked_at: None,
            };
            txn.insert_session_grant(&grant)?;
            Ok(WriteOutcome {
                result: json!({ "grant": view_of(txn, &grant, clock_now(&clock))? }),
                entity_kind: "session_grant".to_string(),
                entity_id: grant.id.clone(),
                payload: serde_json::to_value(&grant).unwrap_or(Value::Null),
                event_kind: GRANT_REQUESTED_EVENT.to_string(),
                summary: summary_of(&grant),
            })
        },
    )
}

/// Approve or deny a pending grant. A person's decision, never an agent's.
fn decide<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: &str,
    decision: &str,
    reason: &str,
) -> Result<Value, VogtError> {
    if ctx.principal.kind == crate::core::ActorKind::Agent {
        return Err(VogtError::GrantRefused(format!(
            "only a person decides a grant, not an agent ({}); \
             approve or deny it from the Inbox",
            ctx.principal.identity_ref
        )));
    }
    let grant = load(ctx, id)?;
    if grant.state != GrantState::Pending {
        return Err(VogtError::Conflict(format!(
            "grant {} is {}, not pending",
            grant.id, grant.state
        )));
    }
    if decision == "deny" {
        return record_decision(ctx, &grant, reason, false, None, None);
    }

    let decided_at = clock_now(&ctx.clock);
    // `decided_at + ttl`, keeping the microseconds. Dropping them made the
    // stored row and the engine payload differ from Python's timedelta sum.
    let expires_at = Moment::from_unix(
        decided_at.unix_seconds().saturating_add(grant.ttl_seconds),
        decided_at.nanos(),
    );
    let engine = engine_of(ctx)?;
    // The engine first: the row says approved only once the grant is held.
    engine.apply_grant(
        &grant.target_engine_session_id,
        &json!({
            "grant_id": grant.id,
            "var": grant.var,
            "project_id": grant.project_id,
            "secret_name": grant.secret_name,
            "uses": grant.uses.to_string(),
            "expires_at": expires_at.to_json(),
            "reason": grant.reason,
        }),
    )?;
    match record_decision(
        ctx,
        &grant,
        reason,
        true,
        Some(decided_at),
        Some(expires_at),
    ) {
        Ok(result) => Ok(result),
        Err(error) => {
            // The record did not say approved, so the engine must not hold a
            // grant the record does not stand behind — unless another person's
            // approval of this same grant landed first, which taking back
            // would revoke.
            if !approved_meanwhile(ctx, &grant.id)? {
                engine.revoke_grant(&grant.target_engine_session_id, &grant.id)?;
            }
            Err(error)
        }
    }
}

/// Withdraw a pending grant or revoke an approved one. A person may revoke
/// any grant; the actor that asked for one may give it up.
fn revoke<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    id: &str,
    reason: &str,
) -> Result<Value, VogtError> {
    let grant = load(ctx, id)?;
    if grant.state != GrantState::Pending && grant.state != GrantState::Approved {
        return Err(VogtError::Conflict(format!(
            "grant {} is {}; there is nothing to revoke",
            grant.id, grant.state
        )));
    }
    if ctx.principal.kind == crate::core::ActorKind::Agent {
        let asker = ctx
            .declared
            .read()?
            .actor_by_identity(&ctx.principal.identity_ref)?;
        if asker
            .as_ref()
            .is_none_or(|actor| actor.id != grant.requested_by)
        {
            return Err(VogtError::GrantRefused(format!(
                "only a person, or the session that asked for it, revokes a grant \
                 ({} did neither)",
                ctx.principal.identity_ref
            )));
        }
    }
    if grant.state == GrantState::Approved {
        if let Some(engine) = &ctx.engine {
            // Absence at the engine is the revoked state, so a grant the
            // engine already lost still records as revoked.
            engine.revoke_grant(&grant.target_engine_session_id, &grant.id)?;
        }
    }

    let reason = reason.to_string();
    let mut write = write_of(ctx);
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(&mut write, "session.grant_revoke", &reason, |txn, actor| {
        let current = txn.session_grant(&grant.id)?.ok_or_else(|| {
            VogtError::NotFound(format!("no grant {}", crate::core::py_repr(&grant.id)))
        })?;
        let revoked = SessionGrant {
            state: GrantState::Revoked,
            revoked_by: Some(actor.id.clone()),
            revoked_at: Some(clock_now(&clock)),
            ..current
        };
        txn.update_session_grant(&revoked)?;
        Ok(WriteOutcome {
            result: json!({ "grant": view_of(txn, &revoked, clock_now(&clock))? }),
            entity_kind: "session_grant".to_string(),
            entity_id: revoked.id.clone(),
            payload: serde_json::to_value(&revoked).unwrap_or(Value::Null),
            event_kind: GRANT_REVOKED_EVENT.to_string(),
            summary: summary_of(&revoked),
        })
    })
}

/// Grants, newest first. A person sees every grant. An agent sees its own
/// session's; an agent that is not a session sees nothing.
fn list<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    state: Option<&str>,
    target: Option<&str>,
    limit: i64,
) -> Result<Value, VogtError> {
    let mut target = match target {
        Some(reference) => Some(crate::application::services::resolve_session(
            ctx, reference,
        )?),
        None => None,
    };
    if ctx.principal.kind == crate::core::ActorKind::Agent {
        let own = own_engine_session(ctx)?;
        if own.is_none() || (target.is_some() && target != own) {
            return Ok(json!({ "grants": [] }));
        }
        target = own;
    }
    let stored = if state == Some("expired") {
        Some("approved")
    } else {
        state
    };
    let view = ctx.declared.read()?;
    let rows = view.list_session_grants(stored, target.as_deref(), limit)?;
    let mut grants = Vec::new();
    for row in &rows {
        // The clock is read once per row, as Python's view does, so a step
        // clock shared across calls advances the same number of ticks in both
        // cores. A read up front would tick once even for an empty list.
        grants.push(view_of(&view, row, clock_now(&ctx.clock))?);
    }
    if matches!(state, Some("approved" | "expired")) {
        grants.retain(|grant| grant["state"].as_str() == state);
    }
    Ok(json!({ "grants": grants }))
}

/// An agent may ask for its own session; for another, only an overseer.
fn check_requester<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    target_engine_id: &str,
) -> Result<(), VogtError> {
    if ctx.principal.kind != crate::core::ActorKind::Agent {
        return Ok(());
    }
    let own = own_engine_session(ctx)?;
    if own.as_deref() == Some(target_engine_id) {
        return Ok(());
    }
    let Some(own) = own else {
        return Err(VogtError::GrantRefused(format!(
            "{} is not a session, so it can ask for a grant only through an oversight session",
            ctx.principal.identity_ref
        )));
    };
    let asker = engine_of(ctx)?.get_session(&own)?;
    if asker
        .as_ref()
        .is_none_or(|session| session.role != "oversight")
    {
        return Err(VogtError::GrantRefused(
            "only an oversight session asks for a grant on another session's \
             behalf; ask your overseer, or ask for your own session"
                .to_string(),
        ));
    }
    Ok(())
}

/// The engine session the calling agent runs in, from its identity.
pub(crate) fn own_engine_session<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<Option<String>, VogtError> {
    let reference = &ctx.principal.identity_ref;
    if let Some(id) = reference.strip_prefix("agent:engine:") {
        return Ok(Some(id.to_string()));
    }
    if let Some(session_id) = reference.strip_prefix("agent:session:") {
        let session = ctx.declared.read()?.session_by_id(session_id)?;
        return Ok(session.map(|session| session.engine_session_id));
    }
    Ok(None)
}

fn approved_meanwhile<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    grant_id: &str,
) -> Result<bool, VogtError> {
    Ok(ctx
        .declared
        .read()?
        .session_grant(grant_id)?
        .is_some_and(|grant| grant.state == GrantState::Approved))
}

fn load<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    grant_id: &str,
) -> Result<SessionGrant, VogtError> {
    ctx.declared
        .read()?
        .session_grant(grant_id.trim())?
        .ok_or_else(|| VogtError::NotFound(format!("no grant {}", crate::core::py_repr(grant_id))))
}

#[cfg(test)]
type RecordHook = Box<dyn FnOnce() -> Result<(), VogtError>>;

#[cfg(test)]
thread_local! {
    /// Test seam: runs once, just before a decision is recorded, so a test can
    /// make the record fail or let another person's decision land first.
    static BEFORE_RECORD: std::cell::RefCell<Option<RecordHook>> = std::cell::RefCell::new(None);
}

/// Runs the test seam, when a test has armed one.
fn before_record() -> Result<(), VogtError> {
    #[cfg(test)]
    {
        let hook = BEFORE_RECORD.with(|slot| slot.borrow_mut().take());
        if let Some(hook) = hook {
            return hook();
        }
    }
    Ok(())
}

fn record_decision<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    grant: &SessionGrant,
    reason: &str,
    approved: bool,
    decided_at: Option<Moment>,
    expires_at: Option<Moment>,
) -> Result<Value, VogtError> {
    before_record()?;
    let grant_id = grant.id.clone();
    let reason = reason.to_string();
    let mut write = write_of(ctx);
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(&mut write, "session.grant_decide", &reason, |txn, actor| {
        let current = txn.session_grant(&grant_id)?;
        if current
            .as_ref()
            .is_none_or(|row| row.state != GrantState::Pending)
        {
            let state = current.map_or("gone".to_string(), |row| row.state.to_string());
            return Err(VogtError::Conflict(format!(
                "grant {grant_id} is {state}, not pending"
            )));
        }
        let Some(current) = current else {
            return Err(VogtError::Conflict(format!(
                "grant {grant_id} is gone, not pending"
            )));
        };
        let decided = SessionGrant {
            state: if approved {
                GrantState::Approved
            } else {
                GrantState::Denied
            },
            decided_by: Some(actor.id.clone()),
            decided_at: Some(decided_at.unwrap_or_else(|| clock_now(&clock))),
            decision_reason: Some(reason.clone()),
            expires_at: if approved { expires_at } else { None },
            ..current
        };
        txn.update_session_grant(&decided)?;
        Ok(WriteOutcome {
            result: json!({ "grant": view_of(txn, &decided, clock_now(&clock))? }),
            entity_kind: "session_grant".to_string(),
            entity_id: decided.id.clone(),
            payload: serde_json::to_value(&decided).unwrap_or(Value::Null),
            event_kind: GRANT_DECIDED_EVENT.to_string(),
            summary: summary_of(&decided),
        })
    })
}

fn view_of(view: &impl ReadView, grant: &SessionGrant, now: Moment) -> Result<Value, VogtError> {
    Ok(json!({
        "id": grant.id,
        "target": grant.target_engine_session_id,
        "kind": grant.kind.to_string(),
        "var": grant.var,
        "project_id": grant.project_id,
        "secret_name": grant.secret_name,
        "capability": grant.capability,
        "uses": grant.uses.to_string(),
        "ttl_seconds": grant.ttl_seconds,
        "reason": grant.reason,
        "requested_by": identity_of(view, &grant.requested_by)?,
        "requested_at": grant.requested_at.to_json(),
        "state": grant.effective_state(now),
        "decided_by": optional_identity(view, grant.decided_by.as_deref())?,
        "decided_at": grant.decided_at.map(|moment| moment.to_json()),
        "decision_reason": grant.decision_reason,
        "expires_at": grant.expires_at.map(|moment| moment.to_json()),
        "revoked_by": optional_identity(view, grant.revoked_by.as_deref())?,
        "revoked_at": grant.revoked_at.map(|moment| moment.to_json()),
    }))
}

fn identity_of(view: &impl ReadView, actor_id: &str) -> Result<String, VogtError> {
    Ok(view
        .actor_by_id(actor_id)?
        .map_or_else(|| actor_id.to_string(), |actor| actor.identity_ref))
}

fn optional_identity(
    view: &impl ReadView,
    actor_id: Option<&str>,
) -> Result<Option<String>, VogtError> {
    actor_id.map(|id| identity_of(view, id)).transpose()
}

fn summary_of(grant: &SessionGrant) -> Value {
    json!({
        "grant_id": grant.id,
        "target": grant.target_engine_session_id,
        "kind": grant.kind.to_string(),
        "var": grant.var,
        "project_id": grant.project_id,
        "secret_name": grant.secret_name,
        "uses": grant.uses.to_string(),
        "state": grant.state.to_string(),
    })
}

/// `GRANT_<secret>` with everything outside an environment name turned into `_`.
fn default_var(secret_name: &str) -> String {
    let mut name = String::from("GRANT_");
    for character in secret_name.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            name.push(character.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}

fn plain_name(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._/-".contains(character))
}

fn env_name(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    value.len() <= 128
        && chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn parse_uses(uses: &str) -> Result<crate::core::GrantUses, VogtError> {
    match uses {
        "once" => Ok(crate::core::GrantUses::Once),
        "ttl" => Ok(crate::core::GrantUses::Ttl),
        other => Err(VogtError::InvalidRequest(format!(
            "uses must be once or ttl, got {}",
            crate::core::py_repr(other)
        ))),
    }
}

fn engine_of<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
) -> Result<&crate::adapters::engine::EngineClient, VogtError> {
    ctx.engine.as_ref().ok_or_else(|| {
        VogtError::EngineUnavailable(
            "no session engine is configured, so there is nothing to open a \
                 terminal on (set VOGT_ENGINE_URL)"
                .to_string(),
        )
    })
}

fn field(params: &Value, operation: &str, name: &str) -> Result<String, VogtError> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| VogtError::InvalidRequest(format!("{operation} needs a {name}")))
}

fn text(params: &Value, name: &str) -> Option<String> {
    params.get(name).and_then(Value::as_str).map(str::to_string)
}

fn clock_now<C: Clock>(clock: &std::sync::Arc<std::sync::Mutex<C>>) -> Moment {
    clock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .now()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock};
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

    /// A context whose engine answers every session lookup as alive and records
    /// every request it was sent.
    fn fresh(
        name: &str,
        principal: Principal,
    ) -> (
        std::path::PathBuf,
        crate::application::context::Built,
        Arc<Mutex<Vec<String>>>,
    ) {
        let dir = std::env::temp_dir().join(format!("vogt-grant-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                log.lock().unwrap().push(format!("{method} {path}"));
                let answer = if method == "GET" {
                    br#"{"id":"eng","alive":true,"role":"worker"}"#.to_vec()
                } else {
                    body.to_vec()
                };
                (200, answer)
            })),
        );
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir.clone(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        (dir, built, seen)
    }

    /// A second context over a context `fresh` already initialised, so two
    /// principals can act on the same store.
    fn fresh_over(
        dir: &std::path::Path,
        principal: Principal,
    ) -> (
        std::path::PathBuf,
        crate::application::context::Built,
        Arc<Mutex<Vec<String>>>,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let engine = EngineClient::new(
            "http://engine",
            None,
            Some(std::sync::Arc::new(move |path, _, body, method| {
                log.lock().unwrap().push(format!("{method} {path}"));
                let answer = if method == "GET" {
                    br#"{"id":"eng","alive":true,"role":"worker"}"#.to_vec()
                } else {
                    body.to_vec()
                };
                (200, answer)
            })),
        );
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir.to_path_buf(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(1_700_000_000, 123_000))),
            None,
            None,
            Some(engine),
            None,
            None,
        )
        .unwrap();
        (dir.to_path_buf(), built, seen)
    }

    fn person() -> Principal {
        Principal::new("local:ada", ActorKind::Human, "Ada").unwrap()
    }

    fn agent(identity: &str) -> Principal {
        Principal::new(identity, ActorKind::Agent, "agent").unwrap()
    }

    fn request(target: &str) -> serde_json::Value {
        json!({
            "reason": "needed",
            "target": target,
            "kind": "credential",
            "uses": "once",
            "ttl_seconds": 60,
            "secret_name": "token",
            "project_id": "proj",
        })
    }

    /// Record a Vogt session so a `ses_…` reference has something to resolve to.
    fn record_session(built: &crate::application::context::Built, id: &str, engine_id: &str) {
        crate::with_ctx!(built, |ctx| {
            let mut write = crate::application::context::write_of(ctx);
            let id = id.to_string();
            let engine_id = engine_id.to_string();
            audited_write(&mut write, "session.start", "test", |txn, actor| {
                txn.insert_project(&Project::new(
                    "prj_1",
                    "proj",
                    "Proj",
                    "/work",
                    Moment::from_unix(1_700_000_000, 0),
                ))?;
                txn.insert_session(&CodingSession {
                    id,
                    engine_session_id: engine_id,
                    project_id: "prj_1".to_string(),
                    work_item_id: None,
                    actor_id: actor.id.clone(),
                    cwd: "/work".to_string(),
                    template: None,
                    model: None,
                    effort: None,
                    reason: "test".to_string(),
                    started_at: Moment::from_unix(1_700_000_000, 0),
                    stopped_at: None,
                })?;
                Ok(crate::application::writes::WriteOutcome {
                    result: json!({}),
                    entity_kind: "session".to_string(),
                    entity_id: "ses".to_string(),
                    payload: json!({}),
                    event_kind: "session.started".to_string(),
                    summary: json!({}),
                })
            })
        })
        .unwrap();
    }

    #[test]
    fn an_agent_cannot_decide_a_grant() {
        let (dir, built, _) = fresh("decide", person());
        let asked = super::grant_request_op(&built, request("eng-1")).unwrap();
        let (_, agent_ctx, _) = fresh_over(&dir, agent("agent:engine:eng-1"));
        let error = super::grant_decide_op(
            &agent_ctx,
            json!({"id": asked["grant"]["id"], "decision": "approve", "reason": "no"}),
        )
        .unwrap_err();
        assert!(
            matches!(error, VogtError::GrantRefused(ref message) if message.contains("only a person")),
            "{error:?}"
        );
    }

    #[test]
    fn approving_keeps_the_microseconds_and_a_person_sees_the_grant() {
        let (_, built, seen) = fresh("approve", person());
        let asked = super::grant_request_op(&built, request("eng-1")).unwrap();
        let id = asked["grant"]["id"].as_str().unwrap().to_string();
        let decided = super::grant_decide_op(
            &built,
            json!({"id": id, "decision": "approve", "reason": "yes"}),
        )
        .unwrap();
        // The step clock advances one second per read. The request takes four
        // reads, so the decision is the fifth: 1_700_000_004 plus the 60s ttl,
        // and the 123µs must survive the sum.
        assert_eq!(
            decided["grant"]["expires_at"],
            "2023-11-14T22:14:24.000123Z"
        );
        let listed =
            super::grant_list_op(&built, json!({"state": "approved", "limit": 10})).unwrap();
        assert_eq!(listed["grants"][0]["id"], id);
        let sent = seen.lock().unwrap().clone();
        assert!(
            sent.iter().any(
                |call| call.starts_with("POST ") && call.contains("/api/sessions/eng-1/grants")
            ),
            "the engine was never asked to apply: {sent:?}"
        );
    }

    #[test]
    fn deciding_the_same_grant_twice_conflicts_and_asks_the_engine_once() {
        // The decision re-reads the row inside the write and proceeds only
        // while it is still pending, so a second decider loses cleanly.
        let (_, built, seen) = fresh("twice", person());
        let asked = super::grant_request_op(&built, request("eng-1")).unwrap();
        let id = asked["grant"]["id"].as_str().unwrap().to_string();
        let params = json!({"id": id, "decision": "approve", "reason": "yes"});
        super::grant_decide_op(&built, params.clone()).unwrap();

        let again = super::grant_decide_op(&built, params).unwrap_err();
        assert!(
            matches!(again, VogtError::Conflict(ref message) if message == &format!("grant {id} is approved, not pending")),
            "{again:?}"
        );
        // A denial of an already-decided grant is the same conflict.
        let denied = super::grant_decide_op(
            &built,
            json!({"id": id, "decision": "deny", "reason": "no"}),
        )
        .unwrap_err();
        assert!(
            matches!(denied, VogtError::Conflict(ref message) if message.contains("is approved, not pending")),
            "{denied:?}"
        );
        // The engine heard the one approval and nothing for the losers.
        let sent = seen.lock().unwrap().clone();
        let applies = sent
            .iter()
            .filter(|call| call.starts_with("POST ") && call.contains("/grants"))
            .count();
        assert_eq!(applies, 1, "{sent:?}");
    }

    #[test]
    fn an_agent_sees_only_its_own_sessions_grants() {
        let (_, built, _) = fresh("scope", agent("agent:engine:eng-1"));
        super::grant_request_op(&built, request("eng-1")).unwrap();
        let own = super::grant_list_op(&built, json!({"limit": 10})).unwrap();
        assert_eq!(own["grants"].as_array().unwrap().len(), 1);
        let other = super::grant_list_op(&built, json!({"target": "eng-2", "limit": 10})).unwrap();
        assert_eq!(other["grants"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn an_unknown_session_id_is_not_found_and_a_blank_one_is_refused() {
        let (_, built, seen) = fresh("target", person());
        let unknown = super::grant_request_op(&built, request("  ses_nope  ")).unwrap_err();
        assert!(
            matches!(unknown, VogtError::NotFound(ref message) if message.contains("no session 'ses_nope'")),
            "{unknown:?}"
        );
        let blank = super::grant_request_op(&built, request("   ")).unwrap_err();
        assert!(matches!(blank, VogtError::InvalidRequest(_)), "{blank:?}");
        // Neither reaches the engine: a wrong id is refused before any lookup.
        assert!(
            seen.lock().unwrap().is_empty(),
            "{:?}",
            seen.lock().unwrap()
        );
    }

    #[test]
    fn a_recorded_session_resolves_to_its_engine_id() {
        let (_, built, _) = fresh("resolve", person());
        record_session(&built, "ses_0001", "eng-9");
        let asked = super::grant_request_op(&built, request("ses_0001")).unwrap();
        assert_eq!(asked["grant"]["target"], "eng-9");
    }

    // -- the rest of the grant invariants, against a stand-in engine that keeps
    // each session's role and liveness and records what it was sent --------

    const BASE: i64 = 1_700_000_000;
    const SECRET: &str = "100.109.218.11_SSH";
    const WHY: &str = "grant test";

    #[derive(Default)]
    struct StandIn {
        /// Engine session id to (alive, role).
        sessions: std::collections::BTreeMap<String, (bool, String)>,
        /// Projects the engine refuses, as one not open to grants is.
        closed_projects: std::collections::BTreeSet<String>,
        applied: Vec<(String, Value)>,
        revoked: Vec<(String, String)>,
    }

    type Shared = Arc<Mutex<StandIn>>;

    fn engine_row(id: &str, alive: bool, role: &str) -> Value {
        json!({
            "id": id,
            "name": id,
            "activity": if alive { "idle" } else { "exited" },
            "alive": alive,
            "exit_code": if alive { Value::Null } else { json!(0) },
            "role": role,
            "cwd": "/srv",
        })
    }

    fn stand_in(shared: &Shared) -> EngineClient {
        let shared = Arc::clone(shared);
        EngineClient::new(
            "http://engine",
            None,
            Some(Arc::new(move |path, _, body, method| {
                let route = path.split("/api/").nth(1).unwrap_or(path).to_string();
                let payload: Value = if body.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(body).unwrap_or(Value::Null)
                };
                let mut state = shared.lock().unwrap();
                let parts: Vec<&str> = route.split('/').collect();
                match (method, parts.as_slice()) {
                    ("GET", ["sessions"]) => {
                        let rows: Vec<Value> = state
                            .sessions
                            .iter()
                            .map(|(id, (alive, role))| engine_row(id, *alive, role))
                            .collect();
                        (200, serde_json::to_vec(&rows).unwrap())
                    }
                    ("GET", ["sessions", id]) => match state.sessions.get(*id) {
                        Some((alive, role)) => (
                            200,
                            serde_json::to_vec(&engine_row(id, *alive, role)).unwrap(),
                        ),
                        None => (404, Vec::new()),
                    },
                    ("POST", ["sessions", id, "grants"]) => {
                        let closed = payload["project_id"]
                            .as_str()
                            .is_some_and(|project| state.closed_projects.contains(project));
                        if closed {
                            let refusal =
                                json!({"error": "forbidden: project is not open to grants"});
                            return (403, serde_json::to_vec(&refusal).unwrap());
                        }
                        state.applied.push((id.to_string(), payload));
                        (200, body.to_vec())
                    }
                    ("DELETE", ["sessions", id, "grants", grant]) => {
                        state.revoked.push((id.to_string(), grant.to_string()));
                        (200, br#"{"revoked":true}"#.to_vec())
                    }
                    _ => (404, Vec::new()),
                }
            })),
        )
    }

    /// A Vogt instance with the four engine sessions the invariants name, and
    /// the person's context over it. `over` makes another principal's.
    fn estate(
        name: &str,
    ) -> (
        std::path::PathBuf,
        Shared,
        crate::application::context::Built,
    ) {
        let dir = std::env::temp_dir().join(format!("vogt-grant-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut clock = None;
        let mut ids = None;
        crate::application::instance::init(&dir, &mut clock, &mut ids).unwrap();
        let mut sessions = std::collections::BTreeMap::new();
        sessions.insert("eng-worker".to_string(), (true, "worker".to_string()));
        sessions.insert("eng-other".to_string(), (true, "worker".to_string()));
        sessions.insert("eng-overseer".to_string(), (true, "oversight".to_string()));
        sessions.insert("eng-gone".to_string(), (false, "worker".to_string()));
        let shared: Shared = Arc::new(Mutex::new(StandIn {
            sessions,
            ..StandIn::default()
        }));
        let built = over(&dir, person(), &shared, BASE);
        (dir, shared, built)
    }

    fn over(
        dir: &std::path::Path,
        principal: Principal,
        shared: &Shared,
        start: i64,
    ) -> crate::application::context::Built {
        build_context(
            crate::config::VogtConfig {
                data_dir: dir.to_path_buf(),
                ..crate::config::VogtConfig::default()
            },
            Some(principal),
            Some(StepClock::new(Moment::from_unix(start, 0))),
            None,
            None,
            Some(stand_in(shared)),
            None,
            None,
        )
        .unwrap()
    }

    fn try_ask(
        built: &crate::application::context::Built,
        target: &str,
        overrides: Value,
    ) -> Result<Value, VogtError> {
        let mut params = json!({
            "reason": "the worker needs the emulator key",
            "target": target,
            "kind": "credential",
            "uses": "once",
            "ttl_seconds": 60,
            "secret_name": SECRET,
            "project_id": "infra",
        });
        for (key, value) in overrides.as_object().unwrap() {
            params[key] = value.clone();
        }
        super::grant_request_op(built, params)
    }

    fn ask(built: &crate::application::context::Built, target: &str, overrides: Value) -> String {
        try_ask(built, target, overrides).unwrap()["grant"]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn decide(
        built: &crate::application::context::Built,
        id: &str,
        decision: &str,
    ) -> Result<Value, VogtError> {
        super::grant_decide_op(
            built,
            json!({"id": id, "decision": decision, "reason": WHY}),
        )
    }

    fn revoke(built: &crate::application::context::Built, id: &str) -> Result<Value, VogtError> {
        super::grant_revoke_op(built, json!({"id": id, "reason": WHY}))
    }

    fn list(built: &crate::application::context::Built, state: Option<&str>) -> Value {
        let mut params = json!({"limit": 50});
        if let Some(state) = state {
            params["state"] = json!(state);
        }
        super::grant_list_op(built, params).unwrap()
    }

    fn agent_named(identity: &str) -> Principal {
        Principal::new(identity, ActorKind::Agent, "agent").unwrap()
    }

    fn overseer_of(dir: &std::path::Path, shared: &Shared) -> crate::application::context::Built {
        over(dir, agent_named("agent:engine:eng-overseer"), shared, BASE)
    }

    #[test]
    fn an_engine_refusal_leaves_the_request_pending_with_its_reason() {
        let (dir, shared, built) = estate("refusal");
        shared
            .lock()
            .unwrap()
            .closed_projects
            .insert("infra".to_string());
        let id = ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));
        let refused = decide(&built, &id, "approve").unwrap_err();
        assert!(
            matches!(refused, VogtError::GrantRefused(ref message) if message.contains("not open to grants")),
            "{refused:?}"
        );
        assert_eq!(list(&built, None)["grants"][0]["state"], "pending");
        let denied = decide(&built, &id, "deny").unwrap();
        assert_eq!(denied["grant"]["state"], "denied");
        assert!(shared.lock().unwrap().applied.is_empty());
    }

    #[test]
    fn only_an_overseer_asks_on_another_sessions_behalf() {
        let (dir, shared, built) = estate("behalf");
        let worker_other = over(&dir, agent_named("agent:engine:eng-other"), &shared, BASE);
        let refused = try_ask(&worker_other, "eng-worker", json!({})).unwrap_err();
        assert!(
            matches!(refused, VogtError::GrantRefused(ref message) if message.contains("only an oversight session")),
            "{refused:?}"
        );
        // Its own session is fine; so is an overseer for a worker, and a person.
        let worker = over(&dir, agent_named("agent:engine:eng-worker"), &shared, BASE);
        ask(&worker, "eng-worker", json!({}));
        ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));
        ask(&built, "eng-other", json!({}));
        // An agent that is not a session at all, the pod's token, cannot ask.
        let pod = over(&dir, agent_named("agent:pod:vogt"), &shared, BASE);
        let not_a_session = try_ask(&pod, "eng-worker", json!({})).unwrap_err();
        assert!(
            matches!(not_a_session, VogtError::GrantRefused(ref message) if message.contains("not a session")),
            "{not_a_session:?}"
        );
        assert!(shared.lock().unwrap().applied.is_empty());
    }

    #[test]
    fn the_requester_is_the_principal_not_a_parameter() {
        let (dir, shared, built) = estate("requester");
        ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));
        let rows = list(&built, None);
        assert_eq!(
            rows["grants"][0]["requested_by"],
            "agent:engine:eng-overseer"
        );
    }

    #[test]
    fn an_agent_cannot_make_itself_an_overseer() {
        let (dir, shared, _) = estate("self-overseer");
        for who in ["eng-other", "eng-worker"] {
            let agent_ctx = over(
                &dir,
                agent_named(&format!("agent:engine:{who}")),
                &shared,
                BASE,
            );
            let refused = crate::registry::default_registry()
                .get("session.set_role")
                .expect("session.set_role is registered")
                .run(
                    Some(&agent_ctx),
                    json!({"id": who, "role": "oversight", "reason": WHY}),
                )
                .unwrap_err();
            assert!(
                matches!(refused, VogtError::RoleRefused(ref message) if message.contains("only a person nominates")),
                "{refused:?}"
            );
        }
        let agent_ctx = over(&dir, agent_named("agent:engine:eng-other"), &shared, BASE);
        let refused = try_ask(&agent_ctx, "eng-worker", json!({})).unwrap_err();
        assert!(
            matches!(refused, VogtError::GrantRefused(ref message) if message.contains("only an oversight session")),
            "{refused:?}"
        );
    }

    #[test]
    fn a_failed_approval_takes_the_grant_back_unless_another_person_approved() {
        let (dir, shared, built) = estate("rollback");
        let id = ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));

        // The record fails after the engine has applied the grant.
        super::BEFORE_RECORD.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|| {
                Err(VogtError::InvalidRequest(
                    "the record could not be written".to_string(),
                ))
            }))
        });
        let failed = decide(&built, &id, "approve").unwrap_err();
        assert!(matches!(failed, VogtError::InvalidRequest(_)), "{failed:?}");
        assert_eq!(
            shared.lock().unwrap().revoked,
            vec![("eng-worker".to_string(), id.clone())]
        );
        assert_eq!(list(&built, None)["grants"][0]["state"], "pending");

        // Two people approve at once: the other's approval lands first and
        // stands at the engine, so nothing is taken back.
        shared.lock().unwrap().revoked.clear();
        let grace = over(
            &dir,
            Principal::new("local:grace", ActorKind::Human, "Grace").unwrap(),
            &shared,
            BASE,
        );
        let racing = id.clone();
        super::BEFORE_RECORD.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                decide(&grace, &racing, "approve").map(|_| ())
            }))
        });
        let lost = decide(&built, &id, "approve").unwrap_err();
        assert!(matches!(lost, VogtError::Conflict(_)), "{lost:?}");
        assert!(
            shared.lock().unwrap().revoked.is_empty(),
            "the other person's approval was not taken back"
        );
        assert_eq!(list(&built, None)["grants"][0]["state"], "approved");
    }

    #[test]
    fn a_grant_is_one_named_credential_for_a_live_session() {
        let (_, _, built) = estate("one-credential");
        let capability = try_ask(
            &built,
            "eng-worker",
            json!({"kind": "capability", "capability": "bypass"}),
        )
        .unwrap_err();
        assert!(
            matches!(capability, VogtError::InvalidRequest(ref message) if message.contains("capability grants are not available")),
            "{capability:?}"
        );
        let no_project = try_ask(&built, "eng-worker", json!({"project_id": null})).unwrap_err();
        assert!(
            matches!(no_project, VogtError::InvalidRequest(ref message) if message.contains("names secret_name and project_id")),
            "{no_project:?}"
        );
        let flag_as_name =
            try_ask(&built, "eng-worker", json!({"secret_name": "--all"})).unwrap_err();
        assert!(
            matches!(flag_as_name, VogtError::InvalidRequest(ref message) if message.contains("plain names")),
            "{flag_as_name:?}"
        );
        let bad_var = try_ask(&built, "eng-worker", json!({"var": "not a var"})).unwrap_err();
        assert!(
            matches!(bad_var, VogtError::InvalidRequest(ref message) if message.contains("environment variable")),
            "{bad_var:?}"
        );
        let gone = try_ask(&built, "eng-gone", json!({})).unwrap_err();
        assert!(
            matches!(gone, VogtError::Conflict(ref message) if message.contains("not running")),
            "{gone:?}"
        );
        assert_eq!(super::default_var(SECRET), "GRANT_100_109_218_11_SSH");
    }

    #[test]
    fn an_approved_grant_reads_expired_after_its_ttl() {
        let (dir, shared, built) = estate("ttl");
        let id = ask(&built, "eng-worker", json!({"ttl_seconds": 60}));
        decide(&built, &id, "approve").unwrap();
        let later = over(&dir, person(), &shared, BASE + 120);
        let expired = list(&later, Some("expired"));
        assert_eq!(expired["grants"].as_array().unwrap().len(), 1);
        assert_eq!(expired["grants"][0]["id"], id);
        assert!(list(&later, Some("approved"))["grants"]
            .as_array()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn revoking_reaches_the_engine_and_only_a_person_or_the_asker_may() {
        let (dir, shared, built) = estate("revoke");
        let overseer = overseer_of(&dir, &shared);
        let id = ask(&overseer, "eng-worker", json!({"uses": "ttl"}));
        decide(&built, &id, "approve").unwrap();

        let worker = over(&dir, agent_named("agent:engine:eng-worker"), &shared, BASE);
        let refused = revoke(&worker, &id).unwrap_err();
        assert!(
            matches!(refused, VogtError::GrantRefused(ref message) if message.contains("only a person, or the session that asked")),
            "{refused:?}"
        );
        let revoked = revoke(&overseer, &id).unwrap();
        assert_eq!(revoked["grant"]["state"], "revoked");
        assert_eq!(
            shared.lock().unwrap().revoked,
            vec![("eng-worker".to_string(), id.clone())]
        );

        // A pending one is withdrawn without troubling the engine.
        let pending = ask(&built, "eng-worker", json!({}));
        revoke(&built, &pending).unwrap();
        assert_eq!(shared.lock().unwrap().revoked.len(), 1);
    }

    #[test]
    fn every_step_is_audited_by_name() {
        let (dir, shared, built) = estate("audit");
        let id = ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));
        decide(&built, &id, "approve").unwrap();
        revoke(&built, &id).unwrap();
        let mut operations: Vec<String> = crate::with_ctx!(&built, |ctx| {
            ctx.declared
                .read()
                .unwrap()
                .list_audit(&crate::storage::interface::AuditQuery {
                    limit: 20,
                    offset: 0,
                    actor_id: None,
                    operation: None,
                    entity_id: Some(id.clone()),
                    project_id: None,
                    since: None,
                    until: None,
                })
        })
        .unwrap()
        .into_iter()
        .map(|row| row.operation)
        .collect();
        operations.sort();
        assert_eq!(
            operations,
            [
                "session.grant_decide",
                "session.grant_request",
                "session.grant_revoke"
            ]
        );
    }

    #[test]
    fn a_pending_grant_is_an_inbox_entry_until_it_is_decided() {
        let (dir, shared, built) = estate("inbox");
        let id = ask(&overseer_of(&dir, &shared), "eng-worker", json!({}));
        let inbox = |built: &crate::application::context::Built| -> Vec<Value> {
            crate::registry::default_registry()
                .get("inbox.list")
                .expect("inbox.list is registered")
                .run(Some(built), json!({}))
                .unwrap()["entries"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|entry| entry["kind"] == "session.grant_request")
                .cloned()
                .collect()
        };
        let entries = inbox(&built);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry["action"]["kind"], "grant");
        assert_eq!(entry["action"]["grant_id"], id);
        assert!(entry["title"].as_str().unwrap().contains(SECRET));
        // The approver sees the target as they know it, and who asks for whom.
        assert!(entry["title"]
            .as_str()
            .unwrap()
            .contains("for session eng-worker"));
        let summary = entry["summary"].as_str().unwrap();
        assert!(
            summary.contains("for session eng-worker (worker)"),
            "{summary}"
        );
        assert!(summary.contains("agent:engine:eng-overseer"), "{summary}");
        assert!(summary.contains("infra"), "{summary}");
        assert_eq!(entry["actor_login"], "agent:engine:eng-overseer");
        assert_eq!(entry["actor_kind"], "bot");
        assert_eq!(entry["evidence_snapshot"]["target_role"], "worker");
        assert_eq!(entry["evidence_snapshot"]["requester_is_target"], false);

        // A session asking for itself reads as such.
        let own = ask(
            &over(&dir, agent_named("agent:engine:eng-other"), &shared, BASE),
            "eng-other",
            json!({}),
        );
        let self_entry = inbox(&built)
            .into_iter()
            .find(|entry| entry["action"]["grant_id"] == own)
            .expect("the self-request is an entry");
        let self_summary = self_entry["summary"].as_str().unwrap();
        assert!(self_summary.contains("asks for") && self_summary.contains("for itself"));
        assert_eq!(self_entry["evidence_snapshot"]["requester_is_target"], true);

        decide(&built, &own, "deny").unwrap();
        decide(&built, &id, "deny").unwrap();
        assert!(inbox(&built).is_empty());
    }
}
