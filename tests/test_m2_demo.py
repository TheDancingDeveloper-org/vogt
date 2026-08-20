"""The M2 demo — the MVP acceptance test.

From `ROADMAP.md`:

    Register a dozen real projects; sweep. The global bugs view shows GitHub
    issues *and* promoted markers from rustnzb and rustTorrent with freshness
    stamps and trust states; suppress a noisy marker and watch it leave the
    ranked view but stay in `observations`; `deps --project nzb-core` lists
    the projects referencing it; adopt one item into a ranked work item. Then
    re-run the whole suite with the network unplugged and the GitHub adapter
    disabled — everything except forge observations still works.

The estate is reproduced in miniature rather than pointed at the real one: a
test that depends on `~/Working` passing or failing is a test about somebody's
laptop. The shapes — a Cargo workspace whose members are also standalone
repositories, markers of both kinds, a forge that may or may not be there —
are the ones that motivated the design.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.github import GitHubClient
from vogt.adapters.github.client import Transport
from vogt.adapters.github.collectors import (
    GitHubActionsCollector,
    GitHubReleaseCollector,
)
from vogt.application.context import AppContext
from vogt.application.models import (
    AdoptParams,
    BacklogParams,
    BugsParams,
    DepsParams,
    ObservationsParams,
    RegisterProjectParams,
    SuppressParams,
    SweepParams,
)
from vogt.application.services import (
    adopt,
    backlog,
    bugs,
    deps,
    observations,
    register_project,
    suppress,
    sweep,
)
from vogt.collectors.registry import CollectorRegistry

WHY = "the MVP demo"

#: Seven `nzb-*` crates, a workspace that vendors them, and a few more
#: projects — the rustnzb three-forms situation that motivated FR-D8 and the
#: reference-level dependency model (DESIGN §3.5).
NZB_CRATES = ("nzb-core", "nzb-decode", "nzb-nntp", "nzb-postproc")
OTHER_PROJECTS = ("rustTorrent", "indexarr-rs", "egressy", "Rust-PAR2")

FORGE_ISSUES = {
    "rustnzb": [
        {
            "number": 12,
            "title": "Segment fetch retries forever",
            "state": "open",
            "labels": [{"name": "bug"}, {"name": "p1"}],
        }
    ],
    "rustTorrent": [
        {
            "number": 3,
            "title": "Peer handshake times out on IPv6",
            "state": "open",
            "labels": [{"name": "bug"}],
        }
    ],
}


def _estate(root: Path) -> list[tuple[str, Path]]:
    """Build a dozen projects on disk."""
    projects: list[tuple[str, Path]] = []

    workspace = root / "rustnzb"
    (workspace / "crates").mkdir(parents=True)
    (workspace / "Cargo.toml").write_text(
        "[workspace]\n"
        "members = " + json.dumps([f"crates/{name}" for name in NZB_CRATES]) + "\n\n"
        "[workspace.dependencies]\n"
        + "".join(f'{name} = {{ path = "../{name}" }}\n' for name in NZB_CRATES),
        encoding="utf-8",
    )
    (workspace / "src.rs").write_text(
        "// FIXME(vogt): retry budget is unbounded here\n"
        "// TODO: tidy this module up one day\n",
        encoding="utf-8",
    )
    projects.append(("rustnzb", workspace))

    for name in NZB_CRATES:
        crate = root / name
        crate.mkdir()
        (crate / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\n', encoding="utf-8"
        )
        projects.append((name, crate))

    for name in OTHER_PROJECTS:
        directory = root / name
        directory.mkdir()
        (directory / "notes.md").write_text(
            f"TODO(vogt): {name} needs a release checklist\n"
            "TODO: some day, maybe, tidy the logging\n",
            encoding="utf-8",
        )
        projects.append((name, directory))

    return projects


def _github(routes: dict[str, list[dict[str, Any]]]) -> Transport:
    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        if "/issues" in url:
            for repo, issues in routes.items():
                if f"/{repo}/" in url:
                    return 200, json.dumps(issues).encode("utf-8")
            return 200, b"[]"
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
        if "/vulnerability-alerts" in url or "/automated-security-fixes" in url:
            return 404, b""
        return 200, b"[]"

    return transport


@pytest.fixture
def estate(instance: AppContext, tmp_path: Path) -> AppContext:
    for name, path in _estate(tmp_path):
        register_project(
            instance,
            RegisterProjectParams(
                name=name,
                root_path=str(path),
                repo_url=f"https://github.com/TheDancingDeveloper-org/{name}",
                reason=WHY,
            ),
        )
    return instance


def _with_github(monkeypatch: pytest.MonkeyPatch) -> None:
    """Configure the optional adapter, without a network."""
    from vogt.adapters.forge.sync import forge_sync_collectors
    from vogt.application.services import collect as collect_service

    route = _github(FORGE_ISSUES)
    client = GitHubClient(token="x", transport=route)

    def configured(path: Any, *, transport: Any = None) -> GitHubClient:
        del path, transport
        return GitHubClient(token="ghp_fake", transport=route)

    monkeypatch.setattr(GitHubClient, "from_token_file", staticmethod(configured))

    def registry(ctx: AppContext) -> CollectorRegistry:
        built = CollectorRegistry()
        for collector in forge_sync_collectors(ctx.observed):
            built.add(collector)
        for collector in (
            GitHubActionsCollector(client),
            GitHubReleaseCollector(client),
        ):
            built.add(collector)
        return built

    monkeypatch.setattr(collect_service, "collector_registry", registry)


def test_m2_demo(estate: AppContext, monkeypatch: pytest.MonkeyPatch) -> None:
    with estate.declared.read() as view:
        assert view.counts().projects == 1 + len(NZB_CRATES) + len(OTHER_PROJECTS)

    _with_github(monkeypatch)
    result = sweep(estate, SweepParams(reason=WHY))
    assert result.projects == 9
    assert all(report.outcome == "ok" for report in result.reports)

    # The global bug view shows GitHub issues *and* promoted markers.
    found = bugs(estate, BugsParams(limit=50))
    titles = [entry.title for entry in found.items]
    assert any("Segment fetch retries forever" in title for title in titles), (
        "a GitHub issue labelled bug"
    )
    assert any("Peer handshake times out" in title for title in titles), (
        "from a second project"
    )
    assert any("retry budget is unbounded" in title for title in titles), (
        "a promoted FIXME marker"
    )
    assert not any("tidy this module up" in title for title in titles), (
        "an unpromoted marker never claims to be work"
    )

    # ...with freshness stamps and trust states.
    assert found.freshness.status == "fresh"
    assert found.freshness.oldest_relevant_sweep is not None
    assert {entry.trust_state for entry in found.items} == {"verified"}

    # Suppress a noisy marker: it leaves the ranked view, stays in evidence.
    noisy = next(
        entry
        for entry in backlog(estate, BacklogParams(limit=100)).items
        if "release checklist" in entry.title
    )
    suppress(
        estate,
        SuppressParams(subject=noisy.ref, reason="tracked in the release runbook"),
    )
    after = backlog(estate, BacklogParams(limit=100))
    assert noisy.ref not in [entry.ref for entry in after.items]
    assert after.suppressed == 1
    still_evidence = observations(
        estate, ObservationsParams(subject_key=noisy.ref, latest_only=False)
    )
    assert still_evidence.observations, "suppression hides; it does not delete"

    # `deps --project nzb-core` lists the projects referencing it.
    referencing = deps(estate, DepsParams(project="nzb-core"))
    assert [ref.from_project_slug for ref in referencing.referenced_by] == ["rustnzb"]
    outward = deps(estate, DepsParams(project="rustnzb"))
    assert {ref.to_project_slug for ref in outward.references_out} >= set(NZB_CRATES)

    # Adopt one item into a ranked work item.
    target = next(
        entry
        for entry in backlog(estate, BacklogParams(limit=100)).items
        if entry.origin == "observed" and "retry budget" in entry.title
    )
    adopted = adopt(
        estate, AdoptParams(subject=target.ref, reason="picking this up now")
    )
    assert adopted.item.origin == "adopted"
    assert adopted.inferred_kind == "bug"

    ranked_after = backlog(estate, BacklogParams(limit=100))
    refs = [entry.ref for entry in ranked_after.items]
    assert adopted.item.ref in refs, "the adopted item is ranked as declared work"
    assert target.ref not in refs, "and is not also listed as an observed subject"


def test_the_same_estate_works_with_no_forge_at_all(estate: AppContext) -> None:
    """NFR-PO1/PO2: unplug the network and the GitHub adapter is simply absent.

    The second half of the demo. Nothing is mocked away here — no token file
    is configured, so `github_collectors` returns nothing and only the three
    offline collectors run.
    """
    from vogt.adapters.github import github_collectors

    assert github_collectors(estate.config) == []

    result = sweep(estate, SweepParams(reason=WHY))
    assert {report.collector for report in result.reports} == {
        "git-local",
        "source-markers",
        "dep-refs",
        "mirrored-source",
    }
    assert all(report.outcome == "ok" for report in result.reports)

    # Everything except forge observations still works.
    found = bugs(estate, BugsParams(limit=50))
    assert any("retry budget is unbounded" in entry.title for entry in found.items)
    assert found.freshness.status == "fresh"

    referencing = deps(estate, DepsParams(project="nzb-core"))
    assert [ref.from_project_slug for ref in referencing.referenced_by] == ["rustnzb"]

    forge = observations(estate, ObservationsParams(kind="forge.issue"))
    assert forge.observations == [], (
        "forge subjects are not collected — which is a different answer from "
        "'there are none', and the freshness stamp says which collectors ran"
    )
    assert "forge-issues" not in found.freshness.collectors


def test_a_second_sweep_finds_nothing_new(estate: AppContext) -> None:
    """FR-O7, NFR-S2: growth is proportional to change, not to polling."""
    first = sweep(estate, SweepParams(offline_only=True, reason=WHY))
    second = sweep(estate, SweepParams(offline_only=True, reason=WHY))

    assert sum(report.new for report in first.reports) > 0
    assert sum(report.new for report in second.reports) == 0
    assert sum(report.unchanged for report in second.reports) == sum(
        report.new for report in first.reports
    )
