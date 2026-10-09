//! Upstream-truth work items: the observed mirror joined to the overlay. Ports
//! `services/upstream.py`.
//!
//! On a linked project the work items *are* the project's observed forge
//! issues: the mirror the sync collectors maintain is the truth for title,
//! labels and open or closed, and the `work_overlay` table carries the
//! vogt-local half — a workflow state richer than open or closed, priority,
//! effort, assignee, initiative. This is the one place that joins the two into
//! a `WorkItem`, so `work.get`, `work.list`, the backlog and the board read the
//! same item.
//!
//! The subject key is the ref. An upstream item's `ref` and `id` are both its
//! forge subject key, because the item has no `wrk_*` row to own a `WI-n`
//! handle and a parallel numbering would mean two names for one thing.

use crate::application::context::AppContext;
use crate::application::views::trust_for;
use crate::core::{
    Clock, IdFactory, LinkState, Moment, Observation, Origin, Priority, Project, WorkItem,
    WorkKind, WorkOverlay, TERMINAL_STATES,
};
use crate::errors::VogtError;
use crate::storage::interface::{ObservedStore, ReadView, WorkFilter};

/// The observed kinds an upstream-truth work item can be assembled from. Issues
/// are the work plane; pull requests stay observed-only candidates in the
/// backlog, because nobody creates a PR through `work.create`.
const UPSTREAM_ITEM_KINDS: &[&str] = &["forge.issue"];

/// Whether this project's work model is the forge's. Reads the persisted
/// `link_state` and nothing else: linking is an explicit act, never an
/// inference from which tokens resolve this second.
pub fn is_linked(project: &Project) -> bool {
    project.link_state == LinkState::Linked
}

/// One upstream-truth work item, assembled the only way there is. Kind and
/// priority reuse the observed-side classification, refined by the overlay
/// where somebody said otherwise. The body is empty because the mirror does not
/// carry issue bodies.
pub fn build_item<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    observation: &Observation,
    overlay: Option<&WorkOverlay>,
    project: &Project,
    workflow: &crate::core::Workflow,
    confirmed_at: Option<Moment>,
) -> WorkItem {
    let title = observation
        .payload
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let title: String = if title.is_empty() {
        observation.subject_key.clone()
    } else {
        title.chars().take(300).collect()
    };
    let updated_at = observation
        .payload
        .get("updated_at")
        .and_then(serde_json::Value::as_str)
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| crate::core::from_iso(raw).ok())
        .unwrap_or(observation.observed_at);
    let labels = observation
        .payload
        .get("labels")
        .and_then(serde_json::Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .map(|label| match label.as_str() {
                    Some(text) => text.to_string(),
                    None => label.to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    let priority = overlay
        .and_then(|overlay| overlay.priority)
        .map(|priority| priority.to_string())
        .unwrap_or_else(|| crate::decisions::priority_of(observation).to_string());
    WorkItem {
        id: observation.subject_key.clone(),
        reference: observation.subject_key.clone(),
        kind: parse_wire(
            crate::decisions::work_kind_of(observation),
            WorkKind::Feature,
        ),
        title,
        body: String::new(),
        state: crate::decisions::upstream_state(observation, overlay, &workflow.initial_state),
        priority: parse_wire(&priority, Priority::P2),
        effort: overlay.and_then(|overlay| overlay.effort),
        project_id: Some(project.id.clone()),
        project_slug: Some(project.slug.clone()),
        initiative_id: overlay.and_then(|overlay| overlay.initiative_id.clone()),
        origin: Origin::Observed,
        trust_state: parse_wire(
            &trust_for(ctx, observation.observed_at, confirmed_at),
            crate::core::TrustState::Unverified,
        ),
        assignee_actor_id: overlay.and_then(|overlay| overlay.assignee_actor_id.clone()),
        assignee_identity_ref: None,
        labels,
        relations: Vec::new(),
        superseded_by: None,
        created_at: observation.observed_at,
        updated_at,
    }
}

/// Every upstream-truth item of one linked project, unfiltered. Subjects a
/// `work_link` already adopted into a declared row are excluded — that row is
/// the item, and emitting both would double-count the work.
pub fn upstream_items<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    project: &Project,
    include_closed: bool,
    limit: i64,
) -> Result<Vec<WorkItem>, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(Vec::new());
    }
    let observations = ctx.observed.latest(
        &[UPSTREAM_ITEM_KINDS[0].to_string()],
        Some(&project.id),
        false,
        !include_closed,
        limit,
    )?;
    if observations.is_empty() {
        return Ok(Vec::new());
    }
    let keys: Vec<String> = observations
        .iter()
        .map(|observation| observation.subject_key.clone())
        .collect();
    let adopted = view.work_links_for_subjects(&keys)?;
    let overlays = view.work_overlays(&keys)?;
    let confirmed = ctx.observed.last_confirmed(&keys)?;
    let mut items = Vec::new();
    for observation in observations
        .iter()
        .filter(|observation| !adopted.contains_key(&observation.subject_key))
    {
        let workflow = view.workflow_for(crate::decisions::work_kind_of(observation))?;
        items.push(build_item(
            ctx,
            observation,
            overlays.get(&observation.subject_key),
            project,
            &workflow,
            confirmed.get(&observation.subject_key).copied(),
        ));
    }
    items.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then(left.reference.cmp(&right.reference))
    });
    Ok(items)
}

