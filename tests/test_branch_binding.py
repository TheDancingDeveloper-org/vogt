"""Branch ↔ work-item binding — declared, observed, and the drift between them.

The four things #283 promises, each with a test that would fail without it:

- the naming convention recognises each shape and refuses a look-alike;
- a Vogt-started session records the branch it will use on the item's overlay,
  audited and without duplicating on a second start;
- the `git-local` sweep observes every branch and records which item it binds
  to, with provenance;
- declared and observed are surfaced side by side, and a branch on one side
  only reads as drift rather than being merged away (FR-O2).
"""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    BindBranchParams,
    GetWorkParams,
    ObservationsParams,
    RegisterProjectParams,
    StartSessionParams,
    SweepParams,
)
from vogt.application.services import (
    bind_branch,
    get_work,
    observations,
    register_project,
    start_session,
    sweep,
)
from vogt.collectors import CollectorContext
from vogt.collectors.git_local import GitLocalCollector
from vogt.core.branches import (
    DEFAULT_BRANCH_PATTERNS,
    default_branch_name,
    match_branch,
)
from vogt.core.entities import Project
from vogt.errors import InvalidRequest, NotFound

from tests.conftest import native_work_item

WHY = "branch-binding test"


# -- the naming convention (deliverable 1) ---------------------------------


@pytest.mark.parametrize(
    ("name", "work_ref", "forge"),
    [
        ("wi-7/add-thing", "WI-7", None),
        ("feature/WI-7-add-thing", "WI-7", None),
        ("wi7", "WI-7", None),
        ("WI-42", "WI-42", None),
        ("gh-264-fix", None, 264),
        ("feature/gh-264", None, 264),
    ],
)
def test_each_shape_binds_to_its_item(
    name: str, work_ref: str | None, forge: int | None
) -> None:
    match = match_branch(name, DEFAULT_BRANCH_PATTERNS)
    assert match is not None, f"{name!r} should bind"
    assert match.work_ref == work_ref
    assert match.forge_number == forge


@pytest.mark.parametrize(
    "name",
    ["main", "develop", "feature/login", "kiwi-7", "gh-pages", "wip", "release"],
)
def test_a_look_alike_binds_to_nothing(name: str) -> None:
    """A word boundary is what keeps `kiwi-7` and `gh-pages` out."""
    assert match_branch(name, DEFAULT_BRANCH_PATTERNS) is None


def test_a_native_ref_and_an_upstream_subject_each_get_a_default_name() -> None:
    assert default_branch_name("WI-7", template="wi-{number}") == "wi-7"
    assert default_branch_name("gh:acme/app#264", template="wi-{number}") == "gh-264"


def test_a_broken_pattern_is_skipped_not_raised() -> None:
    """One bad estate-supplied pattern must not cost every branch after it."""
    patterns = ("(unclosed", r"(?i)\bwi-(?P<n>\d+)\b")
    match = match_branch("wi-9", patterns)
    assert match is not None and match.work_ref == "WI-9"


# -- the observed path (deliverable 3) -------------------------------------


def _project(root: Path, slug: str = "proj") -> Project:
    from datetime import UTC, datetime

    now = datetime(2026, 8, 12, tzinfo=UTC)
    return Project(
        id=f"prj_{slug}",
        slug=slug,
        name=slug,
        root_path=str(root),
        exclusions=[".git/"],
        created_at=now,
        updated_at=now,
    )


def _git_repo_with_branches(root: Path) -> None:
    """A repository whose `wi-1` branch is one commit ahead of `main`."""

    def git(*args: str) -> None:
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)

    git("init", "-q", "-b", "main")
    git("config", "user.email", "t@example.com")
    git("config", "user.name", "Test")
    (root / "a.txt").write_text("hello\n", encoding="utf-8")
    git("add", "a.txt")
    git("commit", "-qm", "first")
    git("branch", "gh-264-fix")  # a forge-numbered branch, at main
    git("checkout", "-q", "-b", "wi-1")  # WI-1's branch, then one commit ahead
    (root / "b.txt").write_text("more\n", encoding="utf-8")
    git("add", "b.txt")
    git("commit", "-qm", "second")
    git("checkout", "-q", "main")


