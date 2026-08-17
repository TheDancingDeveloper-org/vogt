"""Drift proposals: raised with evidence, resolved by a person (FR-R1–R5).

Includes the M3 demo. From `ROADMAP.md`:

    `contract check` a project missing `AGENTS.md` → the result names exactly
    that criterion, and the project's brief shows `non_compliant` alongside
    "checked 4 seconds ago"; check nothing for a week and the same brief says
    so rather than quietly refreshing. Tag a release without updating the
    declared version, sweep → `version_mismatch` proposal with evidence;
    accept it, and the audit trail shows who accepted it and why. Delete the
    observation's history window and confirm the proposal still renders its
    evidence.
"""

from __future__ import annotations

import re
import subprocess
from datetime import UTC, datetime
from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    ContractCheckParams,
    DriftDetectParams,
    DriftListParams,
    DriftResolveParams,
    GetProjectParams,
    ProjectBriefParams,
    PruneParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    brief_project,
    contract_check,
    detect_drift,
    get_project,
    list_drift,
    prune,
    register_project,
    resolve_drift,
    sweep,
)
from vogt.application.services.drift_service import (
    DRIFT_RAISED_EVENT,
    DRIFT_RESOLVED_EVENT,
    DRIFT_SUPERSEDED_EVENT,
)
from vogt.collectors.base import finding
from vogt.core.drift import (
    BROKEN_PATH_DEPENDENCY,
    CI_RED_VS_HEALTHY,
    UNRESOLVED_DEPENDENCY,
    VERSION_MISMATCH,
    EvidenceSnapshot,
    forge_state_mismatch,
    issue_references,
    normalise_version,
)
from vogt.core.entities import Project
from vogt.errors import Conflict, InvalidRequest, NotFound

WHY = "drift test"


def _tagged_repo(root: Path, tag: str) -> Path:
    root.mkdir(parents=True, exist_ok=True)

    def git(*args: str) -> None:
        subprocess.run(["git", "-C", str(root), *args], check=True, capture_output=True)

    git("init", "-q", "-b", "main")
    git("config", "user.email", "t@example.com")
    git("config", "user.name", "Test")
    (root / "a.txt").write_text("hello\n", encoding="utf-8")
    git("add", "a.txt")
    git("commit", "-qm", "first")
    git("tag", tag)
    return root


def _commit_all(root: Path, message: str) -> None:
    """Commit the working tree, so the contract has a repository to read."""
    subprocess.run(
        ["git", "-C", str(root), "add", "-A"], check=True, capture_output=True
    )
    subprocess.run(
        ["git", "-C", str(root), "commit", "-qm", message],
        check=True,
        capture_output=True,
    )


@pytest.fixture
def released(instance: AppContext, tmp_path: Path) -> AppContext:
    """A registered project tagged v1.5 that still declares nothing."""
    _tagged_repo(tmp_path / "repo", "v1.5.0")
    register_project(
        instance,
        RegisterProjectParams(
            name="Released", root_path=str(tmp_path / "repo"), reason=WHY
        ),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    return instance


# -- the comparison --------------------------------------------------------


@pytest.mark.parametrize(
    ("declared", "observed"),
    [("1.4.0", "v1.4.0"), ("v1.4.0", "1.4.0"), ("V1.4.0", "1.4.0")],
)
def test_a_leading_v_is_not_drift(declared: str, observed: str) -> None:
    """Noise that teaches people to ignore drift proposals."""
    assert normalise_version(declared) == normalise_version(observed)


def test_a_tag_ahead_of_the_declared_version_is_drift(released: AppContext) -> None:
    result = detect_drift(released, DriftDetectParams(auto_accept=False, reason=WHY))
    assert [p.kind for p in result.raised] == [VERSION_MISMATCH]
    proposal = result.raised[0]
    assert "v1.5.0" in proposal.summary
    assert proposal.proposed_change["to"] == "v1.5.0"
    assert proposal.status == "open"


def _record_checks(
    ctx: AppContext, project: Project, runs: list[tuple[str, str, str, str]]
) -> None:
    """Seed CI check observations the way `gh-actions` would."""
    findings = [
        finding(
            kind="ci.check",
            subject_key=f"ci:owner/repo@{revision}:{name}",
            project=project,
            payload={
                "revision": revision,
                "check": name,
                "conclusion": conclusion,
                "updated_at": ran_at,
            },
        )
        for revision, name, conclusion, ran_at in runs
    ]
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector="gh-actions", scope=[project.id], at=now)
    ctx.observed.append(row.id, findings, at=now)
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={"projects": 1}, at=now)
    ctx.observed.rebuild_latest()


