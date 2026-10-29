//! `forge.writeback` and `forge.actions`. Ports `writeback.py`'s
//! `set_write_back` and `list_write_backs`.
//!
//! The policy is a fact a person sets; nothing enforces it. The consequence
//! half — the part that posts upstream — lives in `adapters/forge/writeback.rs`
//! and is not this operation.

use serde_json::{json, Value};

use crate::adapters::forge::registry::{
    identity_for, provider_for, provider_for_token, ForgeClient,
};
use crate::application::context::{write_of, AppContext, Built};
use crate::application::services::{context_parts, dispatch, now_of};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{Clock, IdFactory, WriteBack};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ProjectUpdate, ReadView, WriteTxn};

pub const FORGE_WRITEBACK_SET_EVENT: &str = "project.writeback_set";

pub fn writeback_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, set_write_back, params)
}

pub fn writeback_list_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_write_backs, params)
}

/// Set a project's write-back policy. An unlinked project may take a policy: it
/// is stored and has nothing to do until a later link, as Python's
/// `set_write_back` does.
fn set_write_back<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let slug = present_string(&params, "project")?;
    let policy = present_string(&params, "policy")?;
    let reason = present_string(&params, "reason")?;
    policy
        .parse::<WriteBack>()
        .map_err(|_| VogtError::InvalidRequest(format!("write-back policy {policy:?}")))?;
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let project = resolve_project(&declared.read()?, &slug)?;
    // Python's set_write_back does not require the project to be linked.
    // A policy on an unlinked project is stored and simply has nothing to do
    // until a later link; refusing here made the parity step exit 1 where
    // Python exits 0.
    let previous = project.write_back.to_string();
    let project_slug = project.slug.clone();
    let id = project.id.clone();
    let mut writing = write_of(ctx);
    // The audit operation is Python's `WRITEBACK_SET`, which the parity audit
    // trail compares by name.
    let updated = audited_write(&mut writing, "project.writeback", &reason, |txn, _actor| {
        txn.update_project(
            &id,
            &ProjectUpdate {
                write_back: Some(policy.clone()),
                ..ProjectUpdate::default()
            },
            now_of(&ctx.clock),
        )?;
        let updated = txn
            .project_by_slug(&slug)?
            .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))?;
        Ok(WriteOutcome::new(
            json!({"project": updated}),
            "project",
            &id,
            json!({"write_back": policy}),
            FORGE_WRITEBACK_SET_EVENT,
            json!({"slug": project_slug, "from": previous, "to": policy}),
        ))
    })?;
    Ok(updated)
}

/// What write-back has done, newest first. A read, not a mutation: the
/// record is the evidence, and reading it changes nothing.
fn list_write_backs<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let outcome = optional_string(&params, "outcome")?;
    if let Some(outcome) = &outcome {
        if !["attempted", "succeeded", "failed", "skipped"].contains(&outcome.as_str()) {
            return Err(VogtError::InvalidRequest(format!(
                "write-back outcome {outcome:?}"
            )));
        }
    }
    let limit = match params.get("limit") {
        None | Some(Value::Null) => 100,
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| VogtError::InvalidRequest("limit must be an integer".to_owned()))?,
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "limit must be a number".to_owned(),
            ))
        }
    };
    // WriteBackListParams.limit is ge=1, le=500. The bounds live in the model
    // in Python; here the service enforces them so an out-of-range page is
    // the same refusal.
    if !(1..=500).contains(&limit) {
        return Err(VogtError::InvalidRequest(format!(
            "limit must be between 1 and 500, not {limit}"
        )));
    }
    let (declared, _, _, _, _, _) = context_parts(ctx);
    let records = declared
        .read()?
        .list_writeback_actions(outcome.as_deref(), limit)?;
    Ok(json!({
        "actions": records
            .iter()
            .map(|record| json!({
                "id": record.id,
                "at": record.at,
                "action": record.action,
                "subject_key": record.subject_key,
                "policy": record.policy,
                "outcome": record.outcome,
                "reason": record.reason,
                "detail": record.detail,
                "source_url": record.source_url,
            }))
            .collect::<Vec<_>>(),
    }))
}

/// The ledger identity for a write made under the instance file token.
pub(crate) const FILE_TOKEN_IDENTITY: &str = "instance file token";

