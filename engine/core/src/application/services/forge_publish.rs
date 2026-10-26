//! `forge.publish`: create the remote, push, link, migrate. Ports
//! `services/forge_publish.py`.
//!
//! The first verb that creates upstream state and pushes commits. The bounded
//! shape:
//!
//! - It refuses an existing remote. The repository is created fresh under the
//!   acting actor's credential. A name that already exists upstream is the
//!   provider's typed refusal, never a clobber.
//! - It never force-pushes. The push is `push_branch`'s plain push, and a
//!   non-fast-forward rejection is handed back to the person.
//! - It requires clean, explicit local state: a git repository at the project
//!   root, a clean working tree, and a named branch on a commit.
//! - The token is never written down. It reaches git through the askpass
//!   helper, never argv or the checkout's configuration.
//!
//! Ordering is fail-loud: gate, create remote, push, then the declared write
//! that records `repo_url` and links. A failure at a network step leaves the
//! project as it was. The one crash window (remote created, local record lost)
//! is visible: a retry is refused with the repository's name.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::adapters::forge::ForgeProvider;
use crate::adapters::git::{inspect_publish_source, PushRequest};
use crate::application::context::{write_of, AppContext, Built};
use crate::application::resolve;
use crate::application::services::forge_writeback::writer_provider;
use crate::application::services::native_migration::{
    migrate_open_native_items, open_native_items, require_migratable,
};
use crate::application::services::{context_parts, dispatch};
use crate::application::writes::{audited_write, WriteOutcome};
use crate::core::{py_repr, Clock, IdFactory, LinkState, Project};
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ProjectUpdate, ReadView, WriteTxn};

pub const FORGE_PUBLISH: &str = "forge.publish";
pub const FORGE_PUBLISHED_EVENT: &str = "forge.published";

/// Publish resolves its provider before a repository exists to name, so the host
/// is probed with a placeholder URL. Only the host is read. The owner and repo
/// must still parse as valid names. GitHub-only is the same v1 ceiling the whole
/// write path holds.
const HOST_PROBE_URL: &str = "https://github.com/probe/probe";

#[derive(Deserialize)]
struct PublishParams {
    project: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_private")]
    private: bool,
    #[serde(default)]
    description: Option<String>,
    reason: String,
}

/// Private by default: publishing makes a repository, not an announcement.
fn default_private() -> bool {
    true
}

pub fn publish_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, publish_project, params)
}

/// Create a remote repository from a local project and link it.
fn publish_project<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params: PublishParams = serde_json::from_value(params)
        .map_err(|error| VogtError::InvalidRequest(error.to_string()))?;
    let (declared, _, principal, _, _, config) = context_parts(ctx);
    let (project, actor, pending) = {
        let view = declared.read()?;
        let project = resolve::project(&view, &params.project)?;
        let actor = view.actor_by_identity(&principal.identity_ref)?;
        let pending = open_native_items(&view, &project)?;
        (project, actor, pending)
    };

    if project.link_state == LinkState::Linked {
        return Err(VogtError::PublishRefused(format!(
            "{} is already linked to {}; publish creates a repository for a project that has none",
            py_repr(&project.slug),
            project.repo_url.as_deref().unwrap_or("its repository"),
        )));
    }
    if let Some(existing) = &project.repo_url {
        return Err(VogtError::PublishRefused(format!(
            "{} already names a repository ({existing}); publish never overwrites that \
             association — attach to it with `forge link`, or clear the repo_url first if it \
             is wrong",
            py_repr(&project.slug)
        )));
    }

    // The read-only gate before anything exists upstream.
    let source = inspect_publish_source(Path::new(&project.root_path))?;

    let (provider, identity) = writer_provider(ctx, actor.as_ref(), Some(HOST_PROBE_URL))?;
    let Some(provider) = provider else {
        return Err(VogtError::PublishRefused(format!(
            "cannot publish {}: no usable forge credential — link your own forge account \
             (`forge account link`) or configure the instance token file, then retry",
            py_repr(&project.slug)
        )));
    };
    // Same rule as `forge.link`: a policy that would refuse the migration's
    // creates refuses the whole publish.
    require_migratable(&project, &pending)?;

    let name = params.name.clone().unwrap_or_else(|| project.slug.clone());
    // An existing name is the provider's typed refusal (`RemoteRepoExists`).
    // Nothing local has changed at that point.
    let created = provider.create_repo(&name, params.private, params.description.as_deref())?;
    let repo = provider
        .parse(Some(created.web_url.as_str()))
        .ok_or_else(|| {
            VogtError::PublishRefused(format!(
                "the forge created {} but the provider cannot parse its own repository address; \
             nothing local was changed",
                py_repr(&created.web_url)
            ))
        })?;
    // A plain push, authenticated per invocation: the remote is the bare clone
    // URL and the token rides the askpass helper. Never forced.
    let pushed = (ctx.pusher)(&PushRequest {
        root: source.root.clone(),
        remote: provider.clone_url(&repo),
        branch: source.branch.clone(),
        token: provider.clone_token(Some(&repo))?,
        helper_dir: Some(config.resolved_data_dir().join("tmp")),
    })?;

    let previous = project.link_state.to_string();
    let slug = project.slug.clone();
    let project_id = project.id.clone();
    let web_url = created.web_url.clone();
    let branch = source.branch.clone();
    let mut writing = write_of(ctx);
    let published = audited_write(
        &mut writing,
        FORGE_PUBLISH,
        &params.reason,
        |txn, _actor| {
            txn.update_project(
                &project_id,
                &ProjectUpdate {
                    repo_url: Some(web_url.clone()),
                    link_state: Some(LinkState::Linked.to_string()),
                    ..ProjectUpdate::default()
                },
                crate::application::services::now_of(&ctx.clock),
            )?;
            let updated: Project = txn
                .project_by_slug(&slug)?
                .ok_or_else(|| VogtError::NotFound(format!("no project {slug:?}")))?;
            Ok(WriteOutcome::new(
                updated,
                "project",
                &project_id,
                json!({
                    "repo_url": web_url,
                    "link_state": "linked",
                    "branch": branch,
                    "credential": identity,
                }),
                FORGE_PUBLISHED_EVENT,
                json!({
                    "slug": slug,
                    "from": previous,
                    "repo": web_url,
                    "branch": branch,
                    "credential": identity,
                }),
            ))
        },
    )?;

    let mut migrated = Vec::new();
    if !pending.is_empty() {
        migrated = migrate_open_native_items(
            ctx,
            &published,
            &pending,
            &provider,
            &identity,
            &repo,
            &params.reason,
            FORGE_PUBLISH,
        )?;
    }
    Ok(json!({
        "project": published,
        "repo": created.web_url,
        "branch": pushed.branch,
        "revision": pushed.revision,
        "migrated": migrated,
    }))
}
