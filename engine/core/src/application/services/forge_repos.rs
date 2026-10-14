//! `forge.repos`. Ports `application/services/forge_repos.py`.
//!
//! The picker lists the repositories a credential can see and computes
//! `already_registered` against the declared project list. It lists; it never
//! crawls, and picking one is the only way an import ever begins.

use serde_json::{json, Value};

use crate::adapters::forge::registry::{provider_for_host, provider_for_token, supported_hosts};
use crate::adapters::forge::ForgeProvider;
use crate::application::context::{AppContext, Built};
use crate::application::services::{context_parts, dispatch};
use crate::core::{py_repr, Clock, IdFactory};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ReadView};

pub fn repos_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, list_repos, params)
}

/// The repositories the acting credential can see.
///
/// A linked PAT is preferred over the instance file token, so a person sees
/// their own repositories, including private ones their PAT reaches. A host
/// outside the configured set is refused before anything is listed. An empty
/// list carries `detail` when no credential is available, so an empty picker
/// reads as "not collected" rather than "you have no repositories".
fn list_repos<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let host = match params.get("host") {
        None | Some(Value::Null) => "github.com".to_owned(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => {
            return Err(VogtError::InvalidRequest(
                "host must be a string".to_owned(),
            ))
        }
    };
    let (declared, _, principal, _, _, config) = context_parts(ctx);
    if !supported_hosts(Some(config)).contains(&host) {
        return Err(VogtError::InvalidRequest(format!(
            "{} is not a configured forge host; add it to forge_token_files \
             before listing repositories",
            py_repr(&host)
        )));
    }
    let view = declared.read()?;
    let actor = view.actor_by_identity(&principal.identity_ref)?;
    let linked = match &actor {
        Some(actor) => view.forge_account(&actor.id, &host)?,
        None => None,
    };
    let provider = match &linked {
        Some(_) => {
            let ciphertext = actor
                .as_ref()
                .and_then(|actor| view.forge_account_secret(&actor.id, &host).ok().flatten());
            let token = ciphertext.and_then(|stored| {
                crate::adapters::forge::load_cipher(config)
                    .ok()
                    .and_then(|cipher| cipher.decrypt(&stored).ok())
            });
            match token {
                Some(token) => {
                    provider_for_token(&host, &token, Some(config), ctx.forge_transport())
                }
                None => provider_for_host(&host, config, ctx.forge_transport())?,
            }
        }
        None => provider_for_host(&host, config, ctx.forge_transport())?,
    };
    let Some(provider) = provider else {
        return Ok(json!({
            "repos": [],
            "login": Value::Null,
            "detail": "no forge credential is available for this host — neither a \
                       linked account nor the instance file token — so nothing was \
                       listed, which is 'not collected', not 'you have no repositories'",
        }));
    };
    let repos = provider.list_repos()?;
    let registered = registered_keys(&view)?;
    Ok(json!({
        "repos": repos
            .iter()
            .map(|repo| json!({
                "owner": repo.owner,
                "name": repo.name,
                "default_branch": repo.default_branch,
                "visibility": repo.visibility,
                "url": repo.web_url,
                "already_registered": registered.contains(
                    &format!("{host}/{}/{}", repo.owner, repo.name).to_lowercase()
                ),
            }))
            .collect::<Vec<_>>(),
        "login": linked.as_ref().map(|account| &account.login),
        "detail": Value::Null,
    }))
}

/// The declared projects' forge keys, `host/owner/name` lowercased.
///
/// Matching on the web URL would miss a project registered with a trailing
/// `.git` or a different scheme, so the key is the same triple Python uses.
fn registered_keys(view: &impl ReadView) -> Result<std::collections::BTreeSet<String>, VogtError> {
    Ok(view
        .list_projects(10_000, 0)?
        .into_iter()
        .filter_map(|project| project.repo_url)
        .filter_map(|url| forge_key(&url))
        .collect())
}

fn forge_key(url: &str) -> Option<String> {
    let trimmed = url.trim_end_matches('/').trim_end_matches(".git");
    let without_scheme = trimmed.split_once("://").map(|(_, rest)| rest)?;
    let parts: Vec<&str> = without_scheme.split('/').collect();
    if parts.len() < 3 {
        return None;
    }
    Some(format!("{}/{}/{}", parts[0], parts[1], parts[2]).to_lowercase())
}
