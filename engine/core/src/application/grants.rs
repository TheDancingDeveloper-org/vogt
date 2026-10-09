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
    let target_engine_id = resolve_target(ctx, asked.target)?;
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
        Some(reference) => Some(resolve_target(ctx, reference)?),
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
    let now = clock_now(&ctx.clock);
    let view = ctx.declared.read()?;
    let rows = view.list_session_grants(stored, target.as_deref(), limit)?;
    let mut grants = Vec::new();
    for row in &rows {
        grants.push(view_of(&view, row, now)?);
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
fn own_engine_session<C: Clock, I: IdFactory>(
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

fn record_decision<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    grant: &SessionGrant,
    reason: &str,
    approved: bool,
    decided_at: Option<Moment>,
    expires_at: Option<Moment>,
) -> Result<Value, VogtError> {
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

/// A session reference is either a Vogt session id or an engine session id.
///
/// Inlined from `sessions.py`'s `_target` until W9 lands the sessions module,
/// which should own this once and replace the copy. A `ses_…` id must be one
/// Vogt recorded — an unknown one is a wrong id, not an engine id. Anything
/// else is the engine's own id, passed through as given.
fn resolve_target<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    target: &str,
) -> Result<String, VogtError> {
    let wanted = target.trim();
    if wanted.is_empty() {
        return Err(VogtError::InvalidRequest(
            "a session id is required (ses_… or the engine's session UUID)".to_string(),
        ));
    }
    let view = ctx.declared.read()?;
    if wanted.starts_with("ses_") {
        let session = view.session_by_id(wanted)?.ok_or_else(|| {
            VogtError::NotFound(format!("no session {}", crate::core::py_repr(wanted)))
        })?;
        return Ok(session.engine_session_id);
    }
    Ok(wanted.to_string())
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

    use serde_json::json;

    use crate::adapters::engine::EngineClient;
    use crate::application::context::build_context;
    use crate::application::writes::audited_write;
    use crate::core::{ActorKind, CodingSession, Moment, Principal, Project, StepClock};
    use crate::errors::VogtError;
    use crate::storage::interface::WriteTxn;

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
            Some(Box::new(move |path, _, body, method| {
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
            Some(Box::new(move |path, _, body, method| {
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
}