def test_a_stale_red_is_history_not_drift(released: AppContext) -> None:
    """A fixed build must stop being a proposal (FR-O6).

    `ci_red_vs_healthy` read every retained check as one population, so a
    failure from days earlier kept raising a proposal against a project whose
    head was green — and could keep raising it until retention removed the
    row. Being green was not enough to be green.
    """
    with released.declared.read() as view:
        project = view.list_projects(limit=10, offset=0)[0]
    _record_checks(
        released,
        project,
        [
            ("0291aff", "build image", "failure", "2026-08-09T22:41:46Z"),
            ("9fe53d8", "build image", "success", "2026-08-13T09:36:27Z"),
        ],
    )

    # Guards against this test going vacuous: the checks must actually be
    # attached to the project, or "no proposal" proves nothing.
    brief = brief_project(released, ProjectBriefParams(slug=project.slug))
    assert brief.ci_status.status == "passing"
    assert brief.ci_status.revision == "9fe53d8"
    assert brief.ci_status.earlier_failures == 1

    result = detect_drift(released, DriftDetectParams(auto_accept=False, reason=WHY))
    assert CI_RED_VS_HEALTHY not in [p.kind for p in result.raised], (
        "the head is green; the failure is four days and one commit behind it"
    )


def test_a_red_head_is_still_drift(released: AppContext) -> None:
    with released.declared.read() as view:
        project = view.list_projects(limit=10, offset=0)[0]
    _record_checks(
        released,
        project,
        [
            ("0291aff", "build image", "success", "2026-08-09T22:41:46Z"),
            ("9fe53d8", "build image", "failure", "2026-08-13T09:36:27Z"),
        ],
    )

    raised = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised
    ci = [p for p in raised if p.kind == CI_RED_VS_HEALTHY]
    assert len(ci) == 1
    assert "9fe53d8" in ci[0].summary, "and it names the commit that is red"
    assert ci[0].summary.count("build image") == 1, "one workflow, named once"


