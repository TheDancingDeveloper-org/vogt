//! Observation kinds and collector names. Ports `adapters/forge/kinds.py`.
//!
//! Kinds are stable across providers: a GitLab merge request and a GitHub pull
//! request are both `forge.pull_request`. Collector names are durable too, so
//! the old `gh-*` names keep resolving through the alias map.

pub const KIND_ISSUE: &str = "forge.issue";
pub const KIND_PULL_REQUEST: &str = "forge.pull_request";
pub const KIND_CHECK: &str = "ci.check";
pub const KIND_RELEASE: &str = "release";
pub const KIND_POSTURE: &str = "forge.posture";
pub const KIND_NOTIFICATION: &str = "forge.notification";
pub const KIND_LABEL: &str = "forge.label";
/// The per-project sync receipt: "not collected" is never reported as
/// "nothing there".
pub const KIND_SYNC: &str = "forge.sync";
/// One configured deployment lane: its deployed revision and how far behind
/// its branch that is.
pub const KIND_DEPLOY_LANE: &str = "deploy.lane";

pub const COLLECTOR_ISSUES: &str = "forge-issues";
pub const COLLECTOR_PULLS: &str = "forge-prs";
pub const COLLECTOR_CHECKS: &str = "forge-checks";
pub const COLLECTOR_RELEASES: &str = "forge-releases";
pub const COLLECTOR_POSTURE: &str = "forge-posture";
pub const COLLECTOR_NOTIFICATIONS: &str = "forge-notifications";
pub const COLLECTOR_LABELS: &str = "forge-labels";
pub const COLLECTOR_DEPLOY_LANES: &str = "deploy-lanes";

/// Old collector names to the ones that replaced them. A drift proposal or
/// retirement lookup raised before a rename carries the old name; coverage now
/// records only the new one.
pub static COLLECTOR_ALIASES: &[(&str, &str)] = &[
    ("gh-issues", COLLECTOR_ISSUES),
    ("gh-prs", COLLECTOR_PULLS),
    ("gh-actions", COLLECTOR_CHECKS),
    ("gh-releases", COLLECTOR_RELEASES),
    ("gh-posture", COLLECTOR_POSTURE),
    ("gh-notifications", COLLECTOR_NOTIFICATIONS),
    ("gh-consolidate", COLLECTOR_ISSUES),
];

/// The live collector name for a possibly-renamed one (identity if new).
pub fn current_collector(name: &str) -> &str {
    COLLECTOR_ALIASES
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
        .unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_and_new_names_are_identity() {
        assert_eq!(current_collector("gh-actions"), "forge-checks");
        assert_eq!(current_collector("gh-consolidate"), "forge-issues");
        assert_eq!(current_collector("forge-issues"), "forge-issues");
        assert_eq!(current_collector("deploy-lanes"), "deploy-lanes");
    }
}