/// Apply a `WorkFilter` to an assembled item the way the SQL narrowing does.
/// `limit` and `offset` are paging, not narrowing, and callers apply them after
/// any merge.
pub fn matches(item: &WorkItem, filter: &WorkFilter) -> bool {
    if filter
        .project_id
        .as_ref()
        .is_some_and(|project| item.project_id.as_ref() != Some(project))
    {
        return false;
    }
    if !filter.kinds.is_empty()
        && !filter
            .kinds
            .iter()
            .any(|kind| item.kind.to_string() == *kind)
    {
        return false;
    }
    if !filter.states.is_empty() && !filter.states.contains(&item.state) {
        return false;
    }
    if !filter.priorities.is_empty()
        && !filter
            .priorities
            .iter()
            .any(|priority| item.priority.to_string() == *priority)
    {
        return false;
    }
    if filter
        .assignee_actor_id
        .as_ref()
        .is_some_and(|assignee| item.assignee_actor_id.as_ref() != Some(assignee))
    {
        return false;
    }
    if filter
        .initiative_id
        .as_ref()
        .is_some_and(|initiative| item.initiative_id.as_ref() != Some(initiative))
    {
        return false;
    }
    if filter
        .label
        .as_ref()
        .is_some_and(|label| !item.labels.contains(label))
    {
        return false;
    }
    if !filter.trust_states.is_empty()
        && !filter
            .trust_states
            .iter()
            .any(|state| item.trust_state.to_string() == *state)
    {
        return false;
    }
    if let Some(needle) = filter.text.as_deref().filter(|text| !text.is_empty()) {
        let needle = needle.to_lowercase();
        let haystack = [
            item.title.as_str(),
            item.body.as_str(),
            item.reference.as_str(),
        ];
        if !haystack
            .iter()
            .any(|field| field.to_lowercase().contains(&needle))
        {
            return false;
        }
    }
    !(filter.exclude_terminal && TERMINAL_STATES.contains(&item.state.as_str()))
}

/// The linked projects a scope covers: the one given, or every linked one.
pub fn linked_projects(
    view: &impl ReadView,
    project: Option<&Project>,
) -> Result<Vec<Project>, VogtError> {
    if let Some(project) = project {
        return Ok(if is_linked(project) {
            vec![project.clone()]
        } else {
            Vec::new()
        });
    }
    Ok(view
        .list_projects(10_000, 0)?
        .into_iter()
        .filter(is_linked)
        .collect())
}

/// The item a ref names: a declared `WI-n` first, otherwise the upstream item a
/// subject key names. The single resolver, so a declared ref and a forge
/// subject resolve the same way on every surface.
pub fn resolve_work_ref<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    reference: &str,
) -> Result<WorkItem, VogtError> {
    if let Some(item) = view.work_item_by_ref(reference)? {
        return Ok(item);
    }
    resolve_upstream(ctx, view, reference)?.ok_or_else(|| {
        VogtError::NotFound(format!("no work item {}", crate::core::py_repr(reference)))
    })
}

/// The upstream-truth item a subject key names, or `None` when nothing mirrored
/// carries it.
pub fn resolve_upstream<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    view: &impl ReadView,
    reference: &str,
) -> Result<Option<WorkItem>, VogtError> {
    if !ctx.observed.has_evidence_tables()? {
        return Ok(None);
    }
    let Some(observation) = ctx.observed.latest_by_subject(reference)? else {
        return Ok(None);
    };
    let Some(project_id) = observation.project_id.clone() else {
        return Ok(None);
    };
    let Some(project) = view.project_by_id(&project_id)? else {
        return Ok(None);
    };
    if !is_linked(&project) {
        return Ok(None);
    }
    let overlays = view.work_overlays(std::slice::from_ref(&observation.subject_key))?;
    let confirmed = ctx
        .observed
        .last_confirmed(std::slice::from_ref(&observation.subject_key))?;
    let workflow = view.workflow_for(crate::decisions::work_kind_of(&observation))?;
    Ok(Some(build_item(
        ctx,
        &observation,
        overlays.get(&observation.subject_key),
        &project,
        &workflow,
        confirmed.get(&observation.subject_key).copied(),
    )))
}

/// Parse a wire string into its enum, falling back to the vocabulary's own
/// default. `work_kind_of`, `priority_of` and `trust_for` return the text the
/// wire carries; the item stores the enum. `Default` is not that default —
/// `Priority` falls back to `p2` and `TrustState` to `unverified`, not their
/// first variant.
fn parse_wire<T>(text: &str, fallback: T) -> T
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(serde_json::Value::String(text.to_string())).unwrap_or(fallback)
}
