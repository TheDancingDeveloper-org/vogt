//! Per-actor forge account linking, status and unlinking. Ports
//! `application/services/forge_accounts.py`.
//!
//! Linking validates the pasted PAT against the forge before anything is
//! stored, through the context's forge transport. The token is encrypted at
//! rest and never echoed; status and unlink never read it back out.

use serde_json::{json, Value};

use crate::adapters::forge::ForgeProvider;
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::{context_parts, dispatch, now_of, write_context};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView, WriteTxn};

pub const FORGE_ACCOUNT_LINKED_EVENT: &str = "forge.account_linked";
pub const FORGE_ACCOUNT_UNLINKED_EVENT: &str = "forge.account_unlinked";

pub fn account_link_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, account_link, params)
}

pub fn account_status_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, account_status, params)
}

pub fn account_unlink_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, account_unlink, params)
}

/// Validate a pasted PAT against the forge, then store it encrypted.
///
/// The token is the caller's, not a file's, so the provider is built from it
/// directly. A host with no provider, or a token the forge refuses, stores
/// nothing. The result carries the login and the scopes, never the token.
fn account_link<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    // ForgeAccountLinkParams.host defaults to github.com. Only a missing
    // value takes the default; "" still reaches the host check.
    let host = match params.get("host") {
        None | Some(Value::Null) => "github.com".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "host must be a string".to_owned(),
            ))
        }
    };
    let token = present_string(&params, "token")?;
    let reason = present_string(&params, "reason")?;
    let (_, _, _, _, _, config) = context_parts(ctx);
    // The host is checked first, then the cipher, as forge_accounts.py does:
    // an unsupported host is refused before the key is read, and an
    // unconfigured instance refuses before a pasted token reaches the forge.
    require_supported_host(ctx, &host)?;
    let cipher = crate::adapters::forge::load_cipher(config)?;
    let provider = crate::adapters::forge::registry::provider_for_token(
        &host,
        &token,
        Some(config),
        ctx.forge_transport(),
    )
    .ok_or_else(|| {
        VogtError::InvalidRequest(
            "the forge did not accept this token; it may be invalid, expired, \
             or lack the scope to read its own identity"
                .to_owned(),
        )
    })?;
    // A transport error from identity() stays a GitHubUnavailable, which is
    // what the client raises when the forge is unreachable.
    let (login, scopes) = provider.identity()?.ok_or_else(|| {
        VogtError::InvalidRequest(
            "the forge did not accept this token; it did not return an identity".to_owned(),
        )
    })?;
    let encrypted = cipher.encrypt(
        &token,
        now_of(&ctx.clock).unix_seconds().try_into().unwrap_or(0),
    );
    let mut writing = write_of(ctx);
    audited_write(&mut writing, "forge.account_link", &reason, |txn, actor| {
        txn.upsert_forge_account(
            &actor.id,
            &host,
            &login,
            &scopes,
            &encrypted,
            now_of(&ctx.clock),
        )?;
        Ok(WriteOutcome::new(
            json!({
                "host": host,
                "login": login,
                "scopes": scopes,
                "linked": true,
            }),
            "forge_account",
            &format!("{}:{host}", actor.id),
            json!({"host": host, "login": login}),
            FORGE_ACCOUNT_LINKED_EVENT,
            json!({"host": host, "login": login}),
        ))
    })
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
    let host = present_string(&params, "host")?;
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
        "{} has no provider to validate a token against; \
         configure the host under forge_token_files first",
        crate::core::py_repr(host)
    )))
}

/// A host that is present, even when blank. Python's `str` field treats only a
/// missing value as absent; `""` reaches the host check and is refused there,
/// so the shared `require_string` (which rejects blank text) would diverge.
fn present_string(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Err(VogtError::InvalidRequest(format!("{field} is required"))),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{account_link_op, account_status_op, account_unlink_op};
    use crate::application::services::services_test::{moment, opened, principal, unique};
    use crate::errors::VogtError;
    use crate::storage::interface::{DeclaredStore, ObservedStore};
    use serde_json::json;

    /// Linking without a key file is a typed not-configured error and stores nothing.
    #[test]
    fn linking_without_a_key_file_is_not_configured() {
        let built = opened();
        let refused = account_link_op(
            &built,
            json!({"token": "ghp_not_stored", "reason": "link me"}),
        );
        assert!(
            matches!(refused, Err(VogtError::ForgeAccountsNotConfigured(_))),
            "{refused:?}"
        );
        let status = account_status_op(&built, json!({})).unwrap();
        assert!(!status.to_string().contains("ghp_not_stored"));
        assert_eq!(status["accounts"].as_array().unwrap().len(), 0);
    }

    /// An unconfigured host is refused before the key is read and before the token is sent.
    #[test]
    fn an_unconfigured_host_is_rejected() {
        let built = keyed();
        let refused = account_link_op(
            &built,
            json!({
                "host": "no.such.host",
                "token": "ghp_not_stored",
                "reason": "link me",
            }),
        );
        match &refused {
            Err(VogtError::InvalidRequest(message)) => {
                assert!(message.contains("no.such.host"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    /// Unlinking a host the actor never linked is recorded as not having been linked,
    /// and neither status nor the unlink answer carries a token.
    #[test]
    fn unlinking_nothing_says_it_was_not_linked() {
        let built = opened();
        let status = account_status_op(&built, json!({})).unwrap();
        assert!(!status.to_string().contains("ghp_"));
        let gone =
            account_unlink_op(&built, json!({"host": "github.com", "reason": "gone"})).unwrap();
        assert_eq!(gone["linked"], false, "{gone}");
        assert!(gone["login"].is_null());
        assert!(!gone.to_string().contains("ghp_"));
    }

    fn keyed() -> crate::application::context::Built {
        use crate::application::context::{build_context, Built};
        use crate::core::{SequentialIds, StepClock};
        let dir =
            std::env::temp_dir().join(format!("vogt-acct-{}-{}", std::process::id(), unique()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key_file = dir.join("account.key");
        std::fs::write(&key_file, "MDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDAwMDA=").unwrap();
        let built = build_context(
            crate::config::VogtConfig {
                data_dir: dir,
                forge_account_key_file: Some(key_file),
                ..crate::config::VogtConfig::default()
            },
            Some(principal()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            panic!("expected a step clock");
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal()).unwrap();
        ctx.observed.migrate().unwrap();
        built
    }
}