/// The provider a write lands under, and whose identity it is. Ports
/// `writeback.py`'s `_writer_provider`.
///
/// Prefers the acting actor's linked PAT for the target host, which works even
/// when the instance has no file token. Falls back to the file-token provider,
/// which is `None` when neither is configured. The actor lookup is skipped when
/// linking is not configured, so an instance with no key pays nothing. `actor`
/// is `None` for a principal with no actor row yet, who has no linked PAT.
pub(crate) fn writer_provider<C, I>(
    ctx: &AppContext<C, I>,
    actor: Option<&crate::core::Actor>,
    repo_url: Option<&str>,
) -> Result<(Option<ForgeClient>, String), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let config = &ctx.config;
    let host = identity_for(repo_url, config, None).map(|(_, repo)| repo.host);
    if let (Some(actor), Some(host)) = (actor, host) {
        if crate::adapters::forge::account_linking_enabled(config) {
            let view = ctx.declared.read()?;
            let secret = view.forge_account_secret(&actor.id, &host)?;
            let account = view.forge_account(&actor.id, &host)?;
            if let (Some(secret), Some(account)) = (secret, account) {
                let pat = crate::adapters::forge::load_cipher(config)?.decrypt(&secret)?;
                if let Some(provider) =
                    provider_for_token(&host, &pat, Some(config), ctx.forge_transport())
                {
                    return Ok((Some(provider), account.login));
                }
            }
        }
    }
    let file_provider = provider_for(repo_url, config, ctx.forge_transport())?;
    Ok((file_provider, FILE_TOKEN_IDENTITY.to_owned()))
}

fn resolve_project(view: &impl ReadView, slug: &str) -> Result<crate::core::Project, VogtError> {
    crate::application::resolve::project(view, slug)
}

fn present_string(params: &Value, field: &str) -> Result<String, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Err(VogtError::InvalidRequest(format!("{field} is required"))),
        Some(Value::String(text)) if text.trim().is_empty() => {
            Err(VogtError::InvalidRequest(format!("{field} is required")))
        }
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

fn optional_string(params: &Value, field: &str) -> Result<Option<String>, VogtError> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(VogtError::InvalidRequest(format!(
            "{field} must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::{writeback_list_op, writeback_op};
    use crate::application::services::forge_ops_test::insert_project;
    use crate::application::services::services_test::opened;
    use crate::errors::VogtError;
    use serde_json::json;

    /// A policy is stored on the project and the answer names the new value.
    #[test]
    fn writeback_sets_the_policy() {
        let built = opened();
        insert_project(&built, "widget", None, "/tmp", false);
        let set = writeback_op(
            &built,
            json!({"project": "widget", "policy": "comment_only", "reason": "arm it"}),
        )
        .unwrap();
        assert_eq!(set["project"]["write_back"], "comment_only");
    }

    /// An unknown policy and an unknown project are typed refusals, not a stored change.
    #[test]
    fn writeback_refuses_an_unknown_policy_and_an_unknown_project() {
        let built = opened();
        let policy = writeback_op(
            &built,
            json!({"project": "widget", "policy": "sometimes", "reason": "no"}),
        );
        assert!(
            matches!(policy, Err(VogtError::InvalidRequest(_))),
            "{policy:?}"
        );
        insert_project(&built, "widget", None, "/tmp", false);
        let missing = writeback_op(
            &built,
            json!({"project": "nope", "policy": "none", "reason": "no"}),
        );
        assert!(
            matches!(missing, Err(VogtError::NotFound(_))),
            "{missing:?}"
        );
    }

    /// The ledger is a read. An outcome outside the four recorded ones is refused,
    /// and a page that is in range comes back empty when nothing has written.
    #[test]
    fn writeback_list_refuses_an_unknown_outcome_and_reads_an_empty_ledger() {
        let built = opened();
        let bad = writeback_list_op(&built, json!({"outcome": "maybe"}));
        assert!(matches!(bad, Err(VogtError::InvalidRequest(_))), "{bad:?}");
        let listed = writeback_list_op(&built, json!({})).unwrap();
        assert_eq!(listed["actions"].as_array().unwrap().len(), 0);
    }
}
