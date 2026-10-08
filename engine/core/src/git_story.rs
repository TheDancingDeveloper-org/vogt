//! Deriving a work item's git story — branch, PR, phase — from observed data.
//! Ports `src/vogt/core/git_story.py`.
//!
//! A phase is computed from what was observed, never written onto the item, and
//! it never competes with the workflow state the machine owns.

#![allow(dead_code)]

/// The derived phase ladder, in order. Shown beside the workflow state, never
/// as it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPhase {
    NoBranch,
    BranchActive,
    PrOpen,
    InReview,
    Merged,
}

/// The derived PR state. The absence of a PR is `None` at the call site, not a
/// member here — an existing PR always has one of these states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Draft,
    Open,
    InReview,
    Merged,
    Closed,
}

/// Review decisions that mean a review is actually under way. Matched
/// case-insensitively. A forge that does not expose one leaves it absent, which
/// reads as `Open`, never as `InReview`.
const REVIEW_DECIDED: [&str; 4] = ["approved", "changes_requested", "commented", "dismissed"];

pub const DRIFT_CLOSED_ITEM_OPEN_PR: &str = "closed_item_open_pr";
pub const DRIFT_MERGED_PR_OPEN_ITEM: &str = "merged_pr_open_item";
pub const DRIFT_ACTIVE_BRANCH_DONE_ITEM: &str = "active_branch_done_item";

/// Read the PR's collected fields into the richer derived state. `merged` and
/// `closed` are lifecycle facts; above them, `draft` and `in-review` come from
/// the PR's own fields. An absent review decision is honestly `open`.
pub fn derive_pr_state(
    raw_state: Option<&str>,
    draft: bool,
    review_decision: Option<&str>,
) -> PrState {
    let normalised = raw_state.unwrap_or("").trim().to_ascii_lowercase();
    if normalised == "merged" {
        return PrState::Merged;
    }
    if normalised == "closed" {
        return PrState::Closed;
    }
    if draft {
        return PrState::Draft;
    }
    if review_decision.is_some_and(|decision| {
        REVIEW_DECIDED.contains(&decision.trim().to_ascii_lowercase().as_str())
    }) {
        return PrState::InReview;
    }
    PrState::Open
}

/// The single phase shown beside the workflow state. The PR dominates when it
/// is live or shipped. A closed, unmerged PR is a dead end: the item reverts to
/// whatever its branch says.
pub fn derive_phase(has_branch: bool, pr_state: Option<PrState>) -> GitPhase {
    match pr_state {
        Some(PrState::Merged) => GitPhase::Merged,
        Some(PrState::InReview) => GitPhase::InReview,
        Some(PrState::Open | PrState::Draft) => GitPhase::PrOpen,
        Some(PrState::Closed) | None => {
            if has_branch {
                GitPhase::BranchActive
            } else {
                GitPhase::NoBranch
            }
        }
    }
}

/// The contradictions between the item and its git evidence, as `(code,
/// message)` pairs, empty when the two agree. This reports the disagreement; it
/// does not resolve it.
pub fn derive_drift(
    item_terminal: bool,
    pr_state: Option<PrState>,
    has_active_branch: bool,
) -> Vec<(&'static str, &'static str)> {
    let mut findings = Vec::new();
    if item_terminal
        && matches!(
            pr_state,
            Some(PrState::Draft | PrState::Open | PrState::InReview)
        )
    {
        findings.push((
            DRIFT_CLOSED_ITEM_OPEN_PR,
            "the item is closed but its pull request is still open — the change has not landed",
        ));
    }
    if pr_state == Some(PrState::Merged) && !item_terminal {
        findings.push((
            DRIFT_MERGED_PR_OPEN_ITEM,
            "the pull request merged but the item is still open — it never moved to done",
        ));
    }
    if item_terminal && has_active_branch {
        findings.push((
            DRIFT_ACTIVE_BRANCH_DONE_ITEM,
            "a branch is still active in a checkout for an item marked done",
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_phase_ladder() {
        assert_eq!(derive_phase(false, None), GitPhase::NoBranch);
        assert_eq!(derive_phase(true, None), GitPhase::BranchActive);
        assert_eq!(derive_phase(true, Some(PrState::Open)), GitPhase::PrOpen);
        assert_eq!(derive_phase(true, Some(PrState::Draft)), GitPhase::PrOpen);
        assert_eq!(
            derive_phase(true, Some(PrState::InReview)),
            GitPhase::InReview
        );
        assert_eq!(derive_phase(false, Some(PrState::Merged)), GitPhase::Merged);
        // A dead PR is not a phase of its own.
        assert_eq!(
            derive_phase(true, Some(PrState::Closed)),
            GitPhase::BranchActive
        );
        assert_eq!(
            derive_phase(false, Some(PrState::Closed)),
            GitPhase::NoBranch
        );
    }

    #[test]
    fn pr_state_reads_each_field() {
        assert_eq!(
            derive_pr_state(Some("merged"), false, None),
            PrState::Merged
        );
        assert_eq!(
            derive_pr_state(Some("closed"), false, None),
            PrState::Closed
        );
        assert_eq!(derive_pr_state(Some("open"), true, None), PrState::Draft);
        assert_eq!(
            derive_pr_state(Some("open"), false, Some("approved")),
            PrState::InReview
        );
        assert_eq!(
            derive_pr_state(Some("open"), false, Some("changes_requested")),
            PrState::InReview
        );
        // A required-but-not-started review is not a started one, and the match
        // ignores surrounding space and case.
        assert_eq!(
            derive_pr_state(Some("open"), false, Some("review_required")),
            PrState::Open
        );
        assert_eq!(
            derive_pr_state(Some(" OPEN "), false, Some("  Approved ")),
            PrState::InReview
        );
        assert_eq!(derive_pr_state(None, false, None), PrState::Open);
        // Merged wins over a draft flag and a review decision.
        assert_eq!(
            derive_pr_state(Some("merged"), true, Some("approved")),
            PrState::Merged
        );
    }

    fn codes<'a>(findings: &'a [(&'a str, &'a str)]) -> Vec<&'a str> {
        findings.iter().map(|(code, _)| *code).collect()
    }

    #[test]
    fn drift_fires_on_each_contradiction_and_not_on_agreement() {
        assert_eq!(
            codes(&derive_drift(true, Some(PrState::Open), false)),
            vec![DRIFT_CLOSED_ITEM_OPEN_PR]
        );
        assert_eq!(
            codes(&derive_drift(true, Some(PrState::Draft), false)),
            vec![DRIFT_CLOSED_ITEM_OPEN_PR]
        );
        assert_eq!(
            codes(&derive_drift(false, Some(PrState::Merged), false)),
            vec![DRIFT_MERGED_PR_OPEN_ITEM]
        );
        assert_eq!(
            codes(&derive_drift(true, None, true)),
            vec![DRIFT_ACTIVE_BRANCH_DONE_ITEM]
        );
        // Both fire together: a done item with a live PR and a live branch.
        assert_eq!(
            codes(&derive_drift(true, Some(PrState::InReview), true)),
            vec![DRIFT_CLOSED_ITEM_OPEN_PR, DRIFT_ACTIVE_BRANCH_DONE_ITEM]
        );
        assert!(derive_drift(false, Some(PrState::Open), true).is_empty());
        assert!(derive_drift(true, Some(PrState::Merged), false).is_empty());
        assert!(derive_drift(false, None, false).is_empty());
        // A closed PR on a done item is not an open PR.
        assert!(derive_drift(true, Some(PrState::Closed), false).is_empty());
    }
}
