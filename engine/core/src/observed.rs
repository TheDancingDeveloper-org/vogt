//! Reading observed subjects as work. Ports `src/vogt/core/observed.py`.
//!
//! A collected subject appears in a ranked view immediately, but nobody typed it
//! in, so it has no priority, no state and no assignee. The guesses that fill
//! those gaps live here, in one place, because they are best effort and
//! correctable: adopting a subject promotes it into a real work item where the
//! guess can be overridden, and suppressing it removes it from ranked views.

#![allow(dead_code)]

use crate::core::{Observation, WorkOverlay, Workflow, DONE, TERMINAL_STATES};

/// Observation kinds that represent work somebody might do. Everything else a
/// collector finds — checkouts, releases, dependency references, CI checks — is
/// context, not backlog, and never enters a ranked view.
pub const WORKLIKE_KINDS: &[&str] = &["forge.issue", "forge.pull_request", "marker"];

/// Labels that make a forge issue a bug. Matched case-insensitively.
const BUG_LABELS: &[&str] = &["bug", "defect", "regression", "crash"];

/// Marker tags that read as defects rather than as chores.
const BUG_TAGS: &[&str] = &["FIXME", "HACK", "XXX"];

const DEFAULT_OBSERVED_PRIORITY: &str = "p2";
const MARKER_PRIORITY: &str = "p3";

/// The state an observed subject is shown in. Not a workflow state: nothing has
/// transitioned it, and giving it `open` would imply it obeys a machine it has
/// never been through.
pub const OBSERVED_STATE: &str = "observed";

const LIFECYCLE_OPEN: &str = "open";
const LIFECYCLE_CLOSED: &str = "closed";
const LIFECYCLE_UNKNOWN: &str = "unknown";

/// Map an observed subject onto a work kind, best effort. A wrong guess costs
/// one row in the wrong filter and is fixed by adopting the subject.
pub fn work_kind_of(observation: &Observation) -> String {
    if observation.kind == "marker" {
        let tag = observation
            .payload
            .get("tag")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_uppercase();
        return if BUG_TAGS.contains(&tag.as_str()) {
            "bug".to_string()
        } else {
            "chore".to_string()
        };
    }
    if observation.kind == "forge.pull_request" {
        // Work in flight: not a defect, not a new request. `chore` keeps it out
        // of the bug view while leaving it in the backlog.
        return "chore".to_string();
    }
    let bug = labels(observation)
        .iter()
        .any(|label| BUG_LABELS.contains(&label.to_lowercase().as_str()));
    if bug {
        "bug".to_string()
    } else {
        "feature".to_string()
    }
}

/// Whether the source says this subject is still outstanding. `unknown` where
/// the source did not say, and it must stay distinguishable from the other two:
/// a subject nobody could read is not thereby open.
pub fn lifecycle_of(observation: &Observation) -> &'static str {
    if observation.kind == "marker" {
        return LIFECYCLE_OPEN;
    }
    let Some(state) = observation
        .payload
        .get("state")
        .and_then(serde_json::Value::as_str)
    else {
        return LIFECYCLE_UNKNOWN;
    };
    match state.trim().to_lowercase().as_str() {
        LIFECYCLE_OPEN => LIFECYCLE_OPEN,
        LIFECYCLE_CLOSED | "merged" => LIFECYCLE_CLOSED,
        _ => LIFECYCLE_UNKNOWN,
    }
}

/// Map mirror lifecycle plus overlay refinement onto a workflow state. Upstream
/// is the truth for open or closed; the overlay refines which state within that
/// truth. An overlay state is honoured on an open subject even when terminal,
/// because a write-through close lands upstream before the next sweep refreshes
/// the mirror and the item somebody just closed must not read `open` meanwhile.
pub fn upstream_state(
    observation: &Observation,
    overlay: Option<&WorkOverlay>,
    workflow: &Workflow,
) -> String {
    if lifecycle_of(observation) == LIFECYCLE_CLOSED {
        if let Some(state) = overlay.and_then(|overlay| overlay.workflow_state.as_deref()) {
            if TERMINAL_STATES.contains(&state) {
                return state.to_string();
            }
        }
        return DONE.to_string();
    }
    if let Some(state) = overlay.and_then(|overlay| overlay.workflow_state.clone()) {
        return state;
    }
    workflow.initial_state.clone()
}

/// Whether anything actually said what kind of work this is. `work_kind_of` has
/// to return something, and for an unlabelled issue that something is a guess.
pub fn is_classified(observation: &Observation) -> bool {
    if observation.kind == "marker" || observation.kind == "forge.pull_request" {
        return true;
    }
    !labels(observation).is_empty()
}

/// Derive a priority so an observed subject can be ordered at all. An explicit
/// `p0`–`p4` label wins, because somebody said it; otherwise markers sit one
/// band below issues.
pub fn priority_of(observation: &Observation) -> String {
    for label in labels(observation) {
        let candidate = label.trim().to_lowercase();
        if ["p0", "p1", "p2", "p3", "p4"].contains(&candidate.as_str()) {
            return candidate;
        }
    }
    if observation.kind == "marker" {
        MARKER_PRIORITY.to_string()
    } else {
        DEFAULT_OBSERVED_PRIORITY.to_string()
    }
}

/// A one-line description, whatever kind of subject this is.
pub fn title_of(observation: &Observation) -> String {
    if observation.kind == "marker" {
        let text = observation
            .payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();
        let path = observation
            .payload
            .get("path")
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let line = observation
            .payload
            .get("line")
            .map(|value| value.to_string())
            .unwrap_or_else(|| "?".to_string());
        let tag = observation
            .payload
            .get("tag")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("TODO");
        let marker = format!("{tag} {path}:{line}");
        return if text.is_empty() {
            marker
        } else {
            format!("{marker} — {text}")
        };
    }
    let title = observation
        .payload
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    let number = observation
        .payload
        .get("number")
        .map(|value| match value.as_i64() {
            Some(whole) => whole.to_string(),
            None => value.to_string(),
        });
    match (title.is_empty(), number) {
        (false, Some(number)) => format!("#{number} {title}"),
        (false, None) => title.to_string(),
        (true, _) => observation.subject_key.clone(),
    }
}

/// Whether this subject belongs in a ranked view at all. An unpromoted marker
/// is still observed, still queryable and still counted — it just does not
/// claim to be work.
pub fn is_worklike(observation: &Observation) -> bool {
    if !WORKLIKE_KINDS.contains(&observation.kind.as_str()) {
        return false;
    }
    if observation.kind == "marker" {
        return observation.promoted;
    }
    true
}

/// The work-item subject keys a PR observation says it implements, so the
/// backlog can collapse the PR under the one it implements. Empty for anything
/// that is not a PR, or a PR that named no work — never a guess.
pub fn implemented_targets(observation: &Observation) -> Vec<String> {
    if observation.kind != "forge.pull_request" {
        return Vec::new();
    }
    let Some(implements) = observation
        .payload
        .get("implements")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    implements
        .iter()
        .filter_map(|edge| edge.get("subject").and_then(serde_json::Value::as_str))
        .filter(|subject| !subject.is_empty())
        .map(str::to_string)
        .collect()
}

fn labels(observation: &Observation) -> Vec<String> {
    observation
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
        .unwrap_or_default()
}
