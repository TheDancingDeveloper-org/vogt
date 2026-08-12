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
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    coverage,
    deps,
    observations,
    register_project,
    sweep,
)
from vogt.collectors import CollectorContext, CollectorRegistry, Sweeper
from vogt.collectors.base import Collector, Finding, finding, walk_project
from vogt.collectors.dep_refs import DepRefCollector
from vogt.collectors.git_local import GitLocalCollector
from vogt.collectors.source_markers import SourceMarkerCollector
from vogt.core.entities import Project

WHY = "collector test"


def _project(root: Path, slug: str = "fixture") -> Project:
    from datetime import UTC, datetime

    now = datetime(2026, 8, 12, tzinfo=UTC)
    return Project(
        id=f"prj_{slug}",
        slug=slug,
        name=slug,
        root_path=str(root),
        exclusions=[".venv/", "node_modules/", "target/", ".git/"],
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
    found = list(DepRefCollector().collect(ctx, _project(tmp_path)))
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
    found = list(DepRefCollector().collect(ctx, _project(tmp_path)))
    assert {f.payload["raw_target"] for f in found} == {"file:../sib", "packages/*"}


def test_a_malformed_manifest_is_a_fact_not_a_failure(
    ctx: CollectorContext, tmp_path: Path
) -> None:
    (tmp_path / "Cargo.toml").write_text("this is not toml [[[", encoding="utf-8")
    assert list(DepRefCollector().collect(ctx, _project(tmp_path))) == []


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
    assert kinds.count("sweep.completed") == 3


def test_a_sweep_writes_no_audit_rows(instance: AppContext, tmp_path: Path) -> None:
    """FR-O2: collectors never write declared data, so nothing is audited."""
    _register_fixture(instance, tmp_path)
    with instance.declared.read() as view:
        before = len(view.list_audit(limit=100))

    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    with instance.declared.read() as view:
        after = [record.operation for record in view.list_audit(limit=100)]
    assert len(after) == before, f"a sweep audited something: {after}"


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
