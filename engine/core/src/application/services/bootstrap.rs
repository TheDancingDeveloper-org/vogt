//! Bootstrap token adoption. Ports `adopt_bootstrap_core_token` and
//! `adopt_bootstrap_agent_token` in `services/auth.py`.

use crate::application::context::AppContext;
use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
use crate::auth;
use crate::core::{Actor, ActorKind, Clock, IdFactory, Moment, Token};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

use super::auth::scopes_of;

const TOKEN_ADOPT: &str = "token.adopt";
const TOKEN_ADOPTED_EVENT: &str = "token.adopted";
const BOOTSTRAP_CORE_TOKEN_NAME: &str = "bootstrap core token";
const BOOTSTRAP_AGENT_TOKEN_NAME: &str = "bootstrap agent token";

/// What `init` reports for one bootstrap token: `not_configured`,
/// `already_present` or `adopted`. Ports `adopt_bootstrap_core_token` and
/// `adopt_bootstrap_agent_token`.
pub fn adopt_bootstrap<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    core: bool,
) -> Result<String, VogtError> {
    // The file is read only when this process's environment names it. A
    // config file on disk can point at a token this process was never given,
    // and adopting that would make `init` depend on whoever shares the host.
    let env_name = if core {
        "VOGT_BOOTSTRAP_CORE_TOKEN_FILE"
    } else {
        "VOGT_BOOTSTRAP_AGENT_TOKEN_FILE"
    };
    let configured = std::env::var_os(env_name).map(std::path::PathBuf::from);
    let (scopes_raw, identity_ref, name) = if core {
        (
            ctx.config.bootstrap_core_token_scopes.clone(),
            ctx.config.bootstrap_core_token_actor.clone(),
            BOOTSTRAP_CORE_TOKEN_NAME,
        )
    } else {
        (
            ctx.config.agent_session_scopes.clone(),
            ctx.config.bootstrap_agent_token_actor.clone(),
            BOOTSTRAP_AGENT_TOKEN_NAME,
        )
    };
    let Some(path) = configured else {
        return Ok("not_configured".to_string());
    };
    let secret = match std::fs::read_to_string(&path) {
        Ok(text) => text.trim().to_string(),
        Err(_) => return Ok("not_configured".to_string()),
    };
    if secret.is_empty() {
        return Ok("not_configured".to_string());
    }
    let scopes = scopes_of(&scopes_raw)?;
    let token_hash = auth::adopt(&secret).map_err(VogtError::InvalidRequest)?;
    {
        let view = ctx.declared.read()?;
        if let Some(present) = view.token_by_hash(&token_hash)? {
            if present.revoked_at.is_none() {
                return Ok("already_present".to_string());
            }
        }
    }
    let reason = if core {
        "adopting the operator-supplied core token at init"
    } else {
        "adopting the operator-supplied agent token at init"
    };
    let mut write = WriteContext::new(
        &ctx.declared,
        &ctx.principal,
        std::sync::Arc::clone(&ctx.clock),
        std::sync::Arc::clone(&ctx.id_factory),
    );
    audited_write(&mut write, TOKEN_ADOPT, reason, |txn, _actor| {
        adopt_body(txn, ctx, core, &token_hash, &scopes, &identity_ref, name)
    })?;
    Ok("adopted".to_string())
}

fn adopt_body<C: Clock + 'static, I: IdFactory + 'static>(
    txn: &mut impl WriteTxn,
    ctx: &AppContext<C, I>,
    core: bool,
    token_hash: &str,
    scopes: &[String],
    identity_ref: &str,
    name: &str,
) -> Result<WriteOutcome<()>, VogtError> {
    let now = ctx.clock.lock().expect("the clock lock").now();
    if let Some(existing) = txn.token_by_hash(token_hash)? {
        if existing.revoked_at.is_none() {
            return Ok(WriteOutcome::new(
                (),
                "token",
                &existing.id,
                serde_json::json!({"adopted": false, "reason": "already present"}),
                TOKEN_ADOPTED_EVENT,
                serde_json::json!({"actor": existing.actor_identity_ref}),
            ));
        }
        let superseded = revoke_named(txn, &existing.actor_id, &existing.name, now)?;
        txn.reinstate_token(&existing.id)?;
        let reinstated = txn
            .token_by_id(&existing.id)?
            .expect("the token was just reinstated");
        return Ok(WriteOutcome::new(
            (),
            "token",
            &reinstated.id,
            serde_json::json!({
                "adopted": true,
                "reason": "reinstated",
                "superseded": superseded,
            }),
            TOKEN_ADOPTED_EVENT,
            serde_json::json!({"actor": reinstated.actor_identity_ref}),
        ));
    }
    let mut holder = txn.actor_by_identity(identity_ref)?;
    if let Some(found) = &holder {
        if found.kind != ActorKind::Agent {
            let which = if core { "core" } else { "agent" };
            let example = if core {
                "agent:vogt-engine"
            } else {
                "agent:pod:<stack>"
            };
            let role = if core {
                "the token the engine presents"
            } else {
                "the token sessions' agents present"
            };
            return Err(VogtError::InvalidRequest(format!(
                "bootstrap_{which}_token_actor {identity_ref:?} is a person ({}); {role} must be \
                 bound to an agent actor (for example {example})",
                found.kind.as_str()
            )));
        }
    } else {
        let created = Actor {
            id: ctx.id_factory.lock().expect("the id lock").next("act"),
            kind: ActorKind::Agent,
            display_name: identity_ref.to_string(),
            identity_ref: identity_ref.to_string(),
            disabled: false,
            created_at: now,
        };
        txn.insert_actor(&created)?;
        holder = Some(created);
    }
    let holder = holder.expect("created above when absent");
    let superseded = revoke_named(txn, &holder.id, name, now)?;
    let token = Token {
        id: ctx.id_factory.lock().expect("the id lock").next("tok"),
        actor_id: holder.id.clone(),
        actor_identity_ref: Some(holder.identity_ref.clone()),
        name: name.to_string(),
        scopes: scopes.to_vec(),
        kind: crate::core::TokenKind::Api,
        created_at: now,
        expires_at: None,
        last_used_at: None,
        revoked_at: None,
        revoked_reason: None,
    };
    let token_id = token.id.clone();
    txn.insert_token(&token, token_hash)?;
    Ok(WriteOutcome::new(
        (),
        "token",
        &token_id,
        serde_json::json!({
            "actor": holder.identity_ref,
            "scopes": scopes,
            "name": name,
            "source": "operator-supplied",
            "superseded": superseded,
        }),
        TOKEN_ADOPTED_EVENT,
        serde_json::json!({"actor": holder.identity_ref, "scopes": scopes}),
    ))
}

fn revoke_named(
    txn: &mut impl WriteTxn,
    actor_id: &str,
    name: &str,
    at: Moment,
) -> Result<Vec<String>, VogtError> {
    let mut revoked = Vec::new();
    for existing in txn.tokens_for_actor(actor_id, true)? {
        if existing.name == name
            && txn.revoke_token(&existing.id, "superseded by a rotated bootstrap secret", at)?
        {
            revoked.push(existing.id);
        }
    }
    Ok(revoked)
}
