"""Collectors, sweeps, and the coverage record that makes absence meaningful."""

from __future__ import annotations

import subprocess
from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    CoverageParams,
    DepsParams,
    ObservationsParams,
    ProjectBriefParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    brief_project,
    coverage,
    deps,
    observations,
    register_project,
    sweep,
)
from vogt.collectors import CollectorContext, CollectorRegistry, Sweeper
from vogt.collectors.base import (
    Collector,
    CollectorError,
    Finding,
    finding,
    walk_project,
)
from vogt.collectors.dep_refs import KIND_DEP_REF, KIND_DEP_SCAN, DepRefCollector
from vogt.collectors.git_local import GitLocalCollector
from vogt.collectors.mirrored_source import (
    MirroredSourceCollector,
    RegisteredProject,
)
from vogt.collectors.source_markers import SourceMarkerCollector
from vogt.core.entities import Project

WHY = "collector test"


def _project(root: Path, slug: str = "fixture", *, exclusions: bool = False) -> Project:
    from datetime import UTC, datetime

    from vogt.application.models import DEFAULT_EXCLUSIONS

    now = datetime(2026, 8, 12, tzinfo=UTC)
    return Project(
        id=f"prj_{slug}",
        slug=slug,
        name=slug,
        root_path=str(root),
        exclusions=(
            list(DEFAULT_EXCLUSIONS)
            if exclusions
            else [".venv/", "node_modules/", "target/", ".git/"]
        ),
        created_at=now,
        updated_at=now,
    )


@pytest.fixture
def ctx(instance: AppContext) -> CollectorContext:
    return CollectorContext(config=instance.config, clock=instance.clock)


# -- walking ---------------------------------------------------------------


def test_exclusions_are_applied_before_a_file_is_opened(tmp_path: Path) -> None:
    """FR-G12: excluded paths never become observations at all."""
    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "keep.py").write_text("x = 1\n", encoding="utf-8")
    (tmp_path / "node_modules" / "pkg").mkdir(parents=True)
    (tmp_path / "node_modules" / "pkg" / "skip.py").write_text("y = 2\n", "utf-8")

    found = {
        path.name
        for path in walk_project(
            tmp_path, exclusions=("node_modules/",), extensions=(".py",)
        )
    }
    assert found == {"keep.py"}


def test_the_walk_stops_at_the_file_cap(tmp_path: Path) -> None:
    for index in range(20):
        (tmp_path / f"f{index}.py").write_text("x = 1\n", encoding="utf-8")
    walked = list(
        walk_project(tmp_path, exclusions=(), extensions=(".py",), max_files=5)
    )
    assert len(walked) == 5


# -- source markers --------------------------------------------------------


