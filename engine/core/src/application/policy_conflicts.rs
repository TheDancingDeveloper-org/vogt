//! Where a caller's task contradicts the project's landing rule (WI-1832).
//! Ports `src/vogt/application/services/_policy_conflicts.py`.
//!
//! A task is free text written by oversight or a person; the landing rule is
//! the project's recorded policy. When the two disagree on a known axis — open
//! a PR or push only, who merges, which base, who deploys — a worker that reads
//! both stops to ask. The policy wins, and the brief says so in as many words,
//! so the worker has one instruction instead of two.
//!
//! Detection is a reading of the text, not a parse: each axis is a small set of
//! phrasings, and a match preceded by a negation ("open no PR", "never merge")
//! is agreement, not conflict. A miss leaves the brief as it was; a false hit
//! adds a line restating the policy the brief already carries.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::{Match, Regex};

use crate::application::brief::LandingFields;

/// Words that, just before a phrase, turn an instruction into its opposite.
/// Python's `$` (no MULTILINE) also matches before a final newline, hence
/// `\n?$` here.
static NEGATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:no|not|never|don'?t|do\s+not|without|nor|instead\s+of)\b[^.;\n]{0,24}\n?$",
    )
    .expect("the negation pattern compiles")
});

static OPEN_PR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:open|create|raise|file|submit|send)\s+(?:a\s+|an\s+|the\s+)?(?:draft\s+)?(?:PR|pull[\s-]request)\b|\bgh\s+pr\s+create\b",
    )
    .expect("the open-PR pattern compiles")
});

static PUSH_ONLY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:open|create|raise)\s+no\s+(?:PR|pull[\s-]request)\b|\bpush(?:\s+the)?(?:\s+branch)?\s+only\b|\bno\s+(?:PR|pull[\s-]request)\b",
    )
    .expect("the push-only pattern compiles")
});

static MERGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\bgh\s+pr\s+merge\b|\bmerge\s+(?:it|this|the\s+(?:PR|pull[\s-]request|branch|change))\b|\bmerge\s+(?:it\s+)?(?:to|into|onto)\s+[`'"]?[\w./-]+|\band\s+merge\b|\bthen\s+merge\b|\bself[\s-]merge\b"#,
    )
    .expect("the merge pattern compiles")
});

static DEPLOY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(?:deploy|ship|roll\s+out)\s+(?:it|this|the\s+\w+|to\s+[`'"]?[\w./-]+)|\band\s+deploy\b|\bthen\s+deploy\b"#,
    )
    .expect("the deploy pattern compiles")
});

static BASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(?:PR|pull[\s-]request|branch|rebase|base(?:d)?)\s+(?:against|onto|into|off(?:\s+of)?|from|on)\s+[`'"]?(?:origin/)?(?P<branch>[A-Za-z0-9][\w./-]*[\w])[`'"]?"#,
    )
    .expect("the base pattern compiles")
});

/// Who may deploy without the policy being contradicted.
const SELF_DEPLOYERS: [&str; 3] = ["worker", "agent", "session"];

/// Every match of `pattern` that is not negated by the words before it. The
/// window is Python's: the 40 characters (not bytes) before the match.
fn asserted<'t>(pattern: &Regex, task: &'t str) -> Vec<Match<'t>> {
    pattern
        .find_iter(task)
        .filter(|found| {
            let head = &task[..found.start()];
            let skip = head.chars().count().saturating_sub(40);
            let start = head
                .char_indices()
                .nth(skip)
                .map_or(head.len(), |(index, _)| index);
            !NEGATION.is_match(&head[start..])
        })
        .collect()
}

/// The `branch` group of every asserted base phrase in `task`.
fn asserted_bases(task: &str) -> Vec<String> {
    let mut branches = Vec::new();
    for found in asserted(&BASE, task) {
        // The match is re-read for its group; anchored at the same start it
        // is the same match.
        if let Some(captures) = BASE.captures_at(task, found.start()) {
            if let Some(branch) = captures.name("branch") {
                branches.push(branch.as_str().to_string());
            }
        }
    }
    branches
}

