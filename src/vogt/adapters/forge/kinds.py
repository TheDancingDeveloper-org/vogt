"""The observation kinds and collector names the forge layer produces.

Canonical here (D4/#175): everything outside `adapters/forge` reads these
names from this module, so no service knows which forge — or which adapter
package — produced them. `adapters/github` re-exports them for its own use.

Kinds are stable across providers: a GitLab merge request and a GitHub pull
request are both `forge.pull_request`. Collector *names* are durable
identifiers too — FR-R6 retirement and drift evidence read them — so the old
`gh-*` names keep resolving through the alias map (`sync.current_collector`).
"""

from __future__ import annotations

# -- observation kinds -----------------------------------------------------

KIND_ISSUE = "forge.issue"
KIND_PULL_REQUEST = "forge.pull_request"
KIND_CHECK = "ci.check"
KIND_RELEASE = "release"
KIND_POSTURE = "forge.posture"
KIND_NOTIFICATION = "forge.notification"
KIND_LABEL = "forge.label"
#: The per-project sync receipt (the `dep_scan` pattern), FR-O4/O10/O11.
KIND_SYNC = "forge.sync"

# -- collector names -------------------------------------------------------

COLLECTOR_ISSUES = "forge-issues"
COLLECTOR_PULLS = "forge-prs"
COLLECTOR_CHECKS = "forge-checks"
COLLECTOR_RELEASES = "forge-releases"
COLLECTOR_POSTURE = "forge-posture"
COLLECTOR_NOTIFICATIONS = "forge-notifications"
COLLECTOR_LABELS = "forge-labels"

#: Old collector names → the ones that replaced them (D4). A drift proposal or
#: retirement lookup raised before a rename carries the old name; coverage now
#: records only the new one, so these keep the pre-rename paths alive.
COLLECTOR_ALIASES: dict[str, str] = {
    "gh-issues": COLLECTOR_ISSUES,
    "gh-prs": COLLECTOR_PULLS,
    "gh-actions": COLLECTOR_CHECKS,
    "gh-releases": COLLECTOR_RELEASES,
    "gh-posture": COLLECTOR_POSTURE,
    "gh-notifications": COLLECTOR_NOTIFICATIONS,
    "gh-consolidate": COLLECTOR_ISSUES,
}


def current_collector(name: str) -> str:
    """The live collector name for a possibly-renamed one (identity if new)."""
    return COLLECTOR_ALIASES.get(name, name)


__all__ = [
    "COLLECTOR_ALIASES",
    "COLLECTOR_CHECKS",
    "COLLECTOR_ISSUES",
    "COLLECTOR_LABELS",
    "COLLECTOR_NOTIFICATIONS",
    "COLLECTOR_POSTURE",
    "COLLECTOR_PULLS",
    "COLLECTOR_RELEASES",
    "KIND_CHECK",
    "KIND_ISSUE",
    "KIND_LABEL",
    "KIND_NOTIFICATION",
    "KIND_POSTURE",
    "KIND_PULL_REQUEST",
    "KIND_RELEASE",
    "KIND_SYNC",
    "current_collector",
]