def test_an_internal_reference_is_never_a_drift_proposal(
    instance: AppContext, tmp_path: Path
) -> None:
    """The thirty rustnzb proposals, end to end.

    Every one named a crate inside the project's own tree, and the board they
    landed on was 44 proposals with 36 of them this.
    """
    root = tmp_path / "repo"
    (root / "crates" / "core").mkdir(parents=True)
    (root / "crates" / "web").mkdir(parents=True)
    (root / "crates" / "web" / "Cargo.toml").write_text(
        '[dependencies]\ncore = { path = "../core" }\n', encoding="utf-8"
    )
    _tagged_repo(root, "v1.0.0")
    register_project(
        instance, RegisterProjectParams(name="Mono", root_path=str(root), reason=WHY)
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    kinds = [
        p.kind
        for p in detect_drift(
            instance, DriftDetectParams(auto_accept=False, reason=WHY)
        ).raised
    ]
    assert UNRESOLVED_DEPENDENCY not in kinds
    assert BROKEN_PATH_DEPENDENCY not in kinds


def test_an_in_tree_path_that_resolves_to_nothing_is_its_own_kind(
    instance: AppContext, tmp_path: Path
) -> None:
    """Three outcomes, not two.

    "Register the project this points at" is the wrong instruction when the
    target is inside the project: there is nothing to register, the manifest
    is wrong.
    """
    root = tmp_path / "repo"
    (root / "crates").mkdir(parents=True)
    _tagged_repo(root, "v1.0.0")
    (root / "Cargo.toml").write_text(
        '[dependencies]\ngone = { path = "crates/moved-away" }\n', encoding="utf-8"
    )
    register_project(
        instance, RegisterProjectParams(name="Mono", root_path=str(root), reason=WHY)
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    broken = [
        p
        for p in detect_drift(
            instance, DriftDetectParams(auto_accept=False, reason=WHY)
        ).raised
        if p.kind == BROKEN_PATH_DEPENDENCY
    ]
    assert len(broken) == 1
    assert "does not exist" in broken[0].summary
    assert "not a registered project" not in broken[0].summary


def test_raising_a_proposal_is_an_audited_declared_write(
    released: AppContext,
) -> None:
    """NFR-I1: a drift proposal is a declared entity, so raising one is audited.

    It was inserted in a bare `ctx.declared.write()` — an entity row
    landing in the declared store with no audit row and no actor, by the
    operation that creates more rows per run than any other. The rule
    `audited_write` exists to enforce, broken where it was least visible.
    """
    why = "First drift pass after onboarding."
    result = detect_drift(released, DriftDetectParams(auto_accept=False, reason=why))
    assert result.raised, "the fixture is meant to produce one"

    with released.declared.read() as view:
        rows = [
            record
            for record in view.list_audit(limit=100)
            if record.operation == "drift.detect"
        ]
    assert len(rows) == len(result.raised), "one row per proposal raised"
    assert {record.reason for record in rows} == {why}
    assert {record.entity_kind for record in rows} == {"drift_proposal"}
    assert {record.entity_id for record in rows} == {p.id for p in result.raised}


def test_a_proposal_carries_its_evidence(released: AppContext) -> None:
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    snapshot = proposal.evidence_snapshot
    assert snapshot["subject_key"]
    assert snapshot["content_digest"]
    assert snapshot["collector"] == "git-local"
    assert proposal.evidence_observation_id is not None


def test_detecting_twice_does_not_stack_duplicates(released: AppContext) -> None:
    """A proposal is a question; asking it twice is noise."""
    first = detect_drift(released, DriftDetectParams(auto_accept=False, reason=WHY))
    second = detect_drift(released, DriftDetectParams(auto_accept=False, reason=WHY))
    assert len(first.raised) == 1
    assert second.raised == []
    assert second.already_open == 1


def test_an_unresolved_reference_is_drift(instance: AppContext, tmp_path: Path) -> None:
    project = tmp_path / "app"
    project.mkdir()
    (project / "pyproject.toml").write_text(
        '[project]\nname = "app"\n\n[tool.uv.sources]\nsib = { path = "../sib" }\n',
        encoding="utf-8",
    )
    register_project(
        instance,
        RegisterProjectParams(name="App", root_path=str(project), reason=WHY),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))

    result = detect_drift(instance, DriftDetectParams(auto_accept=True, reason=WHY))
    assert [p.kind for p in result.raised] == [UNRESOLVED_DEPENDENCY]
    assert result.auto_accepted == [], "structural kinds are always human-gated"

    listed = list_drift(instance, DriftListParams())
    assert UNRESOLVED_DEPENDENCY in listed.human_gated
    assert "nobody has registered" in listed.human_gated[UNRESOLVED_DEPENDENCY]


def test_detecting_before_a_sweep_says_so(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="nothing to"):
        detect_drift(instance, DriftDetectParams(reason=WHY))


# -- proposals raised under evidence that moved on (#48, FR-R6) ------------


def _broken_reference(instance: AppContext, root: Path) -> Path:
    """A project whose manifest points at a directory inside it that is gone.

    The same shape as the thirty-six: a proposal raised from one reading of a
    manifest, which a later sweep of the *same subject* reads differently.
    """
    (root / "crates").mkdir(parents=True)
    (root / "Cargo.toml").write_text(
        '[dependencies]\ngone = { path = "crates/moved-away" }\n', encoding="utf-8"
    )
    register_project(
        instance, RegisterProjectParams(name="Mono", root_path=str(root), reason=WHY)
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    return root / "crates" / "moved-away"


def test_a_proposal_the_evidence_stopped_reproducing_is_marked_superseded(
    instance: AppContext, tmp_path: Path
) -> None:
    """The thirty-six, and what it took to clear them.

    `WI-2`'s fix stopped `dep-refs` reporting Cargo dependency inheritance as
    an unresolved reference. The proposals it had already raised stayed open
    through the fix, its deploy, a regression and a re-fix, and closed only
    because somebody reconstructed the timeline from timestamps and ran
    `drift resolve --reject` thirty-six times.
    """
    missing = _broken_reference(instance, tmp_path / "repo")
    raised = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    proposal = next(p for p in raised.raised if p.kind == BROKEN_PATH_DEPENDENCY)

    # The directory is restored, and a sweep produces the evidence that says
    # so — the same subject, read differently.
    missing.mkdir()
    sweep(instance, SweepParams(offline_only=True, reason="after the fix"))
    again = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))

    assert again.superseded == [proposal.id]
    marked = next(
        p
        for p in list_drift(instance, DriftListParams()).proposals
        if p.id == proposal.id
    )
    assert marked.status == "open", "marking is not resolving (FR-R2, FR-U18)"
    assert marked.superseded_at is not None
    assert marked.superseded_detail is not None
    assert "no longer reproduces" in marked.superseded_detail
    assert marked.evidence_snapshot, "it still carries what it was raised on"


