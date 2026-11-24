//! Where a work item's change has got to: landed, deployed, verified (WI-1330).
//! Ports `src/vogt/core/delivery_state.py`.
//!
//! Pure: everything here is computed from facts the caller already read — the
//! landed change, the deploy-lane views `deployed.versions` builds from the
//! collector's observations, and the item's `verification` comments. No git or
//! forge call happens here (design §2.5).
//!
//! `crate::delivery` is input delivery to sessions; this is unrelated.

use crate::core::Moment;

use super::models::views::VerificationFields;
use super::models::work::{Delivery, Landed, LaneDeployed, LaneDeployedState, Verified};

/// The kinds whose close can need a verification record. A chore or a
/// question delivers no behaviour a check could exercise, so it never does.
pub const VERIFIED_KINDS: [&str; 2] = ["feature", "bug"];

pub const VERIFICATION_REQUIRED_MESSAGE: &str =
    "post a verification comment (work_comment kind=verification), or ask a person to close it";

/// The parts of one `deployed.versions` lane the projection reads.
#[derive(Debug, Clone, Default)]
pub struct LaneView {
    pub name: String,
    /// `at_head`, `behind`, `diverged`, `unknown` or `not_collected`.
    pub status: String,
    pub commits_behind: Option<i64>,
    pub unpromoted_shas: Vec<String>,
    pub deployed_sha: Option<String>,
}

/// A `verification` comment, flattened for the projection.
#[derive(Debug, Clone)]
pub struct VerificationRecord {
    pub comment_id: String,
    pub at: Moment,
    pub fields: VerificationFields,
}

/// Project the three delivery facts. No landed change → no lane answers.
pub fn project(
    landed: Option<Landed>,
    lanes: &[LaneView],
    verifications: &[VerificationRecord],
    allowed_lanes: &[String],
) -> Delivery {
    let deployed = match &landed {
        None => Vec::new(),
        Some(landed) => lanes
            .iter()
            .map(|lane| lane_of(&landed.sha, lane))
            .collect(),
    };
    Delivery {
        landed,
        deployed,
        verified: verified_of(verifications, allowed_lanes),
    }
}

fn lane_of(sha: &str, lane: &LaneView) -> LaneDeployed {
    let name = &lane.name;
    if lane.status == "at_head" {
        let at = lane
            .deployed_sha
            .as_deref()
            .filter(|deployed| !deployed.is_empty())
            .map(|deployed| format!(" ({deployed})"))
            .unwrap_or_default();
        return LaneDeployed {
            lane: name.clone(),
            state: LaneDeployedState::Deployed,
            evidence: format!("lane {name} is at its branch head{at}"),
        };
    }
    if lane.status != "behind" {
        let state = if lane.status == "diverged" {
            LaneDeployedState::NotDeployed
        } else {
            LaneDeployedState::Unknown
        };
        return LaneDeployed {
            lane: name.clone(),
            state,
            evidence: format!("lane {name} is {}", lane.status),
        };
    }
    let listed = lane.unpromoted_shas.len() as i64;
    let behind = match lane.commits_behind {
        Some(behind) if behind <= listed => behind,
        _ => {
            return LaneDeployed {
                lane: name.clone(),
                state: LaneDeployedState::Unknown,
                evidence: format!(
                    "lane {name} is behind and its commit list is truncated, so the change \
                     may be among the unlisted ones"
                ),
            }
        }
    };
    if sha_in(sha, &lane.unpromoted_shas) {
        return LaneDeployed {
            lane: name.clone(),
            state: LaneDeployedState::NotDeployed,
            evidence: format!("{sha} is among lane {name}'s {behind} unpromoted commits"),
        };
    }
    LaneDeployed {
        lane: name.clone(),
        state: LaneDeployedState::Deployed,
        evidence: format!(
            "lane {name} is {behind} behind and {sha} is not among the unpromoted commits"
        ),
    }
}

/// A hand-off may name an abbreviated sha; match on the common prefix.
fn sha_in(sha: &str, shas: &[String]) -> bool {
    shas.iter().any(|one| {
        !one.is_empty()
            && !sha.is_empty()
            && (one.starts_with(sha) || sha.starts_with(one.as_str()))
    })
}

fn verified_of(records: &[VerificationRecord], allowed_lanes: &[String]) -> Option<Verified> {
    let mut newest_first: Vec<&VerificationRecord> = records.iter().collect();
    newest_first.sort_by_key(|record| std::cmp::Reverse(record.at));
    newest_first
        .into_iter()
        .find(|record| allowed_lanes.is_empty() || allowed_lanes.contains(&record.fields.lane))
        .map(|record| Verified {
            comment_id: record.comment_id.clone(),
            lane: record.fields.lane.clone(),
            what: record.fields.what.clone(),
            evidence: record.fields.evidence.clone(),
            by: record.fields.by.clone(),
            head_sha: record.fields.head_sha.clone(),
            at: record.at.to_json(),
        })
}

/// Whether closing an item of `kind` needs a verification record.
///
/// Only when the project's policy says `required_for_done`, and only for
/// `feature` and `bug` items (`VERIFIED_KINDS`).
pub fn needs_verification(kind: &str, policy: Option<&VerificationPolicy>) -> bool {
    policy.is_some_and(|policy| policy.required_for_done) && VERIFIED_KINDS.contains(&kind)
}

/// The project's `agent_policy.verification`: whether a close needs a record,
/// and on which lanes one counts (any lane when none are listed).
#[derive(Debug, Clone, Default)]
pub struct VerificationPolicy {
    pub required_for_done: bool,
    pub lanes: Vec<String>,
}

impl VerificationPolicy {
    /// Read the stored policy; a document that is not an object is no policy.
    pub fn from_value(value: Option<&serde_json::Value>) -> Option<Self> {
        let object = value?.as_object()?;
        Some(Self {
            required_for_done: object
                .get("required_for_done")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            lanes: object
                .get("lanes")
                .and_then(serde_json::Value::as_array)
                .map(|lanes| {
                    lanes
                        .iter()
                        .filter_map(|lane| lane.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
#[path = "delivery_state_test.rs"]
mod tests;