/// Python's `task_named_base`: the known long-lived branch `task` names as its
/// base, if exactly one.
///
/// A task that says "branch from `rust-core`" on a project that lists
/// `rust-core` among its long-lived branches means it (WI-1841): it is the
/// base, not a contradiction. An unknown branch, or two known ones, is not
/// taken — the first is the conflict check's to report, the second is
/// ambiguous.
pub fn task_named_base(task: &str, landing: Option<&LandingFields>) -> Option<String> {
    let landing = landing?;
    if task.trim().is_empty() {
        return None;
    }
    let known = landing.known_bases();
    let named: BTreeSet<String> = asserted_bases(task)
        .into_iter()
        .filter(|branch| known.contains(branch))
        .collect();
    if named.len() == 1 {
        named.into_iter().next()
    } else {
        None
    }
}

/// Python's `task_policy_conflicts`: the override lines for each axis where
/// `task` contradicts `landing`.
///
/// Empty when there is no task, no landing rule, or no conflict. Each line
/// names the axis, what the task asked, and what the policy requires.
/// `caller_merges` is an oversight session handing out its own landing work
/// on a project where oversight merges: a merge there is the policy, not
/// against it (WI-1841).
pub fn task_policy_conflicts(
    task: &str,
    landing: Option<&LandingFields>,
    caller_merges: bool,
) -> Vec<String> {
    let Some(landing) = landing else {
        return Vec::new();
    };
    if task.trim().is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let base = &landing.base_branch;
    if landing.worker_lands_by == "push_branch" && !asserted(&OPEN_PR, task).is_empty() {
        lines.push(
            "Policy overrides task: push only. The task asks for a pull \
             request; this project lands by pushing the branch. Open NO PR."
                .to_string(),
        );
    }
    if landing.worker_lands_by == "open_pr" && PUSH_ONLY.is_match(task) {
        lines.push(format!(
            "Policy overrides task: open a PR. The task says push only; this \
             project lands by opening a pull request against `{base}`."
        ));
    }
    if !caller_merges && !asserted(&MERGE, task).is_empty() {
        lines.push(format!(
            "Policy overrides task: do not merge. The task asks for a merge; \
             on this project {} merges ({}).",
            landing.merge_by, landing.merge_mode
        ));
    }
    if !SELF_DEPLOYERS.contains(&landing.deploy_by.as_str()) && !asserted(&DEPLOY, task).is_empty()
    {
        lines.push(format!(
            "Policy overrides task: do not deploy. The task asks for a \
             deploy; on this project {} deploys.",
            landing.deploy_by
        ));
    }
    let others: BTreeSet<String> = asserted_bases(task)
        .into_iter()
        .filter(|branch| branch != base)
        .collect();
    if !others.is_empty() {
        let named = others
            .iter()
            .map(|branch| format!("`{branch}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "Policy overrides task: base `{base}`. The task names {named}; \
             this project's workers cut from and land onto `{base}`."
        ));
    }
    lines
}

/// Python's `overrides_section`: the brief section that states the
/// overrides, or "" when there are none.
pub fn overrides_section(lines: &[String]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let rows = lines
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "## Policy overrides task\n\n\
         The task below contradicts this project's landing rule. The policy \
         wins; follow it and do not stop to ask.\n\n\
         {rows}\n"
    )
}

#[cfg(test)]
mod tests {
    //! Ports `tests/test_policy_conflicts.py`.

    use super::*;
    use serde_json::json;

    fn landing_with(changes: serde_json::Value) -> LandingFields {
        let mut fields = json!({
            "base_branch": "main",
            "worker_lands_by": "push_branch",
            "merge_by": "oversight",
            "merge_mode": "rebase",
        });
        for (key, value) in changes.as_object().unwrap() {
            fields[key] = value.clone();
        }
        serde_json::from_value(fields).unwrap()
    }