def test_a_condition_that_comes_back_clears_the_flag(
    instance: AppContext, tmp_path: Path
) -> None:
    """A stale "superseded" is worse than none: it says ignore a live one."""
    missing = _broken_reference(instance, tmp_path / "repo")
    proposal = next(
        p
        for p in detect_drift(
            instance, DriftDetectParams(auto_accept=False, reason=WHY)
        ).raised
        if p.kind == BROKEN_PATH_DEPENDENCY
    )

    missing.mkdir()
    sweep(instance, SweepParams(offline_only=True, reason="after the fix"))
    detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))

    missing.rmdir()
    sweep(instance, SweepParams(offline_only=True, reason="it came back"))
    detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))

    live = next(
        p
        for p in list_drift(instance, DriftListParams()).proposals
        if p.id == proposal.id
    )
    assert live.superseded_at is None
    assert live.superseded_detail is None


def test_nothing_is_superseded_without_a_sweep_that_could_say_so(
    instance: AppContext, tmp_path: Path
) -> None:
    """Coverage-gated, like every other absence claim here (FR-O4).

    Emptying the projection removes the *finding*, not the evidence. Marking
    on that would assert something no collector said.
    """
    _broken_reference(instance, tmp_path / "repo")
    detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))

    instance.observed.replace_dep_refs([])
    again = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))

    assert again.superseded == [], "no sweep since the proposal was raised"


def test_marking_a_proposal_superseded_is_an_audited_write(
    instance: AppContext, tmp_path: Path
) -> None:
    """It changes what the inbox says about a row somebody will act on."""
    missing = _broken_reference(instance, tmp_path / "repo")
    detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    missing.mkdir()
    sweep(instance, SweepParams(offline_only=True, reason="after the fix"))

    detect_drift(instance, DriftDetectParams(auto_accept=False, reason="reconcile"))

    with instance.declared.read() as view:
        events = [e.kind for e in view.list_events(after=0, limit=200)]
        reasons = [r.reason for r in view.list_audit(limit=200)]
    assert DRIFT_SUPERSEDED_EVENT in events
    assert "reconcile" in reasons


def test_detect_names_the_projects_it_could_not_have_raised_anything_for(
    instance: AppContext, tmp_path: Path
) -> None:
    """#50: `detect` refuses on a wholly unswept instance, and said nothing
    about the unswept projects on a partly swept one."""
    swept = tmp_path / "swept"
    swept.mkdir()
    register_project(
        instance, RegisterProjectParams(name="Swept", root_path=str(swept), reason=WHY)
    )
    sweep(instance, SweepParams(project="swept", offline_only=True, reason=WHY))

    unswept = tmp_path / "unswept"
    unswept.mkdir()
    register_project(
        instance,
        RegisterProjectParams(name="Unswept", root_path=str(unswept), reason=WHY),
    )

    result = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    assert result.not_collected == ["unswept"]


