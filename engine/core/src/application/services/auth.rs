//! Tokens and the people who hold them. Ports `services/auth.py`.
//!
//! The secret a token carries is returned once, at issue, and never stored. The
//! database keeps the hash, and an audit row that contained the secret would be
//! a credential leak with a timestamp, so the payload records the actor and the
//! scopes and nothing else.

use serde_json::Value;

use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::writes::{audited_write, WriteContext, WriteOutcome};
use crate::auth::{self, TOKEN_ENTROPY_BYTES};
use crate::core::{Actor, Clock, IdFactory, Moment, Token};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

const TOKEN_ISSUE: &str = "token.issue";
const TOKEN_REVOKE: &str = "token.revoke";
const TOKEN_ISSUED_EVENT: &str = "token.issued";
const TOKEN_REVOKED_EVENT: &str = "token.revoked";

/// `token.issue`, as the registry calls it.
pub fn issue_token_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| issue_token(ctx, &params))
}

fn issue_token<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let name = params["name"].as_str().unwrap_or("").to_string();
    let holder_ref = params["actor"].as_str().unwrap_or("").to_string();
    let scopes = auth::parse_scopes(params["scopes"].as_str().unwrap_or(""))
        .map_err(VogtError::InvalidRequest)?
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let expires_in_days = params["expires_in_days"].as_i64();

    let (secret, token_hash) = auth::issue(&rand_bytes());

    let mut writing = write_of(ctx);
    let issued = issue_recorded(
        &mut writing,
        reason,
        &IssueRequest {
            name,
            holder_ref,
            scopes,
            token_hash,
            expires_in_days,
        },
    )?;

    Ok(serde_json::json!({
        "token": issued,
        "secret": secret,
        "warning": "This is the only time the secret is shown. Store it in a file and point \
                    VOGT_TOKEN_FILE at it — never in argv or a URL.",
    }))
}

/// What an issue writes, gathered so the audit closure takes one argument.
struct IssueRequest {
    name: String,
    holder_ref: String,
    scopes: Vec<String>,
    token_hash: String,
    expires_in_days: Option<i64>,
}

/// The audited half of an issue, split out so the closure is built against the
/// concrete store rather than the generic `DeclaredStore` bound. Against the
/// bound the compiler treats the closure as needing a `'static` lifetime, and
/// it does not compile — the same split `record_check` makes.
fn issue_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut WriteContext<'_, C, I, crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>>,
    reason: &str,
    request: &IssueRequest,
) -> Result<Value, VogtError> {
    let name = request.name.clone();
    let holder_ref = request.holder_ref.clone();
    let scopes = request.scopes.clone();
    let token_hash = request.token_hash.clone();
    let expires_in_days = request.expires_in_days;
    let clock = std::sync::Arc::clone(write.clock());
    let ids = std::sync::Arc::clone(write.ids());
    audited_write(
        write,
        TOKEN_ISSUE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            // Drawn here, after `ensure_actor`, so the ids fall in the same order
            // as the Python body, which calls `ctx.id_factory` and `ctx.clock`
            // inside the transaction.
            let now = clock.lock().expect("the clock lock").now();
            let token_id = ids.lock().expect("the id lock").next("tok");
            let expires_at = expires_in_days
                .map(|days| Moment::from_unix(now.unix_seconds() + days * 86_400, 0));
            let holder = resolve::actor(txn, &holder_ref)?;
            let token = Token {
                id: token_id.clone(),
                actor_id: holder.id.clone(),
                actor_identity_ref: Some(holder.identity_ref.clone()),
                name: name.clone(),
                scopes: scopes.iter().map(|scope| (*scope).to_string()).collect(),
                kind: crate::core::TokenKind::Api,
                created_at: now,
                expires_at,
                last_used_at: None,
                revoked_at: None,
                revoked_reason: None,
            };
            txn.insert_token(&token, &token_hash)?;
            let issued = serde_json::to_value(&token).expect("a token serialises");
            Ok(WriteOutcome::new(
                issued,
                "token",
                &token.id,
                serde_json::json!({"actor": holder.identity_ref, "scopes": scopes, "name": name}),
                TOKEN_ISSUED_EVENT,
                serde_json::json!({"actor": holder.identity_ref, "scopes": scopes}),
            ))
        },
    )
}

/// 32 bytes from the system RNG, the same source `fresh_id` draws from.
fn rand_bytes() -> [u8; TOKEN_ENTROPY_BYTES] {
    let mut entropy = [0u8; TOKEN_ENTROPY_BYTES];
    if let Ok(mut source) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut source, &mut entropy);
    }
    entropy
}

/// `token.list`, as the registry calls it. A read: no audit row, no reason.
pub fn list_tokens_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| list_tokens(ctx, &params))
}

fn list_tokens<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let include_revoked = params["include_revoked"].as_bool().unwrap_or(false);
    let limit = params["limit"].as_i64().unwrap_or(100);
    let view = ctx.declared.read()?;
    let tokens = view.list_tokens(include_revoked, limit)?;
    Ok(serde_json::json!({ "tokens": tokens }))
}

/// `token.revoke`, as the registry calls it.
pub fn revoke_token_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    crate::with_ctx!(ctx, |ctx| revoke_token(ctx, &params))
}

fn revoke_token<C: Clock + 'static, I: IdFactory + 'static>(
    ctx: &AppContext<C, I>,
    params: &Value,
) -> Result<Value, VogtError> {
    let reason = params["reason"].as_str().unwrap_or("");
    let id = params["id"].as_str().unwrap_or("").to_string();
    let mut writing = write_of(ctx);
    let revoked = revoke_recorded(&mut writing, reason, &id)?;
    Ok(serde_json::json!({ "token": revoked }))
}

/// The audited half of a revocation, split out for the same reason
/// `issue_recorded` is: the closure must be built against the concrete store.
fn revoke_recorded<C: Clock + 'static, I: IdFactory + 'static>(
    write: &mut WriteContext<'_, C, I, crate::storage::sqlite::declared::SqliteDeclaredStore<C, I>>,
    reason: &str,
    id: &str,
) -> Result<Value, VogtError> {
    let id = id.to_string();
    let clock = std::sync::Arc::clone(write.clock());
    audited_write(
        write,
        TOKEN_REVOKE,
        reason,
        move |txn: &mut _, _actor: &Actor| {
            let existing = txn.token_by_id(&id)?;
            if existing.is_none() {
                return Err(VogtError::NotFound(format!("no token '{id}'")));
            }
            let now = clock.lock().expect("the clock lock").now();
            if !txn.revoke_token(&id, reason, now)? {
                return Err(VogtError::Conflict(format!(
                    "token '{id}' is already revoked"
                )));
            }
            let updated = txn
                .token_by_id(&id)?
                .expect("the token was just written in this transaction");
            let payload = serde_json::to_value(&updated).expect("a token serialises");
            Ok(WriteOutcome::new(
                payload.clone(),
                "token",
                &id,
                payload,
                TOKEN_REVOKED_EVENT,
                serde_json::json!({ "actor": updated.actor_identity_ref }),
            ))
        },
    )
}
