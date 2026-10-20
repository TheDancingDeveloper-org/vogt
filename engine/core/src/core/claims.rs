//! Check a session's completion claim against git and the forge (WI-1835).
//! Ports `src/vogt/core/claims.py`.
//!
//! A worker that prints `AUTOPILOT: DONE`, or moves its item to review or done,
//! is claiming something: that it committed, pushed, opened or merged a pull
//! request. Workers sometimes claim work they did not do, for example a PR
//! "opened and merged" that had merged the day before the session started.
//! This module compares the claim with facts read elsewhere and names each
//! mismatch in words.
//!
//! Pure: the facts and the clock are parameters, and nothing here reads git,
//! the forge or storage.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use crate::core::Moment;

/// `PR #193`, `pull request #193`, `#193`, `pull/193`. A bare `#n` counts:
/// workers write it that way, and a number that names no observed PR is
/// ignored rather than flagged.
static PR_NUMBER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?:\bpull/|#)(\d{1,7})\b").unwrap());
/// The claim that something landed: "merged", "was merged", "landed".
static MERGE_CLAIM: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)\b(?:merged|landed)\b").unwrap());
const FAILING_CHECKS: &[&str] = &["failure", "failed", "error", "red"];

/// One pull request as the forge was last observed to report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullFact {
    pub number: i64,
    pub state: String,
    pub merged_at: Option<Moment>,
    pub merged_by: Option<String>,
    pub checks: Option<String>,
    pub head_ref: Option<String>,
}

impl PullFact {
    /// A pull with only a number and a state, the rest unknown (Python's
    /// dataclass defaults).
    pub fn new(number: i64, state: impl Into<String>) -> Self {
        Self {
            number,
            state: state.into(),
            merged_at: None,
            merged_by: None,
            checks: None,
            head_ref: None,
        }
    }
}

/// What was checked. A `None` field means it could not be read, and that
/// fact is left unchecked rather than counted as a mismatch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaimFacts {
    pub claim_text: Option<String>,
    pub session_started_at: Option<Moment>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub pushed: Option<bool>,
    pub new_commits: Option<i64>,
    pub pulls: Vec<PullFact>,
}

/// The PR numbers a claim names.
///
/// Python's `int()` also reads a non-ASCII decimal digit run; Rust's parse
/// does not, so such a number is dropped here. Workers write ASCII digits.
pub fn mentioned_pulls(text: Option<&str>) -> BTreeSet<i64> {
    let Some(text) = text.filter(|text| !text.is_empty()) else {
        return BTreeSet::new();
    };
    PR_NUMBER
        .captures_iter(text)
        .filter_map(|captures| captures.get(1)?.as_str().parse::<i64>().ok())
        .collect()
}

pub fn claims_merge(text: Option<&str>) -> bool {
    text.is_some_and(|text| !text.is_empty() && MERGE_CLAIM.is_match(text))
}

/// Each way the claim and the facts disagree, in words. Empty when they
/// agree or when nothing could be checked.
pub fn mismatches(facts: &ClaimFacts) -> Vec<String> {
    let mut found = Vec::new();
    let named = mentioned_pulls(facts.claim_text.as_deref());
    let merge = claims_merge(facts.claim_text.as_deref());
    for pull in &facts.pulls {
        if !named.contains(&pull.number) {
            // Not named, but from this branch or bound to the item: a PR that
            // merged before the session started, from a session that made no
            // commits, is the same false claim made without saying it.
            if facts.new_commits == Some(0) && merged_before_start(pull, facts.session_started_at) {
                found.push(format!(
                    "PR #{} merged at {}, before this session started, and the session made no commits",
                    pull.number,
                    at(pull)
                ));
            }
            continue;
        }
        if merge && pull.state != "merged" {
            found.push(format!(
                "claimed merged; PR #{} is {}",
                pull.number, pull.state
            ));
        }
        if merged_before_start(pull, facts.session_started_at) {
            let started = facts
                .session_started_at
                .expect("merged_before_start needs a start");
            found.push(format!(
                "claimed PR #{}; it merged {}, before this session started ({})",
                pull.number,
                at(pull),
                started.to_iso()
            ));
        }
        let checks = pull.checks.as_deref().unwrap_or("").to_lowercase();
        if FAILING_CHECKS.contains(&checks.as_str()) {
            found.push(format!(
                "PR #{} CI is {}",
                pull.number,
                pull.checks.as_deref().unwrap_or("None")
            ));
        }
    }
    if facts.new_commits == Some(0) {
        let on = match facts.branch.as_deref() {
            Some(branch) if !branch.is_empty() => format!(" on {branch}"),
            _ => String::new(),
        };
        found.push(format!("no new commits from this session{on}"));
    }
    if facts.pushed == Some(false) && facts.new_commits != Some(0) {
        let head: String = facts
            .head
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(12)
            .collect();
        found.push(format!("branch head {head} is not pushed").replace("  ", " "));
    }
    found
}

