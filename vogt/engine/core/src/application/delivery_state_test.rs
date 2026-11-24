//! Ports `tests/test_delivery_state.py`: the pure delivery projection (WI-1330).

use super::*;
use crate::application::models::work::LaneDeployedState::{Deployed, NotDeployed, Unknown};

const SHA: &str = "abc1234";

fn landed() -> Landed {
    Landed {
        sha: SHA.to_string(),
        branch: Some("wi-7".to_string()),
        evidence: "branch wi-7 is on main".to_string(),
    }
}

fn lane(
    status: &str,
    behind: Option<i64>,
    unpromoted: &[&str],
    deployed: Option<&str>,
) -> LaneView {
    LaneView {
        name: "dev".to_string(),
        status: status.to_string(),
        commits_behind: behind,
        unpromoted_shas: unpromoted.iter().map(|sha| sha.to_string()).collect(),
        deployed_sha: deployed.map(str::to_string),
    }
}

fn state(lane: LaneView) -> LaneDeployedState {
    project(Some(landed()), &[lane], &[], &[]).deployed[0].state
}

fn record(day: i64, lane: &str) -> VerificationRecord {
    VerificationRecord {
        comment_id: format!("c{day}"),
        at: Moment::from_unix(1_790_000_000 + day * 86_400, 0),
        fields: VerificationFields {
            head_sha: None,
            lane: lane.to_string(),
            what: "it works".to_string(),
            evidence: format!("day {day}"),
            by: None,
        },
    }
}

#[test]
fn landed_only() {
    let delivery = project(Some(landed()), &[], &[], &[]);
    assert_eq!(delivery.landed, Some(landed()));
    assert!(delivery.deployed.is_empty());
    assert!(delivery.verified.is_none());
}

#[test]
fn nothing_landed_answers_no_lane() {
    assert!(project(None, &[lane("at_head", None, &[], None)], &[], &[])
        .deployed
        .is_empty());
}

#[test]
fn a_lane_at_head_is_deployed() {
    assert_eq!(state(lane("at_head", None, &[], Some("f00"))), Deployed);
}

#[test]
fn a_lane_behind_without_the_sha_is_deployed() {
    assert_eq!(
        state(lane("behind", Some(2), &["111", "222"], None)),
        Deployed
    );
}

#[test]
fn a_lane_behind_with_the_sha_is_not_deployed() {
    let longer = format!("{SHA}ffff");
    let behind = lane("behind", Some(2), &["111", longer.as_str()], None);
    let delivery = project(Some(landed()), &[behind], &[], &[]);
    assert_eq!(delivery.deployed[0].state, NotDeployed);
    assert!(delivery.deployed[0].evidence.contains(SHA));
}

#[test]
fn a_truncated_list_reads_unknown() {
    assert_eq!(state(lane("behind", Some(300), &["111"], None)), Unknown);
    assert_eq!(state(lane("behind", None, &["111"], None)), Unknown);
}

#[test]
fn diverged_is_not_deployed_and_the_rest_unknown() {
    assert_eq!(state(lane("diverged", None, &[], None)), NotDeployed);
    assert_eq!(state(lane("unknown", None, &[], None)), Unknown);
    assert_eq!(state(lane("not_collected", None, &[], None)), Unknown);
}

#[test]
fn verified_is_the_newest_on_an_allowed_lane() {
    let records = [record(1, "dev"), record(3, "staging"), record(2, "dev")];
    let dev = vec!["dev".to_string()];
    let verified = project(None, &[], &records, &dev).verified.unwrap();
    assert_eq!(verified.comment_id, "c2");
    let newest = project(None, &[], &records, &[]).verified.unwrap();
    assert_eq!(newest.comment_id, "c3");
    assert!(project(None, &[], &records, &["prod".to_string()])
        .verified
        .is_none());
}

#[test]
fn only_features_and_bugs_need_verification_under_the_policy() {
    let on = VerificationPolicy {
        required_for_done: true,
        lanes: Vec::new(),
    };
    assert!(needs_verification("feature", Some(&on)));
    assert!(needs_verification("bug", Some(&on)));
    assert!(!needs_verification("chore", Some(&on)));
    assert!(!needs_verification("question", Some(&on)));
    assert!(!needs_verification(
        "feature",
        Some(&VerificationPolicy::default())
    ));
    assert!(!needs_verification("feature", None));
}

#[test]
fn a_stored_policy_is_read_with_its_lanes() {
    let stored = serde_json::json!({"required_for_done": true, "lanes": ["dev"]});
    let policy = VerificationPolicy::from_value(Some(&stored)).unwrap();
    assert!(policy.required_for_done);
    assert_eq!(policy.lanes, ["dev"]);
    assert!(VerificationPolicy::from_value(None).is_none());
}
