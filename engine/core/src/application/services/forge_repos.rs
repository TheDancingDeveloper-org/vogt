//! `forge.repos`. Ports `application/services/forge_repos.py`.
//!
//! The picker lists the repositories a credential can see and computes
//! `already_registered` against the declared project list. It lists; it never
//! crawls, and picking one is the only way an import ever begins.

use serde_json::{json, Value};

use crate::adapters::forge::registry::ForgeClient;
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
    let (declared, _, _, _, _, config) = context_parts(ctx);
    if !supported_hosts(Some(config)).contains(&host) {
        return Err(VogtError::InvalidRequest(format!(
            "{} is not a configured forge host; add it to forge_token_files \
             before listing repositories",
            py_repr(&host)
        )));
    }
    // Linking that is not configured pays nothing: no actor lookup, and the
    // file token is the whole answer. A configured cipher that cannot decrypt
    // the stored PAT raises, as Python's `_reader_provider` does — falling
    // back to the instance token while reporting the linked login would list
    // one person's repositories under another's name.
    let view = declared.read()?;
    let (provider, linked_login) = reader_provider(ctx, &host)?;
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
    let registered = registered_keys(&view, config)?;
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
        "login": linked_login,
        "detail": Value::Null,
    }))
}

/// The provider a read lists or imports through, and whose login it is. Ports
/// `forge_repos.py`'s `_reader_provider`.
///
/// Prefers the acting actor's linked PAT for the host, which reaches their
/// private repositories even with no instance file token. A PAT that cannot be
/// decrypted is an error, never a silent fallback, because falling back would
/// list one person's repositories under another's name. The file-token provider
/// is the answer otherwise, and `None` means neither is configured.
pub(crate) fn reader_provider<C, I>(
    ctx: &AppContext<C, I>,
    host: &str,
) -> Result<(Option<ForgeClient>, Option<String>), VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let config = &ctx.config;
    if crate::adapters::forge::account_linking_enabled(config) {
        let view = ctx.declared.read()?;
        if let Some(actor) = view.actor_by_identity(&ctx.principal.identity_ref)? {
            let secret = view.forge_account_secret(&actor.id, host)?;
            let account = view.forge_account(&actor.id, host)?;
            if let (Some(secret), Some(account)) = (secret, account) {
                let pat = crate::adapters::forge::load_cipher(config)?.decrypt(&secret)?;
                if let Some(provider) =
                    provider_for_token(host, &pat, Some(config), ctx.forge_transport())
                {
                    return Ok((Some(provider), Some(account.login)));
                }
            }
        }
    }
    let file_provider = provider_for_host(host, config, ctx.forge_transport())?;
    Ok((file_provider, None))
}

/// The declared projects' forge keys, `host/owner/name` lowercased.
///
/// Matching on the web URL would miss a project registered with a trailing
/// `.git` or a different scheme, so the key is the same triple Python uses.
fn registered_keys(
    view: &impl ReadView,
    config: &crate::config::VogtConfig,
) -> Result<std::collections::BTreeSet<String>, VogtError> {
    // Python's `_registered_keys` parses each URL through `identity_for`, so
    // the ssh and scp spellings of one repository collapse to the same key
    // as its https URL.
    Ok(view
        .list_projects(100_000, 0)?
        .into_iter()
        .filter_map(|project| project.repo_url)
        .filter_map(|url| {
            crate::adapters::forge::registry::identity_for(Some(&url), config, None).map(
                |(_, repo)| format!("{}/{}/{}", repo.host, repo.owner, repo.repo).to_lowercase(),
            )
        })
        .collect())
}
