//! `forge.import`: turn a picker-listed configured-forge repository into a
//! project. Ports `services/forge_import.py`.
//!
//! `forge.repos` lists the repositories a credential can see, but nothing turned
//! one of those rows into a project. `forge.import` takes a repository named the
//! way `forge.repos` names it (`owner` and `name`), clones it under the same
//! credential the listing used, registers it, and consolidates it. It composes
//! with `project.import`'s clone-register-consolidate core rather than repeating
//! it. Its only addition is resolving the acting actor's linked credential the
//! way the picker does, which matters for a private repository.

use serde::Deserialize;
use serde_json::Value;

use crate::adapters::forge::registry::{identity_for, supported_hosts};
use crate::adapters::forge::RepoRef;
use crate::application::context::{AppContext, Built};
use crate::application::projects::{import_repository, ImportRequest};
use crate::application::services::dispatch;
use crate::application::services::forge_repos::reader_provider;
use crate::core::{Clock, IdFactory};
use crate::errors::VogtError;

pub const FORGE_IMPORT: &str = "forge.import";
pub const FORGE_IMPORTED_EVENT: &str = "forge.imported";

#[derive(Deserialize)]
struct ForgeImportParams {
    owner: String,
    name: String,
    #[serde(default = "default_host")]
    host: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    root_path: Option<String>,
    #[serde(default = "default_lifecycle")]
    lifecycle_state: String,
    #[serde(default = "default_consolidate")]
    consolidate: bool,
    reason: String,
}

fn default_host() -> String {
    "github.com".to_owned()
}

fn default_lifecycle() -> String {
    "active".to_owned()
}

fn default_consolidate() -> bool {
    true
}

pub fn import_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, import_forge_repo, params)
}

/// Clone, register and consolidate a repository the picker listed.
fn import_forge_repo<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params: ForgeImportParams = serde_json::from_value(params)
        .map_err(|error| VogtError::InvalidRequest(error.to_string()))?;
    let config = &ctx.config;
    if !supported_hosts(Some(config)).contains(&params.host) {
        return Err(VogtError::InvalidRequest(format!(
            "{:?} is not a configured forge host; add it to forge_token_files before importing \
             by host",
            params.host
        )));
    }

    // The same credential resolution the picker uses: the acting actor's linked
    // PAT when they have one, and the file token otherwise. `None` means neither
    // is configured, so there is nothing to clone the repository with.
    let (provider, _login) = reader_provider(ctx, &params.host)?;
    if provider.is_none() {
        return Err(VogtError::InvalidRequest(format!(
            "no forge credential is available for this host — neither a linked account \
             (`forge account link`) nor the instance file token — so there is nothing to clone \
             {}/{}/{} with",
            params.host, params.owner, params.name
        )));
    }
    let repo = RepoRef {
        host: params.host.clone(),
        owner: params.owner.clone(),
        repo: params.name.clone(),
    };
    if identity_for(
        Some(&format!(
            "https://{}/{}/{}",
            params.host, params.owner, params.name
        )),
        config,
        ctx.forge_transport(),
    )
    .is_none()
    {
        return Err(VogtError::InvalidRequest(format!(
            "could not resolve {}/{}/{}",
            params.host, params.owner, params.name
        )));
    }
    import_repository(
        ctx,
        &repo,
        provider,
        ImportRequest {
            name: params.display_name,
            root_path: params.root_path,
            lifecycle_state: params.lifecycle_state,
            consolidate: params.consolidate,
            reason: &params.reason,
            operation: FORGE_IMPORT,
            event_kind: FORGE_IMPORTED_EVENT,
        },
    )
}
