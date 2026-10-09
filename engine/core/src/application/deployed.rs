//! `deployed.versions`: is each deployment lane at its branch head? (WI-855)
//! Ports `src/vogt/application/services/deployed.py`.
//!
//! A read path: no forge or network call happens here. The `deploy-lanes`
//! collector reads each configured lane's evidence, and this joins that
//! observation to the declared store. A lane configured but not yet swept is
//! reported `not_collected`, never as "at head".

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};

use super::context::{AppContext, Built};
const KIND_DEPLOY_LANE: &str = "deploy.lane";
use crate::config::DeployLane;
use crate::core::Observation;
use crate::errors::VogtError;
use crate::storage::interface::{DeclaredStore, ObservedStore, ReadView};

/// What each configured lane runs, against its branch head.
pub fn deployed_versions_op(ctx: &Built, params: Value) -> Result<Value, VogtError> {
    match ctx {
        Built::SystemRandom(ctx) => versions(ctx, params),
        Built::SystemSequential(ctx) => versions(ctx, params),
        Built::StepRandom(ctx) => versions(ctx, params),
        Built::StepSequential(ctx) => versions(ctx, params),
    }
}

#[derive(serde::Deserialize)]
struct DeployedVersionsParams {
    project: Option<String>,
    lane: Option<String>,
}

fn versions<C: crate::core::Clock, I: crate::core::IdFactory>(
    ctx: &AppContext<C, I>,
    params: Value,
) -> Result<Value, VogtError> {
    let params: DeployedVersionsParams = serde_json::from_value(params.or_object())
        .map_err(|err| VogtError::InvalidRequest(err.to_string()))?;
    let configured = ctx.config.deploy_lanes.len();
    if configured == 0 {
        return Ok(json!({
            "lanes": [],
            "configured": 0,
            "detail": "deploy_lanes is not configured, so deployed versions are not collected",
            "freshness": crate::application::services::freshness::freshness_of(
                &ctx.observed,
                crate::application::services::now_of(&ctx.clock),
            )?,
        }));
    }
    let mut lanes = ctx.config.deploy_lanes.clone();
    let view = ctx.declared.read()?;
    if let Some(project) = params.project {
        let slug = super::resolve::project(&view, &project)?.slug;
        lanes.retain(|lane| lane.project == slug);
    }
    if let Some(name) = params.lane {
        lanes.retain(|lane| lane.name == name);
    }
    let observed = ctx.observed.latest(
        &[KIND_DEPLOY_LANE.to_string()],
        None,
        false,
        false,
        (lanes.len() * 4).max(50) as i64,
    )?;
    let views: Vec<Value> = lanes
        .iter()
        .map(|lane| {
            let key = format!("deploy:{}/{}", lane.project, lane.name);
            let observation = observed.iter().find(|row| row.subject_key == key);
            lane_view(&view, lane, observation)
        })
        .collect::<Result<_, _>>()?;
    let detail = if views.is_empty() {
        json!("no configured lane matches the filter")
    } else {
        Value::Null
    };
    Ok(json!({
        "lanes": views,
        "configured": configured,
        "detail": detail,
        "freshness": crate::application::services::freshness::freshness_of(
            &ctx.observed,
            crate::application::services::now_of(&ctx.clock),
        )?,
    }))
}

