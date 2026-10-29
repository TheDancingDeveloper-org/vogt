// Pure: observations in, verdicts out. Nothing here reads a store or a clock.

const PULL_REQUEST_EVENTS: &[&str] = &["pull_request", "pull_request_target", "merge_group"];
const GITHUB_MANAGED_EVENTS: &[&str] = &["dynamic"];
const GITHUB_MANAGED_PATH_PREFIX: &str = "dynamic/";
const FAILING: &[&str] = &[
    "failure",
    "timed_out",
    "startup_failure",
    "action_required",
    "error",
];
const PASSING: &[&str] = &["success", "neutral", "skipped"];

fn payload_str<'a>(payload: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|text| !text.is_empty())
}

/// Whether a check observation is a run of a GitHub-managed workflow: its event
/// is `dynamic`, its workflow path sits under `dynamic/`, or — for an
/// observation stored before either was recorded — its name ends in a
/// Dependabot update number.
pub fn github_managed(payload: &serde_json::Value) -> bool {
    if payload_str(payload, "event").is_some_and(|event| GITHUB_MANAGED_EVENTS.contains(&event)) {
        return true;
    }
    if payload_str(payload, "workflow_path")
        .is_some_and(|path| path.starts_with(GITHUB_MANAGED_PATH_PREFIX))
    {
        return true;
    }
    if payload_str(payload, "event").is_some() {
        return false;
    }
    payload_str(payload, "check").is_some_and(dependabot_update_name)
}