    fn landing() -> LandingFields {
        landing_with(json!({}))
    }

    #[test]
    fn each_axis_is_detected() {
        for (task, axis) in [
            ("Open a PR against main.", "push only"),
            ("then `gh pr create --fill`", "push only"),
            ("Create a draft pull request.", "push only"),
            ("Merge it into main when green.", "do not merge"),
            ("Fix, then merge.", "do not merge"),
            ("Deploy to vogt-dev afterwards.", "do not deploy"),
            ("Cut a branch off develop.", "base `main`"),
        ] {
            let lines = task_policy_conflicts(task, Some(&landing()), false);
            let wanted = format!("Policy overrides task: {axis}");
            assert!(
                lines.iter().any(|line| line.contains(&wanted)),
                "{task}: {lines:?}"
            );
        }
    }

    #[test]
    fn agreement_and_negation_are_not_conflicts() {
        for task in [
            "Open NO PR; push the branch only.",
            "Do not open a PR. Never merge, never deploy.",
            "Don't merge it into main.",
            "Resolve the merge conflicts and push.",
            "Rebase onto main and push.",
            "",
        ] {
            assert_eq!(
                task_policy_conflicts(task, Some(&landing()), false),
                Vec::<String>::new(),
                "{task}"
            );
        }
    }

    #[test]
    fn push_only_on_a_pr_project_is_overridden() {
        let lines = task_policy_conflicts(
            "Push the branch only.",
            Some(&landing_with(json!({"worker_lands_by": "open_pr"}))),
            false,
        );
        assert_eq!(
            lines,
            vec![
                "Policy overrides task: open a PR. The task says push only; this \
                 project lands by opening a pull request against `main`."
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_worker_deployer_allows_deploy_and_no_landing_means_nothing() {
        assert!(task_policy_conflicts(
            "Deploy it.",
            Some(&landing_with(json!({"deploy_by": "worker"}))),
            false
        )
        .is_empty());
        assert!(task_policy_conflicts("Open a PR and merge it.", None, false).is_empty());
    }

    #[test]
    fn the_section_is_empty_without_lines() {
        assert_eq!(overrides_section(&[]), "");
        assert!(overrides_section(&["x".to_string()]).starts_with("## Policy overrides task"));
    }

    #[test]
    fn a_known_long_lived_base_is_named_not_contradicted() {
        // WI-1841.
        let landing = landing_with(json!({"initiative_bases": {"rust-port": "rust-core"}}));
        assert_eq!(
            task_named_base("Branch off `rust-core`.", Some(&landing)).as_deref(),
            Some("rust-core")
        );
        assert_eq!(task_named_base("Branch off develop.", Some(&landing)), None);
        assert_eq!(
            task_named_base("Rebase onto main; branch off rust-core.", Some(&landing)),
            None
        );
        assert_eq!(
            task_named_base("Never branch off rust-core.", Some(&landing)),
            None
        );
    }

    #[test]
    fn a_caller_that_merges_is_not_told_not_to_merge() {
        let lines = task_policy_conflicts("Then merge it.", Some(&landing()), true);
        assert!(!lines.iter().any(|line| line.contains("do not merge")));
    }

    #[test]
    fn the_negation_window_is_forty_characters_and_a_final_newline_still_ends_it() {
        // Python slices 40 code points, not bytes, and its `$` matches before
        // a trailing newline.
        // 24 characters but 44 bytes stand between "no" and the merge.
        let wide = "é".repeat(20);
        assert!(
            task_policy_conflicts(&format!("no {wide} merge it"), Some(&landing()), false)
                .is_empty()
        );
        assert!(task_policy_conflicts("no\nmerge it", Some(&landing()), false).is_empty());
    }
}