# -- the lifecycle ---------------------------------------------------------


def test_accepting_applies_the_change_and_is_audited(released: AppContext) -> None:
    """FR-R2: accepting is an ordinary audited write, not a silent mutation."""
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]

    resolved = resolve_drift(
        released,
        DriftResolveParams(
            id=proposal.id, resolution="accepted", reason="the tag is right"
        ),
    )
    assert resolved.change_applied is True
    assert resolved.proposal.status == "accepted"
    assert resolved.proposal.resolved_by_identity_ref == "local:test-user"

    assert (
        get_project(released, GetProjectParams(slug="released")).project.current_version
        == "v1.5.0"
    )

    with released.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "drift.resolve"
    assert record.reason == "the tag is right"


def test_rejecting_changes_nothing(released: AppContext) -> None:
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    resolved = resolve_drift(
        released,
        DriftResolveParams(
            id=proposal.id, resolution="rejected", reason="the tag was a mistake"
        ),
    )
    assert resolved.change_applied is False
    assert (
        get_project(released, GetProjectParams(slug="released")).project.current_version
        is None
    )


def test_contesting_is_a_chosen_resolution(released: AppContext) -> None:
    """`contested` is chosen; `disputed` is computed. One word, one meaning."""
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    resolved = resolve_drift(
        released,
        DriftResolveParams(
            id=proposal.id, resolution="contested", reason="asked the maintainer"
        ),
    )
    assert resolved.proposal.status == "contested"
    assert list_drift(released, DriftListParams(status="open")).proposals == []
    assert list_drift(released, DriftListParams(status="contested")).proposals


def test_resolving_twice_is_a_conflict(released: AppContext) -> None:
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    resolve_drift(
        released,
        DriftResolveParams(id=proposal.id, resolution="rejected", reason="no"),
    )
    with pytest.raises(Conflict, match="already rejected"):
        resolve_drift(
            released,
            DriftResolveParams(id=proposal.id, resolution="accepted", reason="yes"),
        )


def test_resolving_something_absent_says_so(released: AppContext) -> None:
    with pytest.raises(NotFound, match="no drift proposal"):
        resolve_drift(
            released,
            DriftResolveParams(id="dft_nope", resolution="accepted", reason="x"),
        )


def test_the_low_risk_policy_accepts_state_sync_only(released: AppContext) -> None:
    """FR-R3: the shipped default, and what it deliberately will not touch."""
    result = detect_drift(released, DriftDetectParams(auto_accept=True, reason=WHY))
    assert VERSION_MISMATCH in result.auto_acceptable_kinds
    assert UNRESOLVED_DEPENDENCY not in result.auto_acceptable_kinds, (
        "structural kinds are never auto-accepted"
    )
    assert len(result.auto_accepted) == 1

    with released.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert "auto-accepted under the shipped low-risk policy" in record.reason
    assert (
        get_project(released, GetProjectParams(slug="released")).project.current_version
        == "v1.5.0"
    )


def test_raising_and_resolving_both_publish_events(released: AppContext) -> None:
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    resolve_drift(
        released,
        DriftResolveParams(id=proposal.id, resolution="accepted", reason="fine"),
    )
    with released.declared.read() as view:
        kinds = [event.kind for event in view.list_events(after=0, limit=100)]
    assert "drift.raised" in kinds
    assert "drift.resolved" in kinds


# -- FR-R5: a proposal must never outlive its evidence --------------------


def test_retention_refuses_to_prune_evidence_a_proposal_references(
    released: AppContext,
) -> None:
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]
    pinned = proposal.evidence_observation_id
    assert pinned is not None

    with released.declared.read() as view:
        assert pinned in view.drift_evidence_ids()

    prune(released, PruneParams(reason="routine"))

    still_there = released.observed.list_observations(
        subject_key=str(proposal.evidence_snapshot["subject_key"]), limit=10
    )
    assert still_there, "the evidence a proposal points at survives retention"


