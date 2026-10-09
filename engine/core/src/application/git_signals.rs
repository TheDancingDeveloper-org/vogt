//! The git-activity inputs the ranker reads. Ports `GitSignals` and
//! `build_git_signals` in `services/git_story.py`.
//!
//! A moving item — one with a recently-committed branch or an open pull
//! request — ranks above idle work of the same priority. Computing that per
//! item during a backlog scan would be a query storm, so this gathers it for a
//! whole project scope in two reads and answers each item by its ref. `why`,
//! the backlog, the board and the bugs view all read the same signals, which is
//! why they live here rather than inside one of those views.

use std::collections::{HashMap, HashSet};

use crate::application::context::AppContext;
use crate::core::{Clock, IdFactory, Moment};
use crate::errors::VogtError;
use crate::git_story::derive_pr_state;
use crate::storage::interface::ObservedStore;

/// The open pull requests and the branch activity for one project scope, keyed
/// by subject ref and by forge number.
pub struct GitSignals {
    open_pr_refs: HashSet<String>,
    open_pr_numbers: HashSet<i64>,
    branch_age_by_ref: HashMap<String, i64>,
    branch_age_by_number: HashMap<i64, i64>,
}

impl GitSignals {
    /// `(has_open_pr, branch_activity_seconds)` for the item named `reference`.
    pub fn for_ref(&self, reference: &str) -> (bool, Option<i64>) {
        let number = forge_number(reference);
        let open_pr = self.open_pr_refs.contains(reference)
            || number.is_some_and(|number| self.open_pr_numbers.contains(&number));
        let age =
            self.branch_age_by_ref.get(reference).copied().or_else(|| {
                number.and_then(|number| self.branch_age_by_number.get(&number).copied())
            });
        (open_pr, age)
    }
}

/// Gather the ranking git signals for a project scope, or the whole estate
/// when `project_id` is absent.
pub fn git_signals<C: Clock, I: IdFactory>(
    ctx: &AppContext<C, I>,
    project_id: Option<&str>,
    now: Moment,
) -> Result<GitSignals, VogtError> {
    let mut signals = GitSignals {
        open_pr_refs: HashSet::new(),
        open_pr_numbers: HashSet::new(),
        branch_age_by_ref: HashMap::new(),
        branch_age_by_number: HashMap::new(),
    };
    if !ctx.observed.has_evidence_tables()? {
        return Ok(signals);
    }
    let pulls = ctx.observed.latest(
        &["forge.pull_request".to_string()],
        project_id,
        false,
        false,
        1000,
    )?;
    for observation in &pulls {
        let state = derive_pr_state(
            observation
                .payload
                .get("state")
                .and_then(serde_json::Value::as_str),
            observation
                .payload
                .get("draft")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            observation
                .payload
                .get("review_state")
                .and_then(serde_json::Value::as_str),
        );
        if !matches!(
            state,
            crate::git_story::PrState::Open
                | crate::git_story::PrState::Draft
                | crate::git_story::PrState::InReview
        ) {
            continue;
        }
        let Some(edges) = observation
            .payload
            .get("implements")
            .and_then(serde_json::Value::as_array)
        else {
            continue;
        };
        for edge in edges {
            if let Some(subject) = edge.get("subject").and_then(serde_json::Value::as_str) {
                signals.open_pr_refs.insert(subject.to_string());
            }
            if let Some(number) = edge.get("number").and_then(serde_json::Value::as_i64) {
                signals.open_pr_numbers.insert(number);
            }
        }
    }
    let branches =
        ctx.observed
            .latest(&["git.branch".to_string()], project_id, false, false, 1000)?;
    for observation in &branches {
        let Some(committed) = observation
            .payload
            .get("last_commit_at")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Ok(committed) = crate::core::from_iso(committed) else {
            continue;
        };
        let age = now.unix_seconds() - committed.unix_seconds();
        if let Some(work_ref) = observation
            .payload
            .get("work_item_ref")
            .and_then(serde_json::Value::as_str)
        {
            signals
                .branch_age_by_ref
                .entry(work_ref.to_string())
                .and_modify(|kept| *kept = (*kept).min(age))
                .or_insert(age);
        }
        if let Some(number) = observation
            .payload
            .get("forge_number")
            .and_then(serde_json::Value::as_i64)
        {
            signals
                .branch_age_by_number
                .entry(number)
                .and_modify(|kept| *kept = (*kept).min(age))
                .or_insert(age);
        }
    }
    Ok(signals)
}

/// The forge number a reference carries: the digits after the last `#`, which is
/// how an upstream subject such as `gh:acme/widget#12` names its issue. Python's
/// `_forge_number` is `#(\d+)$`, and `\d` is any Unicode decimal digit, so the
/// captured run goes through the same normalisation the branch binding uses —
/// `int()` strips leading zeros. A `gh-<n>` prefix is not one.
fn forge_number(reference: &str) -> Option<i64> {
    static PATTERN: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"#(\d+)$").unwrap());
    let tail = PATTERN.captures(reference)?.get(1)?.as_str();
    crate::branches::normalise_digits(tail).parse().ok()
}

#[cfg(test)]
mod tests {
    use super::forge_number;

    #[test]
    fn a_number_is_the_digits_after_the_last_hash() {
        // Python's `#(\d+)$`: the issue number sits at the end of the subject,
        // so a `gh-<n>` prefix is not a number and a hash earlier in the key is
        // not the one that counts.
        assert_eq!(forge_number("gh:acme/widget#12"), Some(12));
        assert_eq!(forge_number("gh:acme/widget#7"), Some(7));
        assert_eq!(forge_number("#12"), Some(12));
        assert_eq!(forge_number("gh-12"), None);
        assert_eq!(forge_number("gh-12-widget"), None);
        assert_eq!(forge_number("WI-12"), None);
        assert_eq!(forge_number("gh:acme/widget#12-extra"), None);
        assert_eq!(forge_number("gh:acme/widget#"), None);
    }

    #[test]
    fn a_unicode_digit_counts_and_leading_zeros_fold() {
        // `\d` is any decimal digit and `int()` strips leading zeros, both of
        // which the branch binding's normalisation already does.
        assert_eq!(forge_number("gh:acme/widget#١٢"), Some(12));
        assert_eq!(forge_number("gh:acme/widget#007"), Some(7));
    }

    #[test]
    fn the_not_found_text_quotes_like_python() {
        // Python's `!r`: an apostrophe with no double quote takes double quotes,
        // and a tab is escaped rather than printed raw.
        use crate::core::py_repr;
        let text = |reference: &str| {
            format!("no work item or observed subject {}", py_repr(reference))
        };
        assert_eq!(text("it's"), "no work item or observed subject \"it's\"");
        assert_eq!(text("é\t"), "no work item or observed subject 'é\\t'");
        assert_eq!(
            text("say \"hi\""),
            "no work item or observed subject 'say \"hi\"'"
        );
    }
}
