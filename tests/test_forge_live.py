"""The opt-in live forge suite (#294).

Everything in `tests/test_forge_provider.py` runs against a fake transport, so
nothing there touches GitHub's real pagination, PAT scopes, check runs or the
write-back verbs. These tests close that gap by reading and writing the real
`TheDancingDeveloper-org/vogt-fixture` repository — the one
`scripts/fixture_reset.py` rebuilds to the manifest's known state.

They are **opt-in and skipped by default**. Two guards stand between them and
an ordinary `uv run pytest`:

1. `conftest.pytest_collection_modifyitems` skips every `live_forge` item
   unless the run selected them with `-m live_forge`.
2. `_live_provider_and_ref` skips the individual test when no token is
   configured, so `pytest -m live_forge` on a machine without credentials
   skips cleanly rather than erroring.

The manifest-consistency tests below carry no marker, so they *do* run in the
ordinary suite: they assert the fixture manifest is internally coherent, which
is what the live reads are checked against, without a network.
"""

from __future__ import annotations

import json
import os
from pathlib import Path

import pytest

from vogt.adapters.forge import GitHubProvider, RepoRef
from vogt.adapters.github.client import GitHubClient, GitHubUnavailable

MANIFEST_PATH = Path(__file__).parent / "fixtures" / "forge_fixture_manifest.json"


def _manifest() -> dict[str, object]:
    data: dict[str, object] = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    return data


# -- ordinary tests: the manifest the live reads are graded against -------


def test_manifest_counts_match_its_own_declared_items() -> None:
    manifest = _manifest()
    expected = manifest["expected"]
    assert isinstance(expected, dict)
    issues = manifest["issues"]
    pulls = manifest["pulls"]
    assert isinstance(issues, list)
    assert isinstance(pulls, list)

    open_issues = sum(1 for i in issues if i["state"] == "open")
    closed_issues = sum(1 for i in issues if i["state"] == "closed")
    assert open_issues == expected["issues_open"]
    assert closed_issues == expected["issues_closed"]
    assert len(issues) == expected["issues_total"]

    assert len(manifest["labels"]) == expected["labels_total"]  # type: ignore[arg-type]
    assert len(pulls) == expected["pulls_total"]
    assert sum(1 for p in pulls if p.get("draft")) == expected["pulls_draft"]
    assert sum(1 for p in pulls if p.get("closes")) == expected["pulls_with_closes"]
    assert len(manifest["branches"]) == expected["branches_total"]  # type: ignore[arg-type]


def test_manifest_branches_follow_the_283_naming_pattern() -> None:
    """Each fixture branch names the forge number of the item it binds (#283)."""
    manifest = _manifest()
    branches = manifest["branches"]
    assert isinstance(branches, list)
    for branch in branches:
        number = branch["binds_issue"]
        assert str(number) in branch["name"], branch


def test_live_forge_marker_is_registered(pytestconfig: pytest.Config) -> None:
    """The marker exists, so `-m live_forge` is meaningful and not a typo."""
    markers = pytestconfig.getini("markers")
    assert any(str(entry).startswith("live_forge") for entry in markers)


# -- the opt-in live tests ------------------------------------------------


def _live_provider_and_ref() -> tuple[GitHubProvider, RepoRef]:
    token = os.environ.get("VOGT_FIXTURE_TOKEN") or os.environ.get("GH_TOKEN")
    if not token:
        pytest.skip("live_forge needs VOGT_FIXTURE_TOKEN or GH_TOKEN in the env")
    manifest = _manifest()
    repo = os.environ.get("VOGT_FIXTURE_REPO") or str(manifest["repo"])
    provider = GitHubProvider(GitHubClient(token=token))
    ref = provider.parse(f"https://github.com/{repo}")
    if ref is None:  # pragma: no cover - a github.com slug always parses
        pytest.skip(f"{repo!r} is not a github.com repository")
    # The fixture repository is created once, by a human (see
    # docs/FORGE_FIXTURE.md). Until it exists — or if GitHub is unreachable —
    # the opt-in run skips cleanly rather than failing on an empty read.
    try:
        if provider.describe(ref) is None:
            pytest.skip(f"fixture repo {repo!r} not present; run fixture_reset.py")
    except GitHubUnavailable as exc:
        pytest.skip(f"github.com unreachable or token unscoped for {repo!r}: {exc}")
    return provider, ref


@pytest.mark.live_forge
def test_live_reads_match_the_manifest() -> None:
    """Real pagination + normalisation land the manifest's declared counts."""
    manifest = _manifest()
    expected = manifest["expected"]
    assert isinstance(expected, dict)
    provider, ref = _live_provider_and_ref()

    issues = list(provider.issues_updated_since(ref, since=None))
    open_issues = [i for i in issues if i.state == "open"]
    closed_issues = [i for i in issues if i.state == "closed"]
    assert len(open_issues) == expected["issues_open"]
    assert len(closed_issues) == expected["issues_closed"]

    labels_seen = {name for issue in issues for name in issue.labels}
    assert set(expected["issue_labels_present"]) <= labels_seen

    pulls = list(provider.pulls_updated_since(ref, since=None))
    assert sum(1 for pull in pulls if pull.draft) >= expected["pulls_draft"]

    labels = list(provider.labels(ref))
    assert len(labels) >= expected["labels_total"]


@pytest.mark.live_forge
def test_live_writeback_appends_a_comment() -> None:
    """The one write path: a comment posts and reports a `gh:` subject key."""
    manifest = _manifest()
    target = manifest["writeback_target"]
    assert isinstance(target, dict)
    provider, ref = _live_provider_and_ref()

    result = provider.comment(
        ref,
        int(target["issue_number"]),
        "live_forge probe: write-back reachable (safe to delete)",
    )
    assert result.outcome == "succeeded"
    assert result.subject_key is not None
    assert result.subject_key.startswith("gh:")