def test_a_proposal_still_explains_itself_without_the_observed_store(
    released: AppContext,
) -> None:
    """The snapshot is self-contained, which is the point of taking one.

    Simulated by deleting the observation store's file outright — a harsher
    test than retention, and the one that proves the proposal does not
    depend on it.
    """
    proposal = detect_drift(
        released, DriftDetectParams(auto_accept=False, reason=WHY)
    ).raised[0]

    Path(released.config.observed_db_path).unlink()

    listed = list_drift(released, DriftListParams(status="open"))
    recovered = listed.proposals[0]
    assert recovered.summary == proposal.summary
    assert recovered.evidence_snapshot["subject_key"]
    assert recovered.evidence_snapshot["content_digest"]
    assert recovered.evidence_snapshot["observed_at"]
    assert recovered.proposed_change["to"] == "v1.5.0"


# -- the M3 demo -----------------------------------------------------------


def test_m3_demo(instance: AppContext, tmp_path: Path) -> None:
    # A project missing AGENTS.md names exactly that criterion. Everything
    # else is *committed*, not merely written: the contract asks what the
    # repository carries, so a required directory needs a tracked file in it
    # — which is why this repository keeps `design/.gitkeep`.
    project = tmp_path / "repo"
    _tagged_repo(project, "v1.5.0")
    for name in ("README.md", "LICENSE"):
        (project / name).write_text("x\n", encoding="utf-8")
    for name in ("docs", "design", "src"):
        (project / name).mkdir()
        (project / name / ".gitkeep").write_text("", encoding="utf-8")
    _commit_all(project, "scaffold")
    register_project(
        instance,
        RegisterProjectParams(name="Demo", root_path=str(project), reason=WHY),
    )

    checked = contract_check(instance, ContractCheckParams(project="demo", reason=WHY))
    assert [c.target for c in checked.failing] == ["AGENTS.md"]

    brief = brief_project(instance, ProjectBriefParams(slug="demo"))
    assert brief.compliance_status == "non_compliant"
    assert brief.compliance_checked_at is not None

    # Check nothing for a while and the brief says so rather than refreshing.
    (project / "AGENTS.md").write_text("now compliant\n", encoding="utf-8")
    _commit_all(project, "add AGENTS.md")
    unchanged = brief_project(instance, ProjectBriefParams(slug="demo"))
    assert unchanged.compliance_status == "non_compliant"
    assert unchanged.compliance_checked_at == brief.compliance_checked_at

    # A release tagged without updating the declared version, swept.
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    assert (
        brief_project(instance, ProjectBriefParams(slug="demo")).observed_version
        == "v1.5.0"
    )

    raised = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    proposal = next(p for p in raised.raised if p.kind == VERSION_MISMATCH)
    assert proposal.evidence_snapshot["content_digest"]

    # Accept it; the audit trail shows who and why.
    resolve_drift(
        instance,
        DriftResolveParams(
            id=proposal.id,
            resolution="accepted",
            reason="v1.5.0 shipped; the declared version was never updated",
        ),
    )
    after = brief_project(instance, ProjectBriefParams(slug="demo"))
    assert after.declared_version == "v1.5.0"
    assert after.version_matches is True

    with instance.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "drift.resolve"
    assert record.actor_identity_ref == "local:test-user"
    assert "v1.5.0 shipped" in record.reason

    # Prune the history window; the proposal still renders its evidence.
    prune(instance, PruneParams(reason="routine retention"))
    resolved = list_drift(instance, DriftListParams(status="accepted")).proposals[0]
    assert resolved.evidence_snapshot["subject_key"]
    assert resolved.resolution_reason is not None


# -- FR-M2: the engine's drift push filter names the kind this module emits --

ENGINE_PUSH_API = (
    Path(__file__).resolve().parents[1] / "engine" / "server" / "src" / "push_api.rs"
)


def engine_drift_notify_kinds() -> set[str]:
    source = ENGINE_PUSH_API.read_text(encoding="utf-8")
    match = re.search(
        r"const DRIFT_NOTIFY_KINDS: \[&str; \d+\] = \[(.*?)\];", source, re.S
    )
    assert match, "the engine's drift notification filter was not found"
    kinds = set(re.findall(r'"([^"]+)"', match.group(1)))
    assert kinds, "the filter is empty, so this check would prove nothing"
    return kinds