def test_the_collector_reports_each_branch_and_the_item_it_binds_to(
    instance: AppContext, tmp_path: Path
) -> None:
    _git_repo_with_branches(tmp_path)
    ctx = CollectorContext(config=instance.config, clock=instance.clock)

    found = [
        f
        for f in GitLocalCollector().collect(ctx, _project(tmp_path))
        if f.kind == "git.branch"
    ]
    by_name = {str(f.payload["name"]): f.payload for f in found}
    assert {"main", "wi-1", "gh-264-fix"} <= set(by_name)

    assert by_name["wi-1"]["work_item_ref"] == "WI-1"
    assert by_name["wi-1"]["forge_number"] is None
    assert by_name["wi-1"]["ahead"] == 1, "one commit ahead of the default branch"
    assert by_name["wi-1"]["behind"] == 0
    assert by_name["wi-1"]["last_commit_at"] is not None

    assert by_name["gh-264-fix"]["forge_number"] == 264
    assert by_name["gh-264-fix"]["work_item_ref"] is None

    # An unrelated branch binds to nothing, rather than to a guess.
    assert by_name["main"]["work_item_ref"] is None
    assert by_name["main"]["forge_number"] is None


def test_re_observing_an_unchanged_branch_writes_nothing(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-O7/NFR-S2: the digest is over stable fields, so age never churns it."""
    _git_repo_with_branches(tmp_path)
    ctx = CollectorContext(config=instance.config, clock=instance.clock)
    once = {
        str(f.payload["name"]): f.content_digest
        for f in GitLocalCollector().collect(ctx, _project(tmp_path))
        if f.kind == "git.branch"
    }
    twice = {
        str(f.payload["name"]): f.content_digest
        for f in GitLocalCollector().collect(ctx, _project(tmp_path))
        if f.kind == "git.branch"
    }
    assert once == twice


# -- the declared path (deliverable 2) -------------------------------------


def _wired(instance: AppContext, root: Path) -> AppContext:
    import dataclasses

    from vogt.adapters.engine import EngineClient

    from tests.test_sessions import StandInEngine

    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(
            base_url="http://127.0.0.1:8910", transport=StandInEngine()
        ),
    )
    register_project(
        ctx, RegisterProjectParams(name="Proj", root_path=str(root), reason=WHY)
    )
    return ctx


def test_starting_a_session_records_the_branch_on_the_overlay_audited(
    instance: AppContext, tmp_path: Path
) -> None:
    _git_repo_with_branches(tmp_path)
    ctx = _wired(instance, tmp_path)
    native_work_item(ctx, title="An item", project="proj")  # WI-1

    with ctx.declared.read() as view:
        assert view.work_overlay("WI-1") is None, "nothing declared before a session"
        audit_before = len(view.list_audit(limit=200))

    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))

    with ctx.declared.read() as view:
        overlay = view.work_overlay("WI-1")
        assert overlay is not None and overlay.branches == ["wi-1"]
        ops = [r.operation for r in view.list_audit(limit=200)]
    assert len(ops) > audit_before, "the session start is audited"
    assert "session.start" in ops


def test_a_second_session_does_not_duplicate_the_declared_branch(
    instance: AppContext, tmp_path: Path
) -> None:
    """Additive, not append-blind: re-declaring the same branch is a no-op."""
    _git_repo_with_branches(tmp_path)
    ctx = _wired(instance, tmp_path)
    native_work_item(ctx, title="An item", project="proj")  # WI-1

    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))

    with ctx.declared.read() as view:
        overlay = view.work_overlay("WI-1")
    assert overlay is not None and overlay.branches == ["wi-1"]


# -- declared vs observed, kept separate (deliverable 4, drift) ------------


def test_declared_and_observed_are_surfaced_separately_as_drift(
    instance: AppContext, tmp_path: Path
) -> None:
    """The whole point of FR-O2 here: agreement reads `both`, and either side
    alone reads as drift rather than being merged into the other."""
    _git_repo_with_branches(tmp_path)
    ctx = _wired(instance, tmp_path)
    native_work_item(ctx, title="Worked here", project="proj")  # WI-1
    native_work_item(ctx, title="Declared only", project="proj")  # WI-2

    # WI-1 declares `wi-1`, which the repo has (agreement) and also carries a
    # second WI-1 branch nobody declared. WI-2 declares `wi-2`, which the repo
    # does not have (declared-only).
    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    start_session(ctx, StartSessionParams(work_item="WI-2", reason=WHY))

    # A branch the sweep will match to WI-1 but nobody declared.
    subprocess.run(
        ["git", "-C", str(tmp_path), "branch", "feature/WI-1-extra"],
        check=True,
        capture_output=True,
    )
    sweep(ctx, SweepParams(offline_only=True, reason=WHY))

    # Provenance: the observation exists, from the git-local collector.
    observed = observations(ctx, ObservationsParams(kind="git.branch", limit=100))
    assert observed.observations, "the sweep recorded branch observations"
    assert all(o.collector == "git-local" for o in observed.observations)

    one = {b.name: b for b in get_work(ctx, GetWorkParams(ref="WI-1")).branches}
    assert one["wi-1"].source == "both" and one["wi-1"].drift is False
    assert one["wi-1"].last_commit_age_seconds is not None
    assert one["feature/WI-1-extra"].source == "observed"
    assert one["feature/WI-1-extra"].drift is True
    # The forge-numbered branch is not WI-1's and must not leak into its view.
    assert "gh-264-fix" not in one

    two = {b.name: b for b in get_work(ctx, GetWorkParams(ref="WI-2")).branches}
    assert two["wi-2"].source == "declared" and two["wi-2"].drift is True
    assert two["wi-2"].observed_at is None


# -- the declared binding as a first-class write (`work.bind_branch`) -------


def _with_project(instance: AppContext, root: Path) -> AppContext:
    """A context with a registered project, no engine — `bind_branch` needs none."""
    register_project(
        instance, RegisterProjectParams(name="Proj", root_path=str(root), reason=WHY)
    )
    return instance


def test_bind_branch_defaults_from_the_pattern_and_is_audited(
    instance: AppContext, tmp_path: Path
) -> None:
    """No branch given: the name defaults from the template, audited like any
    declared write, and the result echoes the item's branches."""
    ctx = _with_project(instance, tmp_path)
    native_work_item(ctx, title="An item", project="proj")  # WI-1

    result = bind_branch(ctx, BindBranchParams(ref="WI-1", reason=WHY))

    assert {b.name for b in result.branches} == {"wi-1"}
    assert result.branches[0].source == "declared"
    with ctx.declared.read() as view:
        overlay = view.work_overlay("WI-1")
        ops = [r.operation for r in view.list_audit(limit=200)]
    assert overlay is not None and overlay.branches == ["wi-1"]
    assert "work.bind_branch" in ops


def test_bind_branch_accepts_an_explicit_name(
    instance: AppContext, tmp_path: Path
) -> None:
    ctx = _with_project(instance, tmp_path)
    native_work_item(ctx, title="An item", project="proj")  # WI-1

    result = bind_branch(
        ctx, BindBranchParams(ref="WI-1", branch="feature/WI-1-thing", reason=WHY)
    )

    assert {b.name for b in result.branches} == {"feature/WI-1-thing"}
    with ctx.declared.read() as view:
        overlay = view.work_overlay("WI-1")
    assert overlay is not None and overlay.branches == ["feature/WI-1-thing"]


def test_bind_branch_is_idempotent_on_a_branch_already_bound(
    instance: AppContext, tmp_path: Path
) -> None:
    """Binding the same branch twice dedupes rather than erroring or growing."""
    ctx = _with_project(instance, tmp_path)
    native_work_item(ctx, title="An item", project="proj")  # WI-1

    bind_branch(ctx, BindBranchParams(ref="WI-1", branch="wi-1", reason=WHY))
    bind_branch(ctx, BindBranchParams(ref="WI-1", branch="wi-1", reason=WHY))
    result = bind_branch(ctx, BindBranchParams(ref="WI-1", reason=WHY))

    assert [b.name for b in result.branches] == ["wi-1"]
    with ctx.declared.read() as view:
        overlay = view.work_overlay("WI-1")
    assert overlay is not None and overlay.branches == ["wi-1"]


def test_bind_branch_on_an_unknown_ref_says_no_work_item(
    instance: AppContext, tmp_path: Path
) -> None:
    """A typo resolves to a domain not-found, not a foreign-key error."""
    ctx = _with_project(instance, tmp_path)

    with pytest.raises(NotFound, match="WI-70"):
        bind_branch(ctx, BindBranchParams(ref="WI-70", reason=WHY))


def test_bind_branch_on_a_projectless_item_is_refused(
    instance: AppContext, tmp_path: Path
) -> None:
    """An item that belongs to no project has no checkout to bind against."""
    del tmp_path
    native_work_item(instance, title="Homeless")  # WI-1, no project

    with pytest.raises(InvalidRequest, match="no project"):
        bind_branch(instance, BindBranchParams(ref="WI-1", reason=WHY))