def test_only_configured_patterns_are_promoted(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """FR-W11: every marker is observed; only marked work claims to be work."""
    (tmp_path / "a.py").write_text(
        "# TODO(vogt): this one is work\n"
        "# TODO: this one is a note to self\n"
        "# FIXME(vogt): so is this\n"
        "# XXX: and this is not\n",
        encoding="utf-8",
    )
    found = list(SourceMarkerCollector().collect(ctx, _project(tmp_path)))

    assert len(found) == 4, "every marker is observed"
    promoted = {f.payload["text"] for f in found if f.promoted}
    assert promoted == {"this one is work", "so is this"}


def test_marker_subject_keys_are_stable(ctx: CollectorContext, tmp_path: Path) -> None:
    """A stable key is what makes dedup and suppression work at all."""
    (tmp_path / "a.py").write_text("\n# TODO(vogt): here\n", encoding="utf-8")
    once = list(SourceMarkerCollector().collect(ctx, _project(tmp_path)))
    twice = list(SourceMarkerCollector().collect(ctx, _project(tmp_path)))
    assert [f.subject_key for f in once] == ["mark:fixture/a.py#L2"]
    assert [f.content_digest for f in once] == [f.content_digest for f in twice]


def test_binary_and_long_lines_are_skipped(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "big.py").write_text("# TODO(vogt): " + "x" * 600 + "\n", "utf-8")
    (tmp_path / "bin.py").write_bytes(b"\xff\xfe# TODO(vogt): binary\n")
    assert list(SourceMarkerCollector().collect(ctx, _project(tmp_path))) == []


def test_the_scanned_extensions_are_configuration(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-W11: which extensions hold source is an estate's business."""
    (tmp_path / "a.weird").write_text("# TODO(vogt): findable\n", encoding="utf-8")
    default_ctx = CollectorContext(config=instance.config, clock=instance.clock)
    assert list(SourceMarkerCollector().collect(default_ctx, _project(tmp_path))) == []

    widened = instance.config.model_copy(update={"marker_file_extensions": (".weird",)})
    widened_ctx = CollectorContext(config=widened, clock=instance.clock)
    assert (
        len(list(SourceMarkerCollector().collect(widened_ctx, _project(tmp_path)))) == 1
    )


# -- dependency references -------------------------------------------------


def _dep_refs(ctx: CollectorContext, project: Project) -> list[Finding]:
    """The reference findings alone.

    `dep-refs` also emits one scan record per project — what it read, what it
    could not, and what it does not read at all — so that a zero at `deps`
    says which zero it is (#50). These tests are about the references, and
    filtering here keeps each one asserting on the thing it is named for.
    """
    return [
        f for f in DepRefCollector().collect(ctx, project) if f.kind == KIND_DEP_REF
    ]


def _scan(ctx: CollectorContext, project: Project) -> Finding:
    scans = [
        f for f in DepRefCollector().collect(ctx, project) if f.kind == KIND_DEP_SCAN
    ]
    assert len(scans) == 1, "exactly one scan record per project, always"
    return scans[0]


def test_only_internal_looking_references_are_extracted(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """Registry dependencies are ignored: Vogt has no opinion about serde."""
    (tmp_path / "Cargo.toml").write_text(
        "[dependencies]\n"
        'serde = "1.0"\n'
        'nzb-core = { path = "../nzb-core" }\n'
        'other = { git = "https://github.com/o/other" }\n',
        encoding="utf-8",
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    targets = {(f.payload["ref_kind"], f.payload["raw_target"]) for f in found}
    assert targets == {
        ("path", "../nzb-core"),
        ("git", "https://github.com/o/other"),
    }


def test_package_json_workspaces_and_file_refs(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "package.json").write_text(
        '{"dependencies": {"react": "^18", "sib": "file:../sib"},'
        ' "workspaces": ["packages/*"]}',
        encoding="utf-8",
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert {f.payload["raw_target"] for f in found} == {"file:../sib", "packages/*"}


def test_a_workspace_member_is_internal_not_an_unregistered_project(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """The rustnzb case: thirty proposals about a layout chosen on purpose.

    Every one of them named a crate inside the project's own tree, and each
    carried the gate text "usually it is a project nobody has registered yet".
    """
    (tmp_path / "crates" / "nzb-core").mkdir(parents=True)
    (tmp_path / "crates" / "nzb-web").mkdir(parents=True)
    (tmp_path / "crates" / "nzb-web" / "Cargo.toml").write_text(
        '[dependencies]\nnzb-core = { path = "../nzb-core" }\n', encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert [f.payload["scope"] for f in found] == ["internal"]


def test_a_nested_workspace_reaching_back_is_still_internal(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """Why containment is the test and workspace membership is not.

    rustnzb's root manifest carries `exclude = ["benchnzb", "desktop"]`, and
    `fuzz/` is not a member either — three separate Cargo workspaces nested in
    one repository, reaching back into the main one by relative path. Keying
    the fix on `members` is the obvious implementation and still flags five of
    the thirty.
    """
    (tmp_path / "crates" / "nzb-core").mkdir(parents=True)
    (tmp_path / "fuzz").mkdir()
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\nexclude = ["fuzz"]\n', encoding="utf-8"
    )
    (tmp_path / "fuzz" / "Cargo.toml").write_text(
        '[dependencies]\nnzb-core = { path = "../crates/nzb-core" }\n',
        encoding="utf-8",
    )
    scopes = {
        f.payload["raw_target"]: f.payload["scope"]
        for f in _dep_refs(ctx, _project(tmp_path))
    }
    assert scopes["../crates/nzb-core"] == "internal", "excluded, but still in the tree"


def test_a_reference_outside_the_tree_is_external(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """The case the proposal was always meant for: a sibling project."""
    (tmp_path / "repo").mkdir()
    (tmp_path / "sibling").mkdir()
    (tmp_path / "repo" / "Cargo.toml").write_text(
        '[dependencies]\nsib = { path = "../sibling" }\n', encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path / "repo")))
    assert [f.payload["scope"] for f in found] == ["external"]


def test_an_in_tree_path_that_resolves_to_nothing_is_broken(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """Three outcomes, not two.

    Containment alone would swallow this one: it points inside the project,
    where there is nothing to register, and the manifest is simply wrong.
    """
    (tmp_path / "crates").mkdir()
    (tmp_path / "Cargo.toml").write_text(
        '[dependencies]\ngone = { path = "crates/moved-away" }\n', encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert [f.payload["scope"] for f in found] == ["broken"]


def test_a_glob_member_is_internal_rather_than_broken(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """`members = ["crates/*"]` is a pattern; no such directory exists."""
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crates/*"]\n', encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert [f.payload["scope"] for f in found] == ["internal"]


def test_dependency_inheritance_is_not_a_path_at_all(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """`opentelemetry.workspace = true` inherits from the workspace table.

    rustnzb produced three of these and every one was reported as an
    unregistered project named `workspace:.`.
    """
    (tmp_path / "Cargo.toml").write_text(
        "[dependencies]\nopentelemetry = { workspace = true }\n", encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert [f.payload["ref_kind"] for f in found] == ["inherited"]
    assert [f.payload["scope"] for f in found] == ["internal"]


def test_a_git_reference_is_external_however_it_looks(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "Cargo.toml").write_text(
        '[dependencies]\nother = { git = "https://github.com/o/other" }\n',
        encoding="utf-8",
    )
    found = list(_dep_refs(ctx, _project(tmp_path)))
    assert [f.payload["scope"] for f in found] == ["external"]


def test_agent_worktrees_are_not_part_of_the_dependency_graph(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """`vogt` reported six references out and three were one throwaway copy.

    A worktree under `.claude/` carries a complete set of manifests, so the
    graph is inflated by however many happen to be lying around.
    """
    (tmp_path / "engine").mkdir()
    (tmp_path / "engine" / "Cargo.toml").write_text(
        '[dependencies]\ncontract = { path = "contract" }\n', encoding="utf-8"
    )
    worktree = tmp_path / ".claude" / "worktrees" / "docs-pass" / "engine"
    worktree.mkdir(parents=True)
    (worktree / "Cargo.toml").write_text(
        '[dependencies]\ncontract = { path = "contract" }\n', encoding="utf-8"
    )
    found = list(_dep_refs(ctx, _project(tmp_path, exclusions=True)))
    assert len(found) == 1, "one manifest is the project's; the other is scratch"
    assert found[0].payload["manifest"] == "engine/Cargo.toml"


def test_a_malformed_manifest_is_a_fact_not_a_failure(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "Cargo.toml").write_text("this is not toml [[[", encoding="utf-8")
    assert list(_dep_refs(ctx, _project(tmp_path))) == []


# -- the scan record: which zero a zero is (#50) ----------------------------


def test_the_scan_record_names_a_manifest_format_it_does_not_read(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """A Go project with no references has a graph; this one cannot see it."""
    (tmp_path / "go.mod").write_text("module example.com/thing\n", encoding="utf-8")
    scan = _scan(ctx, _project(tmp_path))
    assert scan.payload["references"] == 0
    assert scan.payload["manifests_read"] == 0
    assert scan.payload["unsupported_manifests"] == ["go.mod"], (
        "a zero from a format nothing parses must say so"
    )


def test_the_scan_record_names_a_manifest_that_would_not_parse(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "Cargo.toml").write_text("this is not toml [[[", encoding="utf-8")
    scan = _scan(ctx, _project(tmp_path))
    assert scan.payload["unreadable_manifests"] == ["Cargo.toml"]
    assert scan.payload["manifests_read"] == 0


def test_a_scan_record_is_written_even_for_a_project_with_nothing_in_it(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """The record is the coverage; skipping it when there is nothing to say
    is exactly how "nothing here" and "nobody looked" became one answer."""
    scan = _scan(ctx, _project(tmp_path))
    assert scan.payload == {
        "manifests_read": 0,
        "references": 0,
        "unreadable_manifests": [],
        "unsupported_manifests": [],
        "root_exists": True,
    }


def test_deps_distinguishes_an_unswept_project_from_an_independent_one(
    instance: AppContext, tmp_path: Path
) -> None:
    """WI-9's shape, on the surface #50 named next.

    `has_evidence_tables` is estate-wide: a project registered after the last
    sweep passed it and reported `0 references` as though somebody had looked.
    """
    swept = tmp_path / "swept"
    swept.mkdir()
    (swept / "Cargo.toml").write_text('[package]\nname = "swept"\n', encoding="utf-8")
    register_project(
        instance, RegisterProjectParams(name="Swept", root_path=str(swept), reason=WHY)
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    latecomer = tmp_path / "latecomer"
    latecomer.mkdir()
    (latecomer / "go.mod").write_text("module example.com/late\n", encoding="utf-8")
    register_project(
        instance,
        RegisterProjectParams(name="Latecomer", root_path=str(latecomer), reason=WHY),
    )

    unswept = deps(instance, DepsParams(project="latecomer"))
    assert unswept.status == "not_collected"
    assert unswept.detail is not None and "never walked" in unswept.detail

    looked_at = deps(instance, DepsParams(project="swept"))
    assert looked_at.status == "collected"
    assert looked_at.references_out == []
    assert looked_at.detail is None, "a genuine zero needs no excuse"

    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    now_swept = deps(instance, DepsParams(project="latecomer"))
    assert now_swept.status == "collected"
    assert now_swept.unsupported_manifests == ["go.mod"]
    assert now_swept.detail is not None
    assert "collector's reach" in now_swept.detail


def test_the_brief_does_not_claim_to_have_collected_an_unswept_project(
    instance: AppContext, tmp_path: Path
) -> None:
    other = tmp_path / "other"
    other.mkdir()
    register_project(
        instance, RegisterProjectParams(name="Other", root_path=str(other), reason=WHY)
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    late = tmp_path / "late"
    late.mkdir()
    register_project(
        instance, RegisterProjectParams(name="Late", root_path=str(late), reason=WHY)
    )
    summary = brief_project(instance, ProjectBriefParams(slug="late")).dependencies
    assert summary.status == "not_collected"
    assert summary.detail is not None and "not collected" in summary.detail


# -- mirrored source (FR-D8) -----------------------------------------------


class _Index:
    """The registered project list, as the application layer hands it down."""

    def __init__(self, projects: list[RegisteredProject]) -> None:
        self._projects = projects

    def registered(self) -> list[RegisteredProject]:
        return list(self._projects)


def _crate(root: Path, name: str, version: str) -> None:
    root.mkdir(parents=True, exist_ok=True)
    (root / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "{version}"\n', encoding="utf-8"
    )


def test_a_vendored_crate_that_is_also_a_project_is_reported(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """The rustnzb case, and the eighteen the estate onboarding produced.

    `rustnzb/crates/nzb-core` is a path member of one project and `nzb-core`
    is a separately registered project. Both facts were already true and
    nothing joined them.
    """
    vendoring = tmp_path / "rustnzb"
    _crate(vendoring / "crates" / "nzb-core", "nzb-core", "0.3.0")
    published = tmp_path / "nzb-core"
    _crate(published, "nzb-core", "0.4.0")

    index = _Index(
        [
            RegisteredProject(
                id="prj_published",
                slug="nzb-core",
                root_path=str(published),
                repo_url="https://github.com/o/nzb-core",
            )
        ]
    )
    found = list(
        MirroredSourceCollector(index).collect(ctx, _project(vendoring, "rustnzb"))
    )
    assert len(found) == 1
    payload = found[0].payload
    assert found[0].kind == "mirrored_source"
    assert found[0].subject_key == "mirror:rustnzb/crates/nzb-core->nzb-core"
    assert payload["package"] == "nzb-core"
    assert payload["local_path"] == "crates/nzb-core"
    assert payload["mirrors_project_slug"] == "nzb-core"
    assert (payload["local_version"], payload["published_version"]) == (
        "0.3.0",
        "0.4.0",
    ), "both declared versions are recorded; neither is compared to the other"


def test_a_project_is_not_a_mirror_of_itself(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """The root manifest declares the project's own package, by definition."""
    root = tmp_path / "solo"
    _crate(root, "solo", "1.0.0")
    index = _Index([RegisteredProject(id="prj_solo", slug="solo", root_path=str(root))])
    assert (
        list(MirroredSourceCollector(index).collect(ctx, _project(root, "solo"))) == []
    )


def test_a_package_name_two_projects_claim_is_not_a_finding(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """An ambiguous match would be an assertion about which copy is real."""
    vendoring = tmp_path / "app"
    _crate(vendoring / "vendor" / "shared", "shared", "0.1.0")
    first = tmp_path / "first"
    _crate(first, "shared", "0.1.0")
    second = tmp_path / "second"
    _crate(second, "shared", "0.2.0")

    index = _Index(
        [
            RegisteredProject(id="prj_first", slug="first", root_path=str(first)),
            RegisteredProject(id="prj_second", slug="second", root_path=str(second)),
        ]
    )
    assert (
        list(MirroredSourceCollector(index).collect(ctx, _project(vendoring, "app")))
        == []
    )


def test_a_project_checked_out_inside_another_is_one_copy_not_two(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """A submodule is the same source in one place, not the same in two."""
    outer = tmp_path / "outer"
    inner = outer / "vendor" / "inner"
    _crate(inner, "inner", "1.0.0")
    index = _Index(
        [RegisteredProject(id="prj_inner", slug="inner", root_path=str(inner))]
    )
    assert (
        list(MirroredSourceCollector(index).collect(ctx, _project(outer, "outer")))
        == []
    )


def test_mirrored_source_reaches_deps_from_both_ends(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-D8's whole delivery: `deps` lists it, in both directions."""
    vendoring = tmp_path / "rustnzb"
    _crate(vendoring / "crates" / "nzb-core", "nzb-core", "0.3.0")
    published = tmp_path / "nzb-core"
    _crate(published, "nzb-core", "0.4.0")
    for name, root in (("rustnzb", vendoring), ("nzb-core", published)):
        register_project(
            instance,
            RegisterProjectParams(name=name, root_path=str(root), reason=WHY),
        )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    carrier = deps(instance, DepsParams(project="rustnzb"))
    assert [(m.package, m.mirrors) for m in carrier.mirrors] == [
        ("nzb-core", "nzb-core")
    ]
    assert carrier.mirrored_by == []

    upstream = deps(instance, DepsParams(project="nzb-core"))
    assert [(m.package, m.project) for m in upstream.mirrored_by] == [
        ("nzb-core", "rustnzb")
    ]
    assert upstream.mirrors == []


def test_coverage_counts_every_project_a_collector_has_seen(
    instance: AppContext, tmp_path: Path
) -> None:
    """WI-11. A scoped sweep made every collector look like it had seen one.

    With eight projects registered and the last sweep scoped to one,
    `coverage` reported `projects: 1` for every collector — which reads as
    seven unswept projects and was in fact seven projects swept an hour
    earlier. This file's own STATUS document told readers to trust that count.
    """
    for name in ("Alpha", "Beta"):
        root = tmp_path / name.lower()
        root.mkdir()
        (root / "a.py").write_text("# TODO(vogt): x\n", encoding="utf-8")
        register_project(
            instance,
            RegisterProjectParams(name=name, root_path=str(root), reason=WHY),
        )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    sweep(instance, SweepParams(project="alpha", offline_only=True, reason=WHY))

    entry = next(
        c
        for c in coverage(instance, CoverageParams()).collectors
        if c.collector == "source-markers"
    )
    assert entry.projects == 2, "both, however narrow the last sweep was"
    assert entry.last_sweep_scope == 1, "and the last sweep's scope is still visible"
    assert entry.registered == 2
    assert entry.never_swept == 0


def test_coverage_names_a_project_nothing_has_looked_at(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-O4: a registered project with no evidence behind it is the case.

    It has no observations, so nothing it claims is corroborated, and it was
    previously indistinguishable from a project that had merely missed the
    last sweep.
    """
    swept_root = tmp_path / "swept"
    swept_root.mkdir()
    register_project(
        instance,
        RegisterProjectParams(name="Swept", root_path=str(swept_root), reason=WHY),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    unswept_root = tmp_path / "unswept"
    unswept_root.mkdir()
    registered = register_project(
        instance,
        RegisterProjectParams(name="Unswept", root_path=str(unswept_root), reason=WHY),
    )

    result = coverage(instance, CoverageParams())
    assert result.unswept_project_ids == [registered.project.id]
    assert all(c.never_swept == 1 for c in result.collectors if c.status != "never_run")


# -- git-local -------------------------------------------------------------


def test_a_plain_folder_is_reported_as_one(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    """A folder is a first-class kind of project, not a broken repository."""
    found = list(GitLocalCollector().collect(ctx, _project(tmp_path)))
    assert len(found) == 1
    assert found[0].payload["is_git_repository"] is False


def test_a_real_repository_reports_branch_and_tag(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    def git(*args: str) -> None:
        subprocess.run(
            ["git", "-C", str(tmp_path), *args],
            check=True,
            capture_output=True,
        )

    git("init", "-q", "-b", "main")
    git("config", "user.email", "t@example.com")
    git("config", "user.name", "Test")
    (tmp_path / "a.txt").write_text("hello\n", encoding="utf-8")
    git("add", "a.txt")
    git("commit", "-qm", "first")
    git("tag", "v1.2.3")

    found = list(GitLocalCollector().collect(ctx, _project(tmp_path)))
    checkout = next(f for f in found if f.kind == "git.checkout")
    assert checkout.payload["is_git_repository"] is True
    assert checkout.payload["branch"] == "main"
    assert checkout.payload["latest_tag"] == "v1.2.3"
    assert any(f.kind == "git.tag" for f in found)


def test_an_instance_without_git_reports_nothing_rather_than_a_clean_tree(
    ctx: CollectorContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#20: the observation this used to emit was a fabricated one.

    v0.2.0's image had no git. Every `subprocess.run` raised
    `FileNotFoundError`, `git_output` swallowed it into "", and the collector
    published `{"is_git_repository": true, "branch": "", "head": "",
    "dirty": false, "dirty_files": 0}` for every registered project — a
    positive claim that each working tree was clean, made without reading
    one, and indistinguishable downstream from a real observation.

    A collector that cannot read must say it could not read. `CollectorError`
    is how this codebase already says that: the sweeper records the sweep as
    partial and names the project.
    """
    (tmp_path / ".git").mkdir()

    def _no_binary(*_args: object, **_kwargs: object) -> None:
        raise FileNotFoundError(2, "No such file or directory: 'git'")

    monkeypatch.setattr("vogt.collectors.git_local.subprocess.run", _no_binary)

    with pytest.raises(CollectorError):
        list(GitLocalCollector().collect(ctx, _project(tmp_path)))


def test_a_status_that_cannot_be_read_is_never_reported_as_clean(
    ctx: CollectorContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The same rule one level down, for git running but failing.

    `dirty` is the only field in this payload that is a claim rather than a
    value, so it is the only question asked with `required=True`. "no tags"
    stays an absence and stays forgiving.
    """
    (tmp_path / ".git").mkdir()

    def _status_fails(
        args: list[str], **kwargs: object
    ) -> subprocess.CompletedProcess[str]:
        del kwargs
        # Only `status` fails, so the test proves the `required=True` call is
        # the one that raises rather than any git call raising.
        code = 128 if "status" in args else 0
        stderr = "fatal: bad object" if code else ""
        return subprocess.CompletedProcess(args, code, "", stderr)

    monkeypatch.setattr("vogt.collectors.git_local.subprocess.run", _status_fails)

    with pytest.raises(CollectorError, match="status"):
        list(GitLocalCollector().collect(ctx, _project(tmp_path)))


# -- the sweeper -----------------------------------------------------------


class _Boom:
    """A collector that always fails, to prove one bad project is contained."""

    @property
    def name(self) -> str:
        return "boom"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> list[Finding]:
        del ctx
        msg = f"cannot read {project.slug}"
        raise OSError(msg)


class _Fixed:
    """A collector returning one finding whose content never changes."""

    @property
    def name(self) -> str:
        return "fixed"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> list[Finding]:
        del ctx
        return [
            finding(
                kind="test.subject",
                subject_key=f"test:{project.slug}",
                project=project,
                payload={"stable": True},
            )
        ]


def test_a_failing_collector_makes_the_sweep_partial_and_names_the_project(
    instance: AppContext, ctx: CollectorContext, tmp_path: Path
) -> None:
    sweeper = Sweeper(instance.observed, ctx)
    report = sweeper.run_one(_Boom(), [_project(tmp_path, "one")])

    assert report.outcome == "failed"
    assert "one" in report.failures
    assert report.detail is not None and "cannot read one" in report.detail


def test_one_bad_project_does_not_cost_the_others(
    instance: AppContext, ctx: CollectorContext, tmp_path: Path
) -> None:
    class _Selective(_Fixed):
        def collect(self, ctx: CollectorContext, project: Project) -> list[Finding]:
            if project.slug == "bad":
                msg = "nope"
                raise OSError(msg)
            return super().collect(ctx, project)

    sweeper = Sweeper(instance.observed, ctx)
    report = sweeper.run_one(
        _Selective(), [_project(tmp_path, "good"), _project(tmp_path, "bad")]
    )
    assert report.outcome == "partial"
    assert report.new == 1


def test_unchanged_subjects_do_not_grow_the_store(
    instance: AppContext, ctx: CollectorContext, tmp_path: Path
) -> None:
    """FR-O7, NFR-S2: growth tracks change, not how often we look."""
    sweeper = Sweeper(instance.observed, ctx)
    first = sweeper.run_one(_Fixed(), [_project(tmp_path)])
    second = sweeper.run_one(_Fixed(), [_project(tmp_path)])

    assert (first.new, first.unchanged) == (1, 0)
    assert (second.new, second.unchanged) == (0, 1)
    assert instance.observed.counts()["observations"] == 1


# -- through the services --------------------------------------------------


def _register_fixture(instance: AppContext, root: Path) -> None:
    (root / "notes.md").write_text(
        "TODO(vogt): promoted\nTODO: unpromoted\n", encoding="utf-8"
    )
    (root / "pyproject.toml").write_text(
        '[project]\nname = "f"\n\n[tool.uv.sources]\nsib = { path = "../sib" }\n',
        encoding="utf-8",
    )
    register_project(
        instance,
        RegisterProjectParams(name="Fixture", root_path=str(root), reason=WHY),
    )


def test_a_sweep_records_coverage_and_publishes_an_event(
    instance: AppContext, tmp_path: Path
) -> None:
    _register_fixture(instance, tmp_path)
    result = sweep(instance, SweepParams(offline_only=True, reason=WHY))

    assert result.projects == 1
    assert {report.collector for report in result.reports} == {
        "git-local",
        "source-markers",
        "dep-refs",
        "mirrored-source",
    }, "contract-checker is on demand only and is not in an unnamed sweep"
    assert all(report.outcome == "ok" for report in result.reports)

    covered = coverage(instance, CoverageParams())
    by_collector = {entry.collector: entry.status for entry in covered.collectors}
    assert by_collector["source-markers"] == "ok"
    assert by_collector["contract-checker"] == "never_run", (
        "nothing re-checks the contract on a timer; an unnamed sweep skips it"
    )

    with instance.declared.read() as view:
        kinds = [event.kind for event in view.list_events(after=0, limit=50)]
    assert kinds.count("sweep.completed") == 4


def test_a_sweep_writes_no_audit_rows(instance: AppContext, tmp_path: Path) -> None:
    """FR-O2: collectors never write declared data, so nothing is audited."""
    _register_fixture(instance, tmp_path)
    with instance.declared.read() as view:
        before = len(view.list_audit(limit=100))

    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    with instance.declared.read() as view:
        after = [record.operation for record in view.list_audit(limit=100)]
    assert len(after) == before, f"a sweep audited something: {after}"


def test_a_crash_after_collectors_records_the_batch_as_failed(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """FR-O4, secondary finding in #44.

    Every collector below already commits its own sweep row independently
    (`Sweeper.run_one`), so a crash in the shared projection rebuild that
    follows leaves those rows genuinely `ok` in isolation — but nothing
    downstream (rebuilt projection, `sweep.completed` events) ever ran.
    Left alone, `coverage` would report the batch fresh and fine from a run
    nothing outside the observed store ever heard complete. This is the
    "record the sweep as failed" fix `sweep()` now applies before the
    exception propagates.
    """
    _register_fixture(instance, tmp_path)

    def _boom(rows: object) -> int:
        raise RuntimeError("simulated projection-rebuild crash")

    monkeypatch.setattr(instance.observed, "replace_dep_refs", _boom)

    with instance.declared.read() as view:
        events_before = len(view.list_events(after=0, limit=200))

    with pytest.raises(RuntimeError, match="simulated"):
        sweep(instance, SweepParams(offline_only=True, reason=WHY))

    covered = coverage(instance, CoverageParams())
    by_collector = {entry.collector: entry.status for entry in covered.collectors}
    assert by_collector["git-local"] == "failed"
    assert by_collector["source-markers"] == "failed"
    assert by_collector["dep-refs"] == "failed"

    with instance.declared.read() as view:
        events_after = len(view.list_events(after=0, limit=200))
    assert events_after == events_before, (
        "a crashed sweep must publish no completion event"
    )


def test_observations_are_queryable_including_unpromoted(
    instance: AppContext, tmp_path: Path
) -> None:
    _register_fixture(instance, tmp_path)
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    markers = observations(
        instance, ObservationsParams(kind="marker", latest_only=True, limit=100)
    )
    assert len(markers.observations) == 2, "unpromoted markers stay queryable"
    promoted = observations(
        instance,
        ObservationsParams(kind="marker", promoted_only=True, latest_only=True),
    )
    assert len(promoted.observations) == 1


def test_dependency_references_resolve_to_registered_projects(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-D3/D4: resolution to a registered project, and the reverse lookup."""
    sibling = tmp_path / "sib"
    sibling.mkdir()
    fixture = tmp_path / "fixture"
    fixture.mkdir()
    _register_fixture(instance, fixture)
    register_project(
        instance,
        RegisterProjectParams(name="Sib", root_path=str(sibling), reason=WHY),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    out = deps(instance, DepsParams(project="fixture"))
    assert [ref.to_project_slug for ref in out.references_out] == ["sib"]
    assert out.unresolved == 0

    reverse = deps(instance, DepsParams(project="sib"))
    assert [ref.from_project_slug for ref in reverse.referenced_by] == ["fixture"]


def test_an_inherited_dep_ref_survives_a_real_sweep(
    instance: AppContext, tmp_path: Path
) -> None:
    """Regression for #44.

    `test_dependency_inheritance_is_not_a_path_at_all` above only asserts on
    the collector's in-memory payload — which is exactly why the CHECK
    constraint rejecting `ref_kind='inherited'` shipped unnoticed. This drives
    the same manifest through `sweep()` and real sqlite storage, where #43's
    fix actually broke.
    """
    fixture = tmp_path / "fixture"
    fixture.mkdir()
    (fixture / "Cargo.toml").write_text(
        "[dependencies]\nopentelemetry = { workspace = true }\n", encoding="utf-8"
    )
    _register_fixture(instance, fixture)

    result = sweep(
        instance, SweepParams(collectors=["dep-refs"], offline_only=True, reason=WHY)
    )
    assert result.reports[0].outcome == "ok"

    out = deps(instance, DepsParams(project="fixture"))
    inherited = [ref for ref in out.references_out if ref.ref_kind == "inherited"]
    assert len(inherited) == 1
    assert inherited[0].raw_target == "workspace:."


def test_an_unresolved_reference_keeps_its_raw_target(
    instance: AppContext, tmp_path: Path
) -> None:
    fixture = tmp_path / "fixture"
    fixture.mkdir()
    _register_fixture(instance, fixture)
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    out = deps(instance, DepsParams(project="fixture"))
    assert out.unresolved == 1
    assert out.references_out[0].to_project_id is None
    assert out.references_out[0].raw_target == "../sib"


def test_collectors_can_be_selected_by_name(
    instance: AppContext, tmp_path: Path
) -> None:
    _register_fixture(instance, tmp_path)
    result = sweep(
        instance,
        SweepParams(collectors=["source-markers"], offline_only=True, reason=WHY),
    )
    assert [report.collector for report in result.reports] == ["source-markers"]


def test_the_registry_separates_offline_collectors() -> None:
    """NFR-PO2: the forge-less layer is a real selection, not a mock."""
    registry = CollectorRegistry()
    assert {c.name for c in registry.offline()} == set(registry.names)
    assert all(not c.requires_network for c in registry.offline())


def test_a_duplicate_collector_name_is_refused() -> None:
    registry = CollectorRegistry()
    with pytest.raises(ValueError, match="duplicate collector"):
        registry.add(GitLocalCollector())


def test_an_unknown_collector_is_named_in_the_error() -> None:
    from vogt.errors import NotFound

    with pytest.raises(NotFound, match="no collector named"):
        CollectorRegistry().get("nope")


def _names(collectors: list[Collector]) -> set[str]:
    return {collector.name for collector in collectors}


# -- markers are annotations, not mentions ---------------------------------


@pytest.mark.parametrize(
    "line",
    [
        "# TODO(vogt): a real one",
        "    // FIXME(vogt): indented, in a C-style comment",
        "-- TODO(vogt): SQL comment",
        "- TODO(vogt): a markdown bullet",
        "TODO(vogt): bare at the start of a line",
        "<!-- TODO(vogt): an HTML comment -->",
    ],
)
def test_leading_annotations_are_markers(
    ctx: CollectorContext, tmp_path: Path, line: str
) -> None:
    (tmp_path / "a.py").write_text(line + "\n", encoding="utf-8")
    found = list(SourceMarkerCollector().collect(ctx, _project(tmp_path)))
    assert len(found) == 1, f"{line!r} should be a marker"
    assert found[0].promoted is True


@pytest.mark.parametrize(
    "line",
    [
        # Every one of these is real: sweeping this repository unanchored
        # promoted 21 "markers", all of them documentation *about* markers.
        'marker_promotion_patterns = ["TODO(vogt)", "FIXME(vogt)"]',
        "| `marker_promotion_patterns` | default `TODO(vogt)` | behaviour |",
        "   pattern — default `TODO(vogt):` / `FIXME(vogt):` — enter backlog",
        "promotion by convention (`TODO(vogt):`), audited suppress",
        'assert promoted == {"TODO(vogt)"}',
    ],
)
def test_mentions_in_prose_are_not_markers(
    ctx: CollectorContext, tmp_path: Path, line: str
) -> None:
    """Vogt must not read its own description of markers and file it as work."""
    (tmp_path / "a.py").write_text(line + "\n", encoding="utf-8")
    assert list(SourceMarkerCollector().collect(ctx, _project(tmp_path))) == [], (
        f"{line!r} is a mention, not an annotation"
    )