@pytest.mark.skipif(
    not ENGINE_PUSH_API.is_file(),
    reason="the merged tree carries the engine; a core-only checkout does not",
)
def test_the_engine_notifies_on_the_event_kind_this_module_publishes() -> None:
    """One string, spelled in two languages, with nothing else linking them.

    `push_api::spawn_vogt_drift_watcher` decides what is worth waking someone
    for by matching the core's event `kind` against a named set. That string is
    a literal in Rust and a constant here, and a rename on this side leaves a
    filter matching nothing — a phone that silently stops ringing, which is the
    failure nobody reports because it looks exactly like "no drift".

    Not hypothetical: the engine's own contract crate documented this kind as
    `drift.opened`, which has never been emitted, and a filter written from
    that comment would have shipped broken and looked right.
    """
    kinds = engine_drift_notify_kinds()
    assert DRIFT_RAISED_EVENT in kinds, (
        f"the engine pushes for {sorted(kinds)}, but this module publishes "
        f"{DRIFT_RAISED_EVENT!r} — so newly raised drift reaches nobody's phone"
    )


@pytest.mark.skipif(
    not ENGINE_PUSH_API.is_file(),
    reason="the merged tree carries the engine; a core-only checkout does not",
)
def test_resolving_drift_is_not_worth_a_phone_interruption() -> None:
    """FR-M2 says "new drift", and the filter is a named set for this reason.

    Somebody resolving drift is somebody already looking at it. This is the
    conjunct most likely to be lost by a well-meaning widening to
    `kind.startswith("drift.")`.
    """
    assert DRIFT_RESOLVED_EVENT not in engine_drift_notify_kinds()


# -- reading an issue reference out of an item's own text (FR-R7) ----------


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        (
            "GitHub: https://github.com/TheDancingDeveloper-org/vogt/issues/44",
            ["gh:TheDancingDeveloper-org/vogt#44"],
        ),
        (
            "see TheDancingDeveloper-org/vogt#44 for context",
            ["gh:TheDancingDeveloper-org/vogt#44"],
        ),
        ("http://www.github.com/o/r/issues/7", ["gh:o/r#7"]),
        # Named twice, one reference.
        (
            "https://github.com/o/r/issues/7 and again o/r#7",
            ["gh:o/r#7"],
        ),
    ],
)
def test_a_qualified_reference_names_an_issue(text: str, expected: list[str]) -> None:
    assert issue_references(text) == expected


@pytest.mark.parametrize(
    "text",
    [
        # WI-16's own title. #43 is a pull request; the issue it mirrors is
        # named separately, as a URL.
        "Regression from #43 (WI-2): dep-refs emits a ref_kind storage rejects",
        "fix in PR https://github.com/TheDancingDeveloper-org/vogt/pull/45",
        "issue 44 is the one",
        "#44",
    ],
)
def test_an_ambiguous_reference_is_not_one(text: str) -> None:
    """Precision is load-bearing: a rejected proposal is re-raised by the
    next `detect`, so a false positive is not a one-time cost."""
    assert issue_references(text) == []


def test_the_auto_accept_policy_is_asymmetric_for_forge_state() -> None:
    """Closing on observed evidence, never reopening on its absence (#49)."""
    evidence = EvidenceSnapshot(
        subject_key="gh:o/r#1",
        content_digest="d",
        observed_at=datetime(2026, 8, 12, tzinfo=UTC),
        collector="gh-issues",
    )
    closing = forge_state_mismatch(
        work_item_id="wrk_1",
        work_ref="WI-1",
        declared_state="in_progress",
        upstream_state="closed",
        subject_key="gh:o/r#1",
        project_id=None,
        evidence=evidence,
        evidence_observation_id=None,
    )
    reopening = forge_state_mismatch(
        work_item_id="wrk_1",
        work_ref="WI-1",
        declared_state="done",
        upstream_state="open",
        subject_key="gh:o/r#1",
        project_id=None,
        evidence=evidence,
        evidence_observation_id=None,
    )
    assert closing.auto_acceptable is True
    assert reopening.auto_acceptable is False
