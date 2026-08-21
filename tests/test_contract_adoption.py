"""The contract is something a project opts into (FR-G16 – FR-G19).

Eight projects were contract-checked on the dev instance and every one of
them came back `non_compliant`: `design/` failed 8 out of 8, `LICENSE` failed
8 out of 8, and `src/` failed the three that were Cargo workspaces, which
structurally cannot have a root `src/`. One word carried all three
situations, so the word carried nothing.

These tests pin the four answers to that:

* a project that never adopted the contract is `not_applicable`, and nothing
  anywhere calls it non-compliant (FR-G16);
* the scaffold `project create` writes is reachable for a project Vogt was
  merely handed, and never overwrites (FR-G17);
* an evaluation says what would close each failing criterion, and separates
  the remedies it can perform from the ones somebody has to decide (FR-G18);
* a criterion a project cannot meet by construction is declared, with a
  reason and an author, and is not a failure (FR-G19).
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    ComplianceParams,
    ContractAdoptParams,
    ContractApplicableParams,
    ContractCheckParams,
    ContractEvaluateParams,
    ContractInapplicableParams,
    CreateProjectParams,
    ListAuditParams,
    ProjectBriefParams,
    RegisterProjectParams,
    ScaffoldProjectParams,
)
from vogt.application.services import (
    brief_project,
    compliance,
    contract_adopt,
    contract_applicable,
    contract_check,
    contract_decline,
    contract_evaluate,
    contract_inapplicable,
    create_project,
    list_audit,
    register_project,
    scaffold_project,
)
from vogt.errors import NotFound

from tests.conftest import native_work_item

WHY = "contract adoption test"


def _handed(instance: AppContext, root: Path, name: str = "Handed") -> str:
    """Register a folder Vogt did not create, the way a user would."""
    register_project(
        instance, RegisterProjectParams(name=name, root_path=str(root), reason=WHY)
    )
    return name.lower()


# -- FR-G16: opt-in, and never a fault ------------------------------------


def test_a_handed_project_is_not_applicable_rather_than_non_compliant(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path)

    status = compliance(instance, ComplianceParams(project=slug))

    assert status.status == "not_applicable"
    assert status.adopted is False
    assert status.failing == [], "a project nobody measured has no failures"
    assert "not a fault" in (status.detail or "")


def test_checking_an_unadopted_project_records_nothing(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G16: `FR-G14`'s statuses apply only to projects that adopted one."""
    slug = _handed(instance, tmp_path)

    checked = contract_check(instance, ContractCheckParams(project=slug, reason=WHY))

    assert checked.status == "not_applicable"
    assert checked.recorded is False
    assert checked.criteria == [], "no criterion is reported against a project "
    assert checked.failing == []
    # And the brief agrees, because a view that disagreed with the answer
    # would be a second opinion nobody asked for.
    assert brief_project(instance, ProjectBriefParams(slug=slug)).compliance_status == (
        "not_applicable"
    )
    assert compliance(instance, ComplianceParams(project=slug)).status == (
        "not_applicable"
    )


def test_adopting_makes_the_contract_apply_and_declining_takes_it_back(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path)

    adopted = contract_adopt(instance, ContractAdoptParams(project=slug, reason=WHY))
    assert adopted.adopted is True
    assert adopted.adopted_at is not None

    checked = contract_check(instance, ContractCheckParams(project=slug, reason=WHY))
    assert checked.status == "non_compliant"
    assert checked.recorded is True

    declined = contract_decline(instance, ContractAdoptParams(project=slug, reason=WHY))
    assert declined.adopted is False
    # The verdict does not survive the withdrawal: a project that is no longer
    # measured has nothing to have been found non-compliant about.
    assert compliance(instance, ComplianceParams(project=slug)).status == (
        "not_applicable"
    )


def test_creating_a_project_is_itself_the_opting_in(
    instance: AppContext, tmp_path: Path
) -> None:
    """Asking Vogt for a contract-shaped project *is* adopting the contract.

    The asymmetry this whole change is about ran the other way: a project
    Vogt created got the template, a project Vogt was handed got a verdict.
    Neither half of that was chosen by the person holding the project.
    """
    create_project(
        instance,
        CreateProjectParams(
            name="Made Here", root_path=str(tmp_path / "made"), reason=WHY
        ),
    )

    status = compliance(instance, ComplianceParams(project="made-here"))
    assert status.adopted is True
    assert status.status == "not_checked", "adopted, and nobody has looked yet"


def test_adoption_is_audited_with_its_reason(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path)

    contract_adopt(
        instance,
        ContractAdoptParams(project=slug, reason="we want the conventions here"),
    )

    records = list_audit(instance, ListAuditParams(limit=50)).records
    adoption = [r for r in records if r.operation == "contract.adopt"]
    assert len(adoption) == 1
    assert adoption[0].reason == "we want the conventions here"


