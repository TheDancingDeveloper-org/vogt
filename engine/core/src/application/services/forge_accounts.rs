//! Per-actor forge account status and unlinking. Ports the two halves of
//! `application/services/forge_accounts.py` that never touch a token.
//!
//! `forge.account_link` is not here. It validates a pasted PAT against the
//! forge before storing it, and `AppContext` carries no forge transport, so
//! there is nothing to validate against. Status reads cleartext columns and
//! unlink deletes the stored ciphertext; neither needs the forge.

use serde_json::{json, Value};

use crate::application::context::{AppContext, Built};
use crate::application::services::{context_parts, dispatch, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

pub const FORGE_ACCOUNT_UNLINKED_EVENT: &str = "forge.account_unlinked";

pub fn account_status_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, account_status, params)
}

pub fn account_unlink_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, account_unlink, params)
}

/// What the acting actor has linked. A principal who has never written has no
/// actor row, and so no accounts; a read does not create one. The token is
/// never part of the result.
fn account_status<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let host = crate::application::services::preferences::optional_string(&params, "host")?;
    let (declared, _, principal, _, _, _) = context_parts(ctx);
    let view = declared.read()?;
    let actor = view.actor_by_identity(&principal.identity_ref)?;
    let accounts = match actor {
        None => Vec::new(),
        Some(actor) => match host {
            Some(host) => view.forge_account(&actor.id, &host)?.into_iter().collect(),
            None => view.forge_accounts_for_actor(&actor.id)?,
        },
    };
    Ok(json!({
        "accounts": accounts
            .iter()
            .map(|account| json!({
                "host": account.host,
                "login": account.login,
                "scopes": account.scopes,
                "linked": true,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// Remove the stored PAT for (actor, host).
///
/// This deletes Vogt's copy and returns attribution to the file-token fallback.
/// It does not revoke the token upstream; only the forge can, so a caller who
/// wants it dead revokes it there too.
fn account_unlink<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let host = crate::application::services::preferences::require_string(&params, "host")?;
    let reason = crate::application::services::preferences::require_string(&params, "reason")?;
    require_supported_host(ctx, &host)?;
    let (declared, _, principal, clock, ids, _) = context_parts(ctx);
    let mut writing = write_context(declared, principal, clock, ids);
    audited_write(
        &mut writing,
        "forge.account_unlink",
        &reason,
        |txn, actor| {
            let existing = txn.forge_account(&actor.id, &host)?;
            txn.delete_forge_account(&actor.id, &host)?;
            Ok(WriteOutcome::new(
                json!({
                    "host": host,
                    "login": Value::Null,
                    "scopes": "",
                    "linked": false,
                }),
                "forge_account",
                &format!("{}:{host}", actor.id),
                json!({"host": host, "unlinked": true}),
                FORGE_ACCOUNT_UNLINKED_EVENT,
                json!({"host": host, "was_linked": existing.is_some()}),
            ))
        },
    )
}

/// Only a host some registered provider can read may be unlinked. An unknown
/// host is a request error, not a silent no-op.
fn require_supported_host<C, I>(ctx: &AppContext<C, I>, host: &str) -> Result<(), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let (_, _, _, _, _, config) = context_parts(ctx);
    if crate::adapters::forge::registry::supported_hosts(Some(config))
        .iter()
        .any(|known| known == host)
    {
        return Ok(());
    }
    Err(VogtError::InvalidRequest(format!(
        "{host:?} has no provider to validate a token against; configure the host under forge_token_files first"
    )))
}
