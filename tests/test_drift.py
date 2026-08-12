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

import subprocess
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
from vogt.core.drift import UNRESOLVED_DEPENDENCY, VERSION_MISMATCH, normalise_version
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
    # A project missing AGENTS.md names exactly that criterion.
    project = tmp_path / "repo"
    _tagged_repo(project, "v1.5.0")
    for name in ("README.md", "LICENSE"):
        (project / name).write_text("x\n", encoding="utf-8")
    for name in ("docs", "design", "src"):
        (project / name).mkdir()
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
