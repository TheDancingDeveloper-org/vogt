"""Contract checks: a status you read, never a barrier (FR-G3, G4, G13, G14)."""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    ComplianceParams,
    ContractCheckParams,
    CreateProjectParams,
    CreateWorkParams,
    ProjectBriefParams,
    RegisterProjectParams,
    SweepParams,
    TransitionProjectParams,
)
from vogt.application.services import (
    brief_project,
    compliance,
    contract_check,
    create_project,
    create_work,
    register_project,
    sweep,
    transition_project,
)
from vogt.core.contract import DEFAULT_CONTRACT, evaluate
from vogt.errors import InvalidRequest

WHY = "contract test"


def _compliant(root: Path) -> Path:
    for name in DEFAULT_CONTRACT.required_files:
        (root / name).write_text("x\n", encoding="utf-8")
    for name in DEFAULT_CONTRACT.required_dirs:
        (root / name).mkdir(parents=True, exist_ok=True)
    return root


# -- the evaluation itself -------------------------------------------------


def test_a_check_names_every_rule_not_just_the_failures(tmp_path: Path) -> None:
    """FR-G3: never a bare pass/fail."""
    result = evaluate(tmp_path)
    assert result.status == "non_compliant"
    assert len(result.criteria) > len(result.failing), (
        "the rules that passed are part of the answer too"
    )
    assert {c.target for c in result.failing} >= {"AGENTS.md", "docs"}


def test_a_compliant_project_passes(tmp_path: Path) -> None:
    assert evaluate(_compliant(tmp_path)).status == "compliant"


def test_a_missing_path_fails_with_a_useful_rule(tmp_path: Path) -> None:
    result = evaluate(tmp_path / "absent")
    assert [c.rule for c in result.failing] == ["path.exists"]


def test_this_repository_satisfies_its_own_contract() -> None:
    """NFR-O3, checked against the real thing rather than a fixture."""
    repo = Path(__file__).resolve().parents[1]
    result = evaluate(repo)
    assert result.status == "compliant", [c.detail for c in result.failing]


# -- through the service ---------------------------------------------------


def test_checking_a_bare_path_stores_nothing(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G4: invocable against any folder, without registering it."""
    result = contract_check(
        instance, ContractCheckParams(path=str(tmp_path), reason=WHY)
    )
    assert result.recorded is False
    assert result.project is None
    with instance.declared.read() as view:
        assert view.counts().projects == 0


def test_checking_a_registered_project_records_the_result(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G14: recorded with failing criteria and a checked-at timestamp."""
    register_project(
        instance,
        RegisterProjectParams(name="Bare", root_path=str(tmp_path), reason=WHY),
    )
    result = contract_check(instance, ContractCheckParams(project="bare", reason=WHY))

    assert result.recorded is True
    assert result.status == "non_compliant"
    assert result.checked_at is not None

    status = compliance(instance, ComplianceParams(project="bare"))
    assert status.status == "non_compliant"
    assert status.age_seconds is not None
    assert {c.target for c in status.failing} >= {"AGENTS.md"}


def test_the_demo_case_names_exactly_the_missing_file(
    instance: AppContext, tmp_path: Path
) -> None:
    """The M3 demo: a project missing AGENTS.md names exactly that criterion."""
    _compliant(tmp_path)
    (tmp_path / "AGENTS.md").unlink()
    register_project(
        instance,
        RegisterProjectParams(name="Nearly", root_path=str(tmp_path), reason=WHY),
    )
    result = contract_check(instance, ContractCheckParams(project="nearly", reason=WHY))
    assert [c.target for c in result.failing] == ["AGENTS.md"]
    assert "AGENTS.md is missing" in result.failing[0].detail


def test_a_never_checked_project_says_so(instance: AppContext, tmp_path: Path) -> None:
    """`not_checked` is a first-class, unembarrassing answer."""
    register_project(
        instance,
        RegisterProjectParams(name="Fresh", root_path=str(tmp_path), reason=WHY),
    )
    status = compliance(instance, ComplianceParams(project="fresh"))
    assert status.status == "not_checked"
    assert status.checked_at is None
    assert status.age_seconds is None
    assert status.detail is not None and "contract check" in status.detail


def test_the_status_is_never_refreshed_implicitly(
    instance: AppContext, tmp_path: Path
) -> None:
    """Reading a value must not change it — that is the whole r3 posture."""
    register_project(
        instance,
        RegisterProjectParams(name="Static", root_path=str(tmp_path), reason=WHY),
    )
    contract_check(instance, ContractCheckParams(project="static", reason=WHY))
    first = compliance(instance, ComplianceParams(project="static"))

    _compliant(tmp_path)  # the project becomes compliant on disk...

    second = compliance(instance, ComplianceParams(project="static"))
    assert second.status == "non_compliant", "...and the recorded answer does not move"
    assert second.checked_at == first.checked_at

    contract_check(instance, ContractCheckParams(project="static", reason="ask again"))
    assert compliance(instance, ComplianceParams(project="static")).status == (
        "compliant"
    )


def test_the_brief_shows_compliance_with_its_age(
    instance: AppContext, tmp_path: Path
) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Briefed", root_path=str(tmp_path), reason=WHY),
    )
    contract_check(instance, ContractCheckParams(project="briefed", reason=WHY))
    brief = brief_project(instance, ProjectBriefParams(slug="briefed"))
    assert brief.compliance_status == "non_compliant"
    assert brief.compliance_checked_at is not None


