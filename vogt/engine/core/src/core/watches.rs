//! Pure watch trigger decisions and terminal-safe notices, matching core/watches.py.
use super::Observation;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn pr_outcome(payload: &Value, on: &Value, url: &Option<String>) -> Option<Value> {
    let state = payload["state"].as_str()?;
    on.as_array()?.iter().any(|s| s == state).then(|| {
        json!({
            "state": state, "number": payload["number"], "title": payload["title"],
            "head": payload["head"], "merged_by": payload["merged_by"], "url": url
        })
    })
}

pub fn ci_outcome(checks: &[Observation], revision: &str, workflows: &[Value]) -> Option<Value> {
    let mut latest: BTreeMap<&str, &Observation> = BTreeMap::new();
    for check in checks {
        let p = &check.payload;
        let Some(observed) = p["revision"].as_str() else {
            continue;
        };
        if observed.is_empty()
            || revision.len() < 7
            || !(observed.starts_with(revision) || revision.starts_with(observed))
        {
            continue;
        }
        let Some(name) = p["check"].as_str().filter(|s| !s.is_empty()) else {
            continue;
        };
        if !workflows.is_empty() && !workflows.iter().any(|w| w == name) {
            continue;
        }
        let ran_at = |c: &Observation| {
            (
                c.payload["updated_at"].as_str().unwrap_or("").to_owned(),
                c.payload["run_number"].as_i64().unwrap_or(0),
                c.observed_at,
            )
        };
        if latest
            .get(name)
            .is_none_or(|held| ran_at(check) > ran_at(held))
        {
            latest.insert(name, check);
        }
    }
    if latest.is_empty()
        || workflows
            .iter()
            .any(|w| !latest.contains_key(w.as_str().unwrap_or("")))
    {
        return None;
    }
    let mut failing = Vec::new();
    let mut cancelled = true;
    for (name, check) in &latest {
        let conclusion = check.payload["conclusion"]
            .as_str()
            .filter(|s| !s.is_empty())?;
        if [
            "failure",
            "timed_out",
            "startup_failure",
            "action_required",
            "error",
        ]
        .contains(&conclusion)
        {
            failing.push(*name);
        }
        cancelled &= ["cancelled", "stale"].contains(&conclusion);
    }
    let url = failing
        .iter()
        .find_map(|name| latest[name].source_url.clone());
    Some(
        json!({"state": if !failing.is_empty() { "failed" } else if cancelled { "cancelled" } else { "passed" }, "revision": revision, "checks": latest.keys().collect::<Vec<_>>(), "failing": failing, "url": url}),
    )
}

pub fn notice(kind: &str, description: &str, outcome: &Value, note: &str) -> String {
    let state = outcome["state"].as_str().unwrap_or("").to_uppercase();
    let verdict = match kind {
        "pr" => format!(
            "{}{}",
            state,
            outcome["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" ({s})"))
                .unwrap_or_default()
        ),
        "ci" => {
            let failing = outcome["failing"].as_array().cloned().unwrap_or_default();
            let detail = if failing.is_empty() {
                format!(
                    "; {} check(s)",
                    outcome["checks"].as_array().map_or(0, Vec::len)
                )
            } else {
                format!(
                    "; failing: {}",
                    failing
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let url = outcome["url"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| format!(" — {s}"))
                .unwrap_or_default();
            format!(
                "CI {state} @ {}{detail}{url}",
                outcome["revision"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>()
            )
        }
        _ => state,
    };
    let suffix = if note.is_empty() {
        String::new()
    } else {
        format!(". {note}")
    };
    format!("[vogt] watch fired: {description} -> {verdict}{suffix}")
        .chars()
        .filter(|c| crate::core::watches::printable(*c))
        .take(500)
        .collect()
}

fn printable(c: char) -> bool {
    // Python str.isprintable excludes Unicode separators and Other categories.
    c == ' '
        || !matches!(
            unicode_general_category::get_general_category(c),
            unicode_general_category::GeneralCategory::Control
                | unicode_general_category::GeneralCategory::Format
                | unicode_general_category::GeneralCategory::Surrogate
                | unicode_general_category::GeneralCategory::PrivateUse
                | unicode_general_category::GeneralCategory::Unassigned
                | unicode_general_category::GeneralCategory::LineSeparator
                | unicode_general_category::GeneralCategory::ParagraphSeparator
                | unicode_general_category::GeneralCategory::SpaceSeparator
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(name: &str, conclusion: &str, run: i64) -> Observation {
        Observation {
            id: format!("{name}-{run}"),
            sweep_id: String::new(),
            collector: String::new(),
            kind: "ci.check".into(),
            project_id: None,
            subject_key: String::new(),
            payload: json!({"revision":"abcdef1234", "check":name,"conclusion":conclusion,"run_number":run}),
            content_digest: String::new(),
            source_url: Some("https://example.test/run".into()),
            promoted: false,
            observed_at: super::super::Moment::from_unix(1, 0),
        }
    }
    #[test]
    fn ci_waits_for_required_checks_and_uses_latest_run_number() {
        let checks = vec![check("build", "failure", 1), check("build", "success", 2)];
        assert_eq!(
            ci_outcome(&checks, "abcdef1", &[]).unwrap()["state"],
            "passed"
        );
        assert!(ci_outcome(&checks, "abcdef1", &[json!("test")]).is_none());
        assert!(ci_outcome(&checks, "abcdef", &[]).is_none());
        assert!(ci_outcome(&[check("test", "", 1)], "abcdef1", &[]).is_none());
        assert_eq!(
            ci_outcome(&[check("test", "timed_out", 1)], "abcdef1", &[]).unwrap()["state"],
            "failed"
        );
        assert_eq!(
            ci_outcome(&[check("test", "cancelled", 1)], "abcdef1", &[]).unwrap()["state"],
            "cancelled"
        );
    }
    #[test]
    fn pr_and_notice_are_exact_and_terminal_safe() {
        let payload = json!({"state":"merged", "number":1,"title":"hello\u{001b}\r\n\u{200b}"});
        assert!(pr_outcome(&payload, &json!(["closed"]), &None).is_none());
        let outcome = pr_outcome(&payload, &json!(["merged"]), &None).unwrap();
        assert_eq!(
            notice("pr", "PR #1 in project", &outcome, "note"),
            "[vogt] watch fired: PR #1 in project -> MERGED (hello). note"
        );
        assert_eq!(
            notice(
                "session",
                "session x",
                &json!({"state":"gone"}),
                &"x".repeat(700)
            )
            .chars()
            .count(),
            500
        );
    }
}