fn lane_view(
    view: &impl ReadView,
    lane: &DeployLane,
    observation: Option<&Observation>,
) -> Result<Value, VogtError> {
    let Some(observation) = observation else {
        return Ok(json!({
            "name": lane.name,
            "lane": lane.name,
            "project_slug": lane.project,
            "branch": lane.branch,
            "status": "not_collected",
            "detail": "the deploy-lanes collector has not read this lane yet",
        }));
    };
    let payload = &observation.payload;
    let receipt = as_object(payload.get("receipt"));
    let live = as_object(payload.get("live"));
    let compare = as_object(payload.get("compare"));
    let mut commits: Vec<Value> = Vec::new();
    if let Some(compare) = compare {
        for raw in compare
            .get("commits")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(sha) = raw.get("sha").and_then(Value::as_str) else {
                continue;
            };
            if sha.is_empty() {
                continue;
            }
            let subject = raw.get("subject").and_then(Value::as_str).unwrap_or("");
            commits.push(json!({
                "sha": sha,
                "subject": subject,
                "work_items": work_refs(subject),
            }));
        }
    }
    let mut refs: BTreeSet<String> = BTreeSet::new();
    for commit in &commits {
        for item in commit["work_items"].as_array().into_iter().flatten() {
            if let Some(text) = item.as_str() {
                refs.insert(text.to_string());
            }
        }
    }
    let mut items: Vec<Value> = Vec::new();
    for reference in &refs {
        let item = view.work_item_by_ref(reference)?;
        items.push(json!({
            "ref": reference,
            "title": item.as_ref().map(|item| item.title.clone()),
            "state": item.as_ref().map(|item| item.state.to_string()),
        }));
    }
    let details: Vec<&str> = ["live_detail", "receipt_detail", "compare_detail"]
        .into_iter()
        .filter_map(|key| payload.get(key).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .collect();
    let ahead = compare.and_then(|object| integer(object.get("ahead_by")));
    let deployed_from = payload
        .get("deployed_from")
        .and_then(Value::as_str)
        .filter(|text| *text == "live" || *text == "receipt");
    Ok(json!({
        "name": lane.name,
        "lane": lane.name,
        "project_slug": lane.project,
        "branch": lane.branch,
        "status": status(compare),
        "deployed_sha": text(payload.get("deployed_sha")),
        "deployed_from": deployed_from,
        "version": live.and_then(|object| text(object.get("version"))),
        "source_tag": receipt.and_then(|object| text(object.get("source_tag"))),
        "receipt_status": receipt.and_then(|object| text(object.get("status"))),
        "receipt_at": receipt.and_then(|object| text(object.get("timestamp"))),
        "live_sha": live.and_then(|object| text(object.get("source_sha"))),
        "head_sha": compare.and_then(|object| text(object.get("head_sha"))),
        "commits_behind": ahead,
        "unpromoted_commits": commits,
        "unpromoted_work_items": items,
        "truncated": compare.is_some_and(|object| object.get("truncated").and_then(Value::as_bool).unwrap_or(false)),
        "observed_at": observation.observed_at,
        "detail": if details.is_empty() { Value::Null } else { json!(details.join("; ")) },
    }))
}

/// `at_head`, `behind`, `diverged`, `unknown`, or `not_collected`.
fn status(compare: Option<&Map<String, Value>>) -> &'static str {
    let Some(compare) = compare else {
        return "unknown";
    };
    let state = compare.get("status").and_then(Value::as_str);
    let ahead = integer(compare.get("ahead_by"));
    let behind = integer(compare.get("behind_by")).unwrap_or(0);
    if state == Some("diverged") || (ahead.unwrap_or(0) > 0 && behind > 0) {
        return "diverged";
    }
    if state == Some("identical") || ahead == Some(0) {
        // A deployed revision *ahead* of the branch (behind_by > 0, nothing
        // ahead) is not on it: deployed from somewhere else.
        return if behind > 0 { "diverged" } else { "at_head" };
    }
    if ahead.is_some_and(|count| count > 0) {
        return "behind";
    }
    "unknown"
}

fn work_refs(subject: &str) -> Vec<String> {
    let mut found: BTreeSet<String> = BTreeSet::new();
    let bytes = subject.as_bytes();
    let mut index = 0;
    while index + 3 < bytes.len() {
        if bytes[index..].starts_with(b"WI-")
            && (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        {
            let start = index;
            index += 3;
            let digits = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            if index > digits && (index == bytes.len() || !bytes[index].is_ascii_alphanumeric()) {
                found.insert(subject[start..index].to_string());
                continue;
            }
        }
        index += 1;
    }
    found.into_iter().collect()
}

fn as_object(value: Option<&Value>) -> Option<&Map<String, Value>> {
    value.and_then(Value::as_object)
}

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value.and_then(Value::as_i64)
}