def test_a_plain_sweep_does_not_check_the_contract(
    instance: AppContext, tmp_path: Path
) -> None:
    """r3: nothing re-checks compliance on a timer."""
    register_project(
        instance,
        RegisterProjectParams(name="Unchecked", root_path=str(tmp_path), reason=WHY),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    assert compliance(instance, ComplianceParams(project="unchecked")).status == (
        "not_checked"
    )


def test_checking_needs_exactly_one_target(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="--path or --project"):
        contract_check(instance, ContractCheckParams(reason=WHY))
    with pytest.raises(InvalidRequest, match="not both"):
        contract_check(
            instance, ContractCheckParams(path="/tmp", project="x", reason=WHY)
        )


# -- FR-G13: nothing consumes compliance as a precondition ----------------


def test_a_non_compliant_project_refuses_nothing(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G13, asserted rather than asserted-about.

    Registration, work creation and lifecycle transitions all succeed
    against a project that fails its contract. Compliance is a value you
    read; nothing branches on it.
    """
    register_project(
        instance,
        RegisterProjectParams(name="Messy", root_path=str(tmp_path), reason=WHY),
    )
    contract_check(instance, ContractCheckParams(project="messy", reason=WHY))
    assert compliance(instance, ComplianceParams(project="messy")).status == (
        "non_compliant"
    )

    created = create_work(
        instance,
        CreateWorkParams(
            kind="bug", title="Still allowed", project="messy", reason=WHY
        ),
    )
    assert created.item.project_slug == "messy"

    moved = transition_project(
        instance,
        TransitionProjectParams(slug="messy", to_state="maintenance", reason=WHY),
    )
    assert moved.project.lifecycle_state == "maintenance"


def test_no_code_path_branches_on_compliance_status() -> None:
    """A structural check, so the rule survives people who have not read it.

    Only the modules that *compute or report* compliance may mention the
    statuses. If a service starts comparing against them, FR-G13 has been
    broken and this fails.
    """
    root = Path(__file__).resolve().parents[1] / "src" / "vogt"
    allowed = {
        Path("core/contract.py"),
        Path("core/entities.py"),
        Path("application/models.py"),
        Path("application/services/contracts.py"),
        Path("application/services/projects.py"),
        Path("collectors/contract_checker.py"),
        Path("storage/interface.py"),
        Path("storage/sqlite/declared.py"),
    }
    offenders = []
    for path in root.rglob("*.py"):
        relative = path.relative_to(root)
        if relative in allowed:
            continue
        text = path.read_text(encoding="utf-8")
        for needle in ("non_compliant", '"compliant"', "'compliant'"):
            if needle in text:
                offenders.append(f"{relative}: {needle}")
    assert offenders == [], (
        "FR-G13: compliance is a value to be read, and these modules look "
        f"like they branch on it: {offenders}"
    )


def test_project_create_scaffolds_something_compliant(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G11: starting compliant is easier than becoming compliant."""
    target = tmp_path / "brand-new"
    create_project(
        instance,
        CreateProjectParams(
            name="Brand New", root_path=str(target), owner="tester", reason=WHY
        ),
    )
    result = contract_check(
        instance, ContractCheckParams(project="brand-new", reason=WHY)
    )
    assert result.status == "compliant", [c.detail for c in result.failing]
