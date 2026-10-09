//! Turning what callers type into what the store holds.
//! Ports `src/vogt/application/services/_resolve.py`.
//!
//! Callers name things the way humans and agents do — `WI-7`, a project slug,
//! an actor's `identity_ref`. Everything resolves here so that a mistyped
//! reference produces "no work item WI-70" rather than a foreign-key error
//! surfacing from three layers down.
//!
//! `work_item` resolves declared rows only. Every operation that must also
//! accept an upstream subject key resolves through the upstream service, which
//! tries this first and then the observed mirror. The two cannot disagree: a
//! `WI-` ref never contains `:`, so the namespaces are disjoint.

use crate::core::{py_repr, Actor, Initiative, Project, WorkItem};
use crate::errors::VogtError;
use crate::storage::interface::ReadView;

pub fn project(view: &impl ReadView, slug: &str) -> Result<Project, VogtError> {
    match view.project_by_slug(slug)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no project with slug {}",
            py_repr(slug)
        ))),
    }
}

pub fn work_item(view: &impl ReadView, reference: &str) -> Result<WorkItem, VogtError> {
    match view.work_item_by_ref(reference)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no work item {}",
            py_repr(reference)
        ))),
    }
}

pub fn actor(view: &impl ReadView, identity_ref: &str) -> Result<Actor, VogtError> {
    match view.actor_by_identity(identity_ref)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no actor with identity {} — create it with `actor create` first",
            py_repr(identity_ref)
        ))),
    }
}

pub fn initiative(view: &impl ReadView, slug: &str) -> Result<Initiative, VogtError> {
    match view.initiative_by_slug(slug)? {
        Some(found) => Ok(found),
        None => Err(VogtError::NotFound(format!(
            "no initiative with slug {}",
            py_repr(slug)
        ))),
    }
}

pub fn label_exists(view: &impl ReadView, name: &str) -> Result<(), VogtError> {
    if view.label_by_name(name)?.is_none() {
        return Err(VogtError::NotFound(format!(
            "no label named {} — create it with `label create` first",
            py_repr(name)
        )));
    }
    Ok(())
}