fn merged_before_start(pull: &PullFact, started: Option<Moment>) -> bool {
    match (pull.merged_at, started) {
        (Some(merged_at), Some(started)) => pull.state == "merged" && merged_at < started,
        _ => false,
    }
}

fn at(pull: &PullFact) -> String {
    let by = match pull.merged_by.as_deref() {
        Some(merger) if !merger.is_empty() => format!(" by {merger}"),
        _ => String::new(),
    };
    let when = pull
        .merged_at
        .map(Moment::to_iso)
        .unwrap_or_else(|| "an unknown time".to_string());
    format!("{when}{by}")
}

#[cfg(test)]
mod tests {
    //! Ports the pure half of `tests/test_claim_check.py`.

    use super::*;
    use crate::core::from_iso;

    fn started() -> Moment {
        from_iso("2026-10-18T09:00:00+00:00").unwrap()
    }

    #[test]
    fn names_the_prs_a_claim_mentions() {
        assert_eq!(
            mentioned_pulls(Some("PR #193 was opened and merged; see pull/7")),
            BTreeSet::from([193, 7])
        );
        assert!(mentioned_pulls(None).is_empty());
    }

    #[test]
    fn a_pr_merged_before_the_session_started_is_flagged() {
        // The WI-1707 claim: "PR #193 was opened and merged", merged the day before.
        let facts = ClaimFacts {
            claim_text: Some("PR #193 was opened and merged. AUTOPILOT: DONE".into()),
            session_started_at: Some(started()),
            branch: Some("wi-1707-x".into()),
            head: Some("abc".into()),
            pushed: Some(true),
            new_commits: Some(0),
            pulls: vec![PullFact {
                merged_at: Some(started().plus_days(-1)),
                merged_by: Some("sprooty".into()),
                ..PullFact::new(193, "merged")
            }],
        };
        let found = mismatches(&facts);
        assert!(found.iter().any(|line| line.contains("PR #193")
            && line.contains("before this session started")
            && line.contains("sprooty")));
        assert!(found.contains(&"no new commits from this session on wi-1707-x".to_string()));
        assert_eq!(
            found[0],
            "claimed PR #193; it merged 2026-10-17T09:00:00+00:00 by sprooty, before this \
             session started (2026-10-18T09:00:00+00:00)"
        );
    }

    #[test]
    fn an_unnamed_pr_merged_before_the_start_with_no_commits_is_flagged() {
        let facts = ClaimFacts {
            claim_text: Some("AUTOPILOT: DONE".into()),
            session_started_at: Some(started()),
            new_commits: Some(0),
            pulls: vec![PullFact {
                merged_at: Some(started().plus_days(-1)),
                ..PullFact::new(193, "merged")
            }],
            ..ClaimFacts::default()
        };
        assert_eq!(
            mismatches(&facts),
            vec![
                "PR #193 merged at 2026-10-17T09:00:00+00:00, before this session started, \
                 and the session made no commits"
                    .to_string(),
                "no new commits from this session".to_string(),
            ]
        );
    }

    #[test]
    fn a_claimed_merge_of_an_open_pr_is_flagged() {
        let facts = ClaimFacts {
            claim_text: Some("merged #4".into()),
            session_started_at: Some(started()),
            new_commits: Some(2),
            pushed: Some(true),
            pulls: vec![PullFact {
                checks: Some("failure".into()),
                ..PullFact::new(4, "open")
            }],
            ..ClaimFacts::default()
        };
        assert_eq!(
            mismatches(&facts),
            vec!["claimed merged; PR #4 is open", "PR #4 CI is failure"]
        );
    }

    #[test]
    fn an_honest_claim_and_unknown_facts_flag_nothing() {
        let honest = ClaimFacts {
            claim_text: Some("pushed wi-1-x, PR #9 opened".into()),
            session_started_at: Some(started()),
            new_commits: Some(3),
            pushed: Some(true),
            pulls: vec![PullFact {
                checks: Some("success".into()),
                ..PullFact::new(9, "open")
            }],
            ..ClaimFacts::default()
        };
        assert!(mismatches(&honest).is_empty());
        let unknown = ClaimFacts {
            claim_text: Some("done".into()),
            ..ClaimFacts::default()
        };
        assert!(mismatches(&unknown).is_empty());
    }

    #[test]
    fn unpushed_work_is_flagged() {
        let facts = ClaimFacts {
            claim_text: Some("done".into()),
            session_started_at: Some(started()),
            head: Some("0123456789abcdef".into()),
            new_commits: Some(1),
            pushed: Some(false),
            ..ClaimFacts::default()
        };
        assert_eq!(
            mismatches(&facts),
            vec!["branch head 0123456789ab is not pushed"]
        );
        // An unread head leaves no double space.
        let headless = ClaimFacts {
            head: None,
            ..facts
        };
        assert_eq!(mismatches(&headless), vec!["branch head is not pushed"]);
    }
}