trait OrObject {
    fn or_object(self) -> Value;
}
impl OrObject for Value {
    fn or_object(self) -> Value {
        if self.is_null() {
            json!({})
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DeployLane;
    use crate::core::{ActorKind, Principal, SequentialIds, StepClock};

    fn moment() -> crate::core::Moment {
        crate::core::Moment::from_unix(1_700_000_000, 0)
    }

    fn context(lanes: Vec<DeployLane>) -> Built {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "vogt-deployed-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::VogtConfig {
            data_dir: dir,
            deploy_lanes: lanes,
            ..crate::config::VogtConfig::default()
        };
        let principal = Principal::new("local:test-user", ActorKind::Human, "Test").unwrap();
        let built = crate::application::context::build_context(
            config,
            Some(principal.clone()),
            Some(StepClock::new(moment())),
            Some(SequentialIds::new(None).unwrap()),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let Built::StepSequential(ctx) = &built else {
            unreachable!()
        };
        ctx.declared.migrate().unwrap();
        ctx.declared.bootstrap(&principal).unwrap();
        ctx.observed.migrate().unwrap();
        built
    }

    fn lane() -> DeployLane {
        DeployLane {
            name: "prod".to_string(),
            project: "vogt".to_string(),
            branch: "main".to_string(),
            receipt_repo: None,
            receipt_path: None,
            version_url: None,
        }
    }

    #[test]
    fn with_no_lanes_configured_nothing_is_collected() {
        let ctx = context(Vec::new());
        let result = deployed_versions_op(&ctx, Value::Null).unwrap();
        assert_eq!(result["configured"], 0);
        assert!(result["lanes"].as_array().unwrap().is_empty());
        assert!(result["detail"]
            .as_str()
            .unwrap()
            .contains("not configured"));
        assert_eq!(result["freshness"]["status"], "never_swept");
    }

    #[test]
    fn a_configured_lane_without_a_sweep_is_not_collected() {
        let ctx = context(vec![lane()]);
        let result = deployed_versions_op(&ctx, Value::Null).unwrap();
        assert_eq!(result["configured"], 1);
        assert_eq!(result["lanes"][0]["status"], "not_collected");
        assert_eq!(result["lanes"][0]["project_slug"], "vogt");
    }

    #[test]
    fn a_lane_name_filter_that_matches_nothing_says_so() {
        let ctx = context(vec![lane()]);
        let result = deployed_versions_op(&ctx, json!({"lane": "staging"})).unwrap();
        assert!(result["lanes"].as_array().unwrap().is_empty());
        assert!(result["detail"]
            .as_str()
            .unwrap()
            .contains("no configured lane matches"));
    }

    #[test]
    fn status_follows_the_compare() {
        assert_eq!(status(None), "unknown");
        assert_eq!(
            status(Some(&json!({"ahead_by": 0}).as_object().unwrap().clone())),
            "at_head"
        );
        assert_eq!(
            status(Some(&json!({"ahead_by": 3}).as_object().unwrap().clone())),
            "behind"
        );
        assert_eq!(
            status(Some(
                &json!({"ahead_by": 1, "behind_by": 1})
                    .as_object()
                    .unwrap()
                    .clone()
            )),
            "diverged"
        );
        assert_eq!(
            status(Some(
                &json!({"ahead_by": 0, "behind_by": 2})
                    .as_object()
                    .unwrap()
                    .clone()
            )),
            "diverged"
        );
    }

    #[test]
    fn work_refs_finds_bare_wi_numbers_and_ignores_glued_ones() {
        assert_eq!(work_refs("fixes WI-12 and WI-3"), ["WI-12", "WI-3"]);
        assert!(work_refs("notWI-12 or WI-").is_empty());
    }
}