def test_a_project_that_declined_is_never_refused_anything(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-G13 still holds, and FR-G16 does not become a new gate."""
    slug = _handed(instance, tmp_path)

    # FR-G13 / FR-G16: contract state gates nothing. The write plane's own
    # NotLinked refusal (#181, decision 10) is about link state, not the
    # contract, so the declared row still lands the audited way here.
    created = native_work_item(
        instance, kind="bug", title="Tracked anyway", project=slug
    )

    assert created.ref.startswith("WI-")


# -- FR-G17: the scaffold reaches a project Vogt was handed ---------------


def test_scaffolding_an_existing_project_writes_the_skeleton_once(
    instance: AppContext, tmp_path: Path
) -> None:
    (tmp_path / "README.md").write_text("mine, and not yours to rewrite\n")
    slug = _handed(instance, tmp_path)

    first = scaffold_project(instance, ScaffoldProjectParams(project=slug, reason=WHY))

    assert any(path.endswith("AGENTS.md") for path in first.created)
    assert any(path.endswith("README.md") for path in first.skipped)
    assert (tmp_path / "README.md").read_text() == "mine, and not yours to rewrite\n", (
        "skipped means untouched, which is what makes this safe to offer"
    )
    assert (tmp_path / "docs").is_dir()

    second = scaffold_project(instance, ScaffoldProjectParams(project=slug, reason=WHY))
    assert second.created == [], "everything was already there the second time"


def test_scaffolding_says_so_when_the_root_is_not_there(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path / "gone", name="Gone")

    with pytest.raises(NotFound):
        scaffold_project(instance, ScaffoldProjectParams(project=slug, reason=WHY))


def test_scaffolding_does_not_adopt_the_contract_behind_the_owner(
    instance: AppContext, tmp_path: Path
) -> None:
    """Writing files and agreeing to be measured are different acts."""
    slug = _handed(instance, tmp_path)

    scaffold_project(instance, ScaffoldProjectParams(project=slug, reason=WHY))

    assert compliance(instance, ComplianceParams(project=slug)).adopted is False


# -- FR-G18: a reported gap comes with the means to close it --------------


def test_an_evaluation_says_what_would_close_each_failing_criterion(
    tmp_path: Path, instance: AppContext
) -> None:
    result = contract_evaluate(instance, ContractEvaluateParams(path=str(tmp_path)))

    advice = {one.target: one for one in result.recommendations}
    assert advice, "a report with no remedy is the enforcing posture in a costume"
    assert advice["AGENTS.md"].remedy == "scaffold"
    assert "project scaffold" in advice["AGENTS.md"].instruction
    # The two the product must not decide for anybody.
    assert advice["LICENSE"].remedy == "judgement"
    assert "does not pick one" in advice["LICENSE"].instruction
    assert advice["design"].remedy == "judgement"


def test_recommendations_are_advisory_and_apply_nothing(
    instance: AppContext, tmp_path: Path
) -> None:
    contract_evaluate(instance, ContractEvaluateParams(path=str(tmp_path)))

    assert not (tmp_path / "AGENTS.md").exists(), (
        "a recommendation that wrote the file would be the gate again"
    )


# -- FR-G19: unmet, and unmeetable, are different facts -------------------


def test_a_criterion_declared_inapplicable_is_reported_and_not_failed(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path, name="Workspace")
    contract_adopt(instance, ContractAdoptParams(project=slug, reason=WHY))

    declared = contract_inapplicable(
        instance,
        ContractInapplicableParams(
            project=slug,
            rule="required_dir",
            target="src",
            reason="a Cargo workspace has no root src/",
        ),
    )
    assert declared.declared is True

    checked = contract_check(instance, ContractCheckParams(project=slug, reason=WHY))

    failing = {c.target for c in checked.failing}
    assert "src" not in failing, "an unmeetable criterion is not a failed one"
    inapplicable = {c.target: c for c in checked.inapplicable}
    assert "src" in inapplicable, "and it is still reported, with the argument"
    assert "Cargo workspace" in inapplicable["src"].detail
    assert not any(one.target == "src" for one in checked.recommendations), (
        "there is nothing to recommend about a criterion that cannot apply"
    )


def test_an_inapplicability_declaration_carries_an_author_and_a_reason(
    instance: AppContext, tmp_path: Path
) -> None:
    slug = _handed(instance, tmp_path)
    contract_adopt(instance, ContractAdoptParams(project=slug, reason=WHY))

    result = contract_inapplicable(
        instance,
        ContractInapplicableParams(
            project=slug,
            rule="required_dir",
            target="design",
            reason="this repository's design record lives in the estate wiki",
        ),
    )

    assert len(result.exemptions) == 1
    exemption = result.exemptions[0]
    assert exemption.target == "design"
    assert exemption.declared_by
    assert "estate wiki" in exemption.reason

    records = list_audit(instance, ListAuditParams(limit=50)).records
    assert any(r.operation == "contract.inapplicable" for r in records), (
        "a silent exemption is the thing this requirement forbids"
    )


def test_a_declaration_can_be_withdrawn(instance: AppContext, tmp_path: Path) -> None:
    slug = _handed(instance, tmp_path)
    contract_adopt(instance, ContractAdoptParams(project=slug, reason=WHY))
    contract_inapplicable(
        instance,
        ContractInapplicableParams(
            project=slug, rule="required_dir", target="src", reason="no root src/"
        ),
    )

    withdrawn = contract_applicable(
        instance,
        ContractApplicableParams(
            project=slug,
            rule="required_dir",
            target="src",
            reason="the layout changed and there is a root src/ now",
        ),
    )

    assert withdrawn.exemptions == []
    checked = contract_check(instance, ContractCheckParams(project=slug, reason=WHY))
    assert "src" in {c.target for c in checked.failing}


def test_compliance_stops_reporting_a_failure_somebody_declared_unmeetable(
    instance: AppContext, tmp_path: Path
) -> None:
    """The recorded answer predates the declaration; the reading must not."""
    slug = _handed(instance, tmp_path)
    contract_adopt(instance, ContractAdoptParams(project=slug, reason=WHY))
    contract_check(instance, ContractCheckParams(project=slug, reason=WHY))
    assert "src" in {
        c.target for c in compliance(instance, ComplianceParams(project=slug)).failing
    }

    contract_inapplicable(
        instance,
        ContractInapplicableParams(
            project=slug, rule="required_dir", target="src", reason="no root src/"
        ),
    )

    status = compliance(instance, ComplianceParams(project=slug))
    assert "src" not in {c.target for c in status.failing}
    assert "src" in {c.target for c in status.inapplicable}
