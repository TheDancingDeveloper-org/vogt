//! `forge.link`: the explicit act that makes a project upstream-truth. Ports
//! `services/forge_link.py`.
//!
//! Linking is a decision, not an inference. What it validates is exactly what
//! write-through needs: a `repo_url` a registered provider matches, and a usable
//! credential for it, resolved the way the write path resolves one. Linking also
//! migrates every open native work item the project still holds (see
//! `native_migration`). The policy gate runs before the link commits, so no
//! project ends up linked but unmigratable. Re-linking is idempotent and resumes
//! a partial migration.
//!
//! There is deliberately no `forge.unlink`: withdrawing the upstream-truth model
//! is a migration question, not a toggle.

use serde::Deserialize;
use serde_json::{json, Value};

use crate::adapters::forge::registry::unsupported_reason;
use crate::adapters::forge::ForgeProvider;
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

pub const FORGE_LINK: &str = "forge.link";
pub const FORGE_LINKED_EVENT: &str = "forge.linked";

#[derive(Deserialize)]
struct LinkParams {
    project: String,
    reason: String,
}

pub fn link_op(built: &Built, params: Value) -> Result<Value, VogtError> {
    dispatch!(built, link_project, params)
}

/// Set an already-registered project `linked`, with its preconditions named.
fn link_project<C, I>(ctx: &AppContext<C, I>, params: Value) -> Result<Value, VogtError>
where
    C: Clock + 'static,
    I: IdFactory + 'static,
{
    let params: LinkParams = serde_json::from_value(params)
        .map_err(|error| VogtError::InvalidRequest(error.to_string()))?;
    let (declared, _, principal, _, _, config) = context_parts(ctx);
    let (project, actor, pending) = {
        let view = declared.read()?;
        let project = resolve::project(&view, &params.project)?;
        let actor = view.actor_by_identity(&principal.identity_ref)?;
        let pending = open_native_items(&view, &project)?;
        (project, actor, pending)
    };

    // The config travels with the question: a Forgejo host is supported exactly
    // when `[forge_token_files]` names it.
    if let Some(unsupported) = unsupported_reason(project.repo_url.as_deref(), Some(config)) {
        return Err(VogtError::LinkRefused(format!(
            "cannot link {}: {unsupported}. Set a supported repo_url with `project update`, \
             or import the repository through `project import`.",
            py_repr(&project.slug)
        )));
    }
    let repo_url = project.repo_url.clone();
    let (provider, identity) = writer_provider(ctx, actor.as_ref(), repo_url.as_deref())?;
    let Some(provider) = provider else {
        return Err(VogtError::LinkRefused(format!(
            "cannot link {}: no usable credential for {} — link your own forge account \
             (`forge account link`) or configure the instance token file, then retry.",
            py_repr(&project.slug),
            repo_url.as_deref().unwrap_or("this project")
        )));
    };
    // Before the link commits, not after: a policy that refuses the migration's
    // creates must refuse the whole act, or the project ends up linked with
    // native items no re-run could ever move.
    require_migratable(&project, &pending)?;

    let previous = project.link_state.to_string();
    let slug = project.slug.clone();
    let project_id = project.id.clone();
    let mut writing = write_of(ctx);
    let linked = audited_write(&mut writing, FORGE_LINK, &params.reason, |txn, _actor| {
        txn.update_project(
            &project_id,
            &ProjectUpdate {
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
            json!({"link_state": "linked", "credential": identity}),
            FORGE_LINKED_EVENT,
            json!({"slug": slug, "from": previous, "to": "linked", "credential": identity}),
        ))
    })?;

    let mut migrated = Vec::new();
    if !pending.is_empty() {
        let repo = provider.parse(linked.repo_url.as_deref()).ok_or_else(|| {
            VogtError::InvalidRequest(format!(
                "{:?} names no repository a provider can parse",
                linked.slug
            ))
        })?;
        migrated = migrate_open_native_items(
            ctx,
            &linked,
            &pending,
            &provider,
            &identity,
            &repo,
            &params.reason,
            FORGE_LINK,
        )?;
    }
    Ok(json!({"project": linked, "migrated": migrated}))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::link_op;
    use crate::application::services::forge_ops_test::{insert_project, with_token_on};
    use crate::application::services::services_test::opened;
    use crate::errors::VogtError;

    #[test]
    fn linking_a_project_with_a_resolvable_credential_marks_it_linked() {
        let built = with_token_on(&crate::application::forge_standin::StandInForge::one_of_each());
        insert_project(
            &built,
            "widget",
            Some("https://github.com/acme/widget"),
            "/tmp",
            false,
        );
        let linked = link_op(
            &built,
            json!({"project": "widget", "reason": "make it upstream-truth"}),
        )
        .unwrap();
        assert_eq!(linked["project"]["link_state"], "linked", "{linked}");
        assert_eq!(linked["migrated"], json!([]));
    }

    #[test]
    fn linking_an_unknown_project_is_a_miss() {
        let built = opened();
        let refused = link_op(&built, json!({"project": "nope", "reason": "link it"}));
        assert!(
            matches!(refused, Err(VogtError::NotFound(_))),
            "{refused:?}"
        );
    }

    #[test]
    fn linking_without_a_credential_is_refused_before_anything_changes() {
        let built = opened();
        insert_project(
            &built,
            "widget",
            Some("https://github.com/acme/widget"),
            "/tmp",
            false,
        );
        match link_op(&built, json!({"project": "widget", "reason": "link it"})) {
            Err(VogtError::LinkRefused(message)) => {
                assert!(message.contains("no usable credential"), "{message}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