/// ` - Update #<digits>` at the end of a run name, where the end is Python's
/// `$`: a trailing newline still matches.
fn dependabot_update_name(name: &str) -> bool {
    let name = name.strip_suffix('\n').unwrap_or(name);
    let Some(marker) = name.rfind(" - Update #") else {
        return false;
    };
    let tail = &name[marker + " - Update #".len()..];
    let digit = fancy_regex::Regex::new(r"^\d$").expect("digit");
    !tail.is_empty()
        && tail
            .chars()
            .all(|c| digit.is_match(&c.to_string()).unwrap_or(false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneKind {
    Branch,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRef {
    pub kind: LaneKind,
    pub lane: String,
    pub reference: String,
}

/// The lane a run belongs to, or `None` when it is not watched. A run with no
/// ref, one a pull request started, or one from a managed workflow is never
/// watched. A ref matching a branch pattern is a branch even if a tag pattern
/// would also match it.
pub fn watched_ref(
    branch: Option<&str>,
    event: Option<&str>,
    branches: &[&str],
    tags: &[&str],
) -> Option<WatchedRef> {
    let branch = branch.filter(|text| !text.is_empty())?;
    if event.is_some_and(|event| {
        PULL_REQUEST_EVENTS.contains(&event) || GITHUB_MANAGED_EVENTS.contains(&event)
    }) {
        return None;
    }
    for pattern in branches {
        if crate::glob::glob_match(pattern, branch) {
            return Some(WatchedRef {
                kind: LaneKind::Branch,
                lane: branch.to_string(),
                reference: branch.to_string(),
            });
        }
    }
    for pattern in tags {
        if crate::glob::glob_match(pattern, branch) {
            return Some(WatchedRef {
                kind: LaneKind::Tag,
                lane: (*pattern).to_string(),
                reference: branch.to_string(),
            });
        }
    }
    None
}

fn ci_workflow_of(check: &crate::core::Observation) -> String {
    payload_str(&check.payload, "check")
        .unwrap_or("workflow")
        .to_string()
}

fn ci_revision_of(check: &crate::core::Observation) -> String {
    payload_str(&check.payload, "revision")
        .unwrap_or("")
        .to_string()
}

fn ci_conclusion_of(check: &crate::core::Observation) -> Option<String> {
    payload_str(&check.payload, "conclusion").map(str::to_string)
}

/// When a run ran, for ordering: its own `updated_at`, then its run number,
/// then when Vogt observed it. Only a real integer counts; a bool, a float, a
/// string or a missing value all order as 0, the way `isinstance(n, int)` reads.
fn ci_ran_at(check: &crate::core::Observation) -> (String, (bool, u64, i64), crate::core::Moment) {
    (
        payload_str(&check.payload, "updated_at")
            .unwrap_or("")
            .to_string(),
        run_number_of(&check.payload),
        check.observed_at,
    )
}

/// A plain integer, kept whole even past `i64`. Anything else — a bool, a
/// float, a string, a missing value — is 0.
fn run_number_of(payload: &serde_json::Value) -> (bool, u64, i64) {
    match payload.get("run_number") {
        Some(serde_json::Value::Bool(true)) => (false, 0, 1),
        Some(serde_json::Value::Number(number)) if number.is_i64() || number.is_u64() => {
            match number.as_i64() {
                Some(value) => (false, 0, value),
                None => (true, number.as_u64().unwrap_or(0), 0),
            }
        }
        _ => (false, 0, 0),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefFailure {
    pub observation: crate::core::Observation,
    pub workflow: String,
    pub lane: WatchedRef,
    pub conclusion: String,
}

/// Every watched lane whose newest decisive run failed. A run still going, a
/// cancelled run and a stale one say nothing, so they neither raise nor clear.
pub fn watched_failures(
    checks: &[crate::core::Observation],
    branches: &[&str],
    tags: &[&str],
) -> Vec<RefFailure> {
    let mut newest: Vec<(LaneKey, usize, String)> = Vec::new();
    let mut places: Vec<(LaneKey, WatchedRef)> = Vec::new();
    for (index, check) in checks.iter().enumerate() {
        let Some(conclusion) = ci_conclusion_of(check) else {
            continue;
        };
        if !FAILING.contains(&conclusion.as_str()) && !PASSING.contains(&conclusion.as_str()) {
            continue;
        }
        let Some(lane) = watched_ref(
            payload_str(&check.payload, "branch"),
            payload_str(&check.payload, "event"),
            branches,
            tags,
        ) else {
            continue;
        };
        if github_managed(&check.payload) {
            continue;
        }
        let key = LaneKey {
            project_id: check.project_id.clone(),
            workflow: ci_workflow_of(check),
            kind: lane.kind as u8,
            lane: lane.lane.clone(),
        };
        let replace = newest
            .iter()
            .find(|(held, _, _)| held == &key)
            .is_none_or(|(_, held, _)| ci_ran_at(check) > ci_ran_at(&checks[*held]));
        if replace {
            if let Some(slot) = newest.iter_mut().find(|(held, _, _)| held == &key) {
                slot.1 = index;
                slot.2 = conclusion;
                places
                    .iter_mut()
                    .find(|(held, _)| held == &key)
                    .expect("a lane was stored with the run")
                    .1 = lane;
            } else {
                newest.push((key.clone(), index, conclusion));
                places.push((key, lane));
            }
        }
    }
    newest
        .into_iter()
        .filter(|(_, _, conclusion)| FAILING.contains(&conclusion.as_str()))
        .map(|(key, index, conclusion)| RefFailure {
            observation: checks[index].clone(),
            workflow: key.workflow.clone(),
            lane: places
                .iter()
                .find(|(held, _)| held == &key)
                .expect("a lane was stored with the run")
                .1
                .clone(),
            conclusion,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LaneKey {
    project_id: Option<String>,
    workflow: String,
    kind: u8,
    lane: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchState {
    Running,
    Passed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchRun {
    pub observation: crate::core::Observation,
    pub workflow: String,
    pub conclusion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCi {
    pub branch: String,
    pub revision: String,
    pub state: BranchState,
    pub runs: Vec<BranchRun>,
    pub failing: Vec<String>,
    pub concluded_at: Option<String>,
}

/// What CI says about `branch`'s newest observed revision, or `None` when no
/// run on the branch has been observed.
pub fn branch_ci(checks: &[crate::core::Observation], branch: &str) -> Option<BranchCi> {
    let on_branch: Vec<&crate::core::Observation> = checks
        .iter()
        .filter(|check| {
            payload_str(&check.payload, "branch") == Some(branch)
                && !ci_revision_of(check).is_empty()
        })
        .collect();
    if on_branch.is_empty() {
        return None;
    }
    let revision = ci_revision_of(
        on_branch
            .iter()
            .rev()
            .max_by_key(|check| ci_ran_at(check))
            .expect("non-empty"),
    );
    let mut latest: Vec<(String, &crate::core::Observation)> = Vec::new();
    for check in &on_branch {
        if ci_revision_of(check) != revision {
            continue;
        }
        let workflow = ci_workflow_of(check);
        let replace = latest
            .iter()
            .find(|(held, _)| held == &workflow)
            .is_none_or(|(_, held)| ci_ran_at(check) > ci_ran_at(held));
        if replace {
            if let Some(slot) = latest.iter_mut().find(|(held, _)| held == &workflow) {
                slot.1 = check;
            } else {
                latest.push((workflow, check));
            }
        }
    }
    latest.sort_by(|left, right| left.0.cmp(&right.0));
    let runs: Vec<BranchRun> = latest
        .into_iter()
        .map(|(workflow, check)| BranchRun {
            observation: (*check).clone(),
            workflow,
            conclusion: ci_conclusion_of(check),
        })
        .collect();
    let failing: Vec<String> = runs
        .iter()
        .filter(|run| {
            run.conclusion
                .as_deref()
                .is_some_and(|c| FAILING.contains(&c))
        })
        .map(|run| run.workflow.clone())
        .collect();
    let state = if runs.iter().any(|run| run.conclusion.is_none()) {
        BranchState::Running
    } else if !failing.is_empty() {
        BranchState::Failed
    } else if runs
        .iter()
        .all(|run| matches!(run.conclusion.as_deref(), Some("cancelled" | "stale")))
    {
        BranchState::Cancelled
    } else {
        BranchState::Passed
    };
    let concluded_at = if state == BranchState::Running {
        None
    } else {
        let stamps: Vec<&str> = checks
            .iter()
            .filter(|check| {
                payload_str(&check.payload, "branch") == Some(branch)
                    && ci_revision_of(check) == revision
            })
            .filter_map(|check| {
                check
                    .payload
                    .get("updated_at")
                    .and_then(serde_json::Value::as_str)
            })
            .collect();
        if stamps.is_empty() {
            None
        } else if stamps.iter().all(|stamp| stamp.is_empty()) {
            Some(String::new())
        } else {
            stamps.iter().copied().max().map(str::to_string)
        }
    };
    Some(BranchCi {
        branch: branch.to_string(),
        revision,
        state,
        runs,
        failing,
        concluded_at,
    })
}

#[cfg(test)]
mod ci_alert_tests {
    use super::*;

    fn check(
        branch: &str,
        event: &str,
        name: &str,
        conclusion: &str,
        updated: &str,
    ) -> crate::core::Observation {
        crate::core::Observation {
            id: String::new(),
            sweep_id: String::new(),
            collector: "ci".to_string(),
            kind: "check".to_string(),
            project_id: Some("p".to_string()),
            subject_key: String::new(),
            payload: serde_json::json!({"branch": branch, "event": event, "check": name, "conclusion": conclusion, "updated_at": updated, "revision": "abc"}),
            content_digest: String::new(),
            source_url: None,
            promoted: false,
            observed_at: crate::core::from_iso("2026-10-09T00:00:00Z").unwrap(),
        }
    }

    #[test]
    fn a_pull_request_and_a_dependabot_run_are_not_watched() {
        let branches = ["main", "master", "prod"];
        let tags = ["v*"];
        assert!(watched_ref(Some("main"), Some("pull_request"), &branches, &tags).is_none());
        assert!(watched_ref(Some("main"), Some("dynamic"), &branches, &tags).is_none());
        assert!(watched_ref(Some("wi-7"), Some("push"), &branches, &tags).is_none());
        assert!(watched_ref(None, Some("push"), &branches, &tags).is_none());
        let tag = watched_ref(Some("v0.7.2"), Some("push"), &branches, &tags).unwrap();
        assert_eq!(tag.kind, LaneKind::Tag);
        assert_eq!(tag.lane, "v*");
        assert!(github_managed(
            &serde_json::json!({"check": "npm in /web - Update #1606151494"})
        ));
        assert!(!github_managed(
            &serde_json::json!({"event": "push", "check": "build"})
        ));
    }

    #[test]
    fn a_later_success_clears_the_alert_and_a_cancelled_run_does_not() {
        let branches = ["main"];
        let tags = ["v*"];
        let failed = check("main", "push", "build", "failure", "2026-10-09T01:00:00Z");
        let cancelled = check("main", "push", "build", "cancelled", "2026-10-09T02:00:00Z");
        assert_eq!(
            watched_failures(&[failed.clone(), cancelled], &branches, &tags).len(),
            1
        );
        let fixed = check("main", "push", "build", "success", "2026-10-09T03:00:00Z");
        assert!(watched_failures(&[failed, fixed], &branches, &tags).is_empty());
    }

    #[test]
    fn a_bound_branch_reports_its_newest_revision() {
        let mut older = check(
            "feature",
            "push",
            "build",
            "failure",
            "2026-10-09T01:00:00Z",
        );
        older.payload["revision"] = "old".into();
        let head = check(
            "feature",
            "push",
            "build",
            "success",
            "2026-10-09T02:00:00Z",
        );
        let found = branch_ci(&[older, head], "feature").unwrap();
        assert_eq!(found.revision, "abc");
        assert_eq!(found.state, BranchState::Passed);
        assert!(branch_ci(&[], "feature").is_none());
    }

    #[test]
    fn a_character_class_matches_and_the_first_maximum_wins() {
        let tags = ["v[0-9]*"];
        let matched = watched_ref(Some("v1.2.3"), Some("push"), &[], &tags).unwrap();
        assert_eq!(matched.kind, LaneKind::Tag);
        assert!(watched_ref(Some("vA"), Some("push"), &[], &tags).is_none());
        assert!(watched_ref(Some("]x"), Some("push"), &[], &["[]]x"]).is_some());

        let first = check("main", "push", "build", "failure", "2026-10-09T01:00:00Z");
        let mut second = check("main", "push", "build", "failure", "2026-10-09T01:00:00Z");
        second.project_id = None;
        let found = watched_failures(&[first.clone(), second], &["main"], &[]);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].observation.project_id.as_deref(), Some("p"));
        assert!(found[1].observation.project_id.is_none());

        // A later run on the same lane replaces the ref the earlier one matched.
        let mut v1 = check("v1", "push", "build", "failure", "2026-10-09T01:00:00Z");
        let mut v2 = check("v2", "push", "build", "failure", "2026-10-09T02:00:00Z");
        v1.payload["branch"] = serde_json::json!("v1");
        v2.payload["branch"] = serde_json::json!("v2");
        let replaced = watched_failures(&[v1, v2], &[], &["v*"]);
        assert_eq!(replaced.len(), 1);
        assert_eq!(replaced[0].lane.reference, "v2");

        // Runs come out sorted by workflow, not in the order they were seen.
        let test = check("main", "push", "test", "success", "2026-10-09T01:00:00Z");
        let build = check("main", "push", "build", "success", "2026-10-09T01:00:00Z");
        let ci = branch_ci(&[test, build], "main").unwrap();
        assert_eq!(
            ci.runs
                .iter()
                .map(|run| run.workflow.as_str())
                .collect::<Vec<_>>(),
            vec!["build", "test"]
        );
    }
}
