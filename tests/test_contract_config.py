"""The contract is sourced from configuration (FR-G1).

FR-G1 asks for a contract that is declarative, carries a version identifier,
names every failing criterion, and is **sourced from configuration**. The first
three shipped at M3; the fourth did not. It was the constant `DEFAULT_CONTRACT`
in `core/contract.py`, `VogtConfig` had no contract keys, and `evaluate()` was
only ever called with the default — so nobody self-hosting Vogt could state a
contract other than this repository's own without editing Python, in a product
whose whole point is that other people run it.

The interesting half is not "a setting exists". It is what a *version* means
once the rules are editable: a recorded compliance status names the contract it
was evaluated against, and that name is worthless if two instances with
different rules both call themselves `v1`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import ContractEvaluateParams
from vogt.application.services import contract_evaluate
from vogt.config import VogtConfig
from vogt.core.contract import (
    COMPLIANT,
    DEFAULT_CONTRACT,
    NON_COMPLIANT,
    NOT_CHECKED,
    Contract,
    configured_contract,
    contract_from_settings,
    evaluate,
)
from vogt.core.principal import Principal

WHY = "contract config test"


def _config(tmp_path: Path, **overrides: object) -> VogtConfig:
    return VogtConfig(
        data_dir=tmp_path / "instance",
        sqlite_synchronous="off",
        **overrides,  # type: ignore[arg-type]
    )


# -- the setting is read ----------------------------------------------------


def test_the_default_configuration_is_the_shipped_contract(tmp_path: Path) -> None:
    """Changing where it is read from must not change what it says.

    The rules were a constant and are now four settings; if the defaults did
    not reproduce the constant exactly, every existing instance's compliance
    status would silently change meaning on upgrade.
    """
    contract = configured_contract(_config(tmp_path))

    assert contract.required_files == DEFAULT_CONTRACT.required_files
    assert contract.required_dirs == DEFAULT_CONTRACT.required_dirs
    assert contract.required_meta == DEFAULT_CONTRACT.required_meta
    assert contract.version == DEFAULT_CONTRACT.version


def test_a_configured_contract_is_what_gets_evaluated(tmp_path: Path) -> None:
    """End to end, through the operation a self-hoster would actually call.

    A project laid out to *this* deployment's rules and not to Vogt's own is
    the case FR-G1 exists for, and it fails against the stock contract — so a
    pass here can only come from the configuration being read.
    """
    project = tmp_path / "house-style"
    (project / "documentation").mkdir(parents=True)
    (project / "lib").mkdir()
    (project / "OWNERS.md").write_text("someone\n", encoding="utf-8")

    config = _config(
        tmp_path,
        contract_required_files=("OWNERS.md",),
        contract_required_dirs=("documentation", "lib"),
        contract_required_meta=(),
        contract_version="house-2026.1",
    )
    ctx = build_context(
        config=config,
        principal=Principal(
            identity_ref="local:t", kind="human", display_name="Tester"
        ),
    )

    result = contract_evaluate(ctx, ContractEvaluateParams(path=str(project)))

    assert result.status == COMPLIANT, [c.rule for c in result.failing]
    assert result.contract_version == "house-2026.1"

    # The same tree against the shipped contract is not compliant, which is
    # what makes the assertion above about configuration rather than luck.
    assert evaluate(project, DEFAULT_CONTRACT).status == NON_COMPLIANT


def test_a_failing_criterion_is_still_named_under_a_custom_contract(
    tmp_path: Path,
) -> None:
    """FR-G1's "never a bare boolean" clause does not weaken when edited."""
    project = tmp_path / "incomplete"
    project.mkdir()

    config = _config(tmp_path, contract_required_files=("HOUSE-RULES.md",))
    ctx = build_context(
        config=config,
        principal=Principal(
            identity_ref="local:t", kind="human", display_name="Tester"
        ),
    )

    result = contract_evaluate(ctx, ContractEvaluateParams(path=str(project)))

    failing = [c.target for c in result.failing]
    assert any("HOUSE-RULES.md" in target for target in failing), failing


# -- what a version means once the rules are editable -----------------------


def test_editing_the_rules_without_renaming_them_changes_the_version(
    tmp_path: Path,
) -> None:
    """A status must never claim to be the stock `v1` when it is not.

    This is the hazard FR-G1's two clauses create together: make the rules
    configurable and the version stays a free-text field an operator forgets.
    Two instances would then record `v1` against different rules and every
    comparison across them would be wrong with nothing on screen to say so.
    """
    contract = configured_contract(
        _config(tmp_path, contract_required_files=("README.md",))
    )

    assert contract.version.startswith("v1+")
    assert contract.version != "v1"


def test_a_named_contract_keeps_its_name(tmp_path: Path) -> None:
    """Deriving over the top of an explicit choice would be rude.

    Naming the contract is the deliberate act the digest infers in its
    absence; an operator who did it should not find a hash bolted on.
    """
    contract = configured_contract(
        _config(
            tmp_path,
            contract_required_files=("README.md",),
            contract_version="acme-2026.1",
        )
    )

    assert contract.version == "acme-2026.1"


def test_the_stock_rules_keep_the_stock_version(tmp_path: Path) -> None:
    """No digest where nothing was edited — otherwise every existing status
    would stop matching the contract it was recorded against."""
    assert configured_contract(_config(tmp_path)).version == "v1"


def test_the_digest_ignores_the_order_the_rules_were_written_in() -> None:
    """The same three files listed differently is the same contract.

    Without this, reformatting a config file changes every project's recorded
    contract version and invites a re-check of an estate for no reason.
    """
    one = contract_from_settings(
        version="v1",
        required_files=("b.md", "a.md"),
        required_dirs=("src",),
        required_meta=(),
    )
    other = contract_from_settings(
        version="v1",
        required_files=("a.md", "b.md"),
        required_dirs=("src",),
        required_meta=(),
    )
    assert one.version == other.version


def test_different_rules_get_different_digests() -> None:
    """The property the digest exists for, asserted rather than assumed."""
    versions = {
        contract_from_settings(
            version="v1",
            required_files=files,
            required_dirs=("src",),
            required_meta=(),
        ).version
        for files in (("a.md",), ("b.md",), ("a.md", "b.md"))
    }
    assert len(versions) == 3


@pytest.mark.parametrize(
    "field",
    ["contract_required_files", "contract_required_dirs", "contract_required_meta"],
)
def test_every_rule_set_reaches_the_contract(tmp_path: Path, field: str) -> None:
    """One helper reads all four settings, so none can be quietly dropped.

    Parametrized because the failure this guards is a call site that picks up
    three of them and misses the fourth — which reads as a working feature
    until somebody configures the one that is ignored.
    """
    contract = configured_contract(_config(tmp_path, **{field: ("marker.md",)}))
    attribute = field.removeprefix("contract_")
    assert getattr(contract, attribute) == ("marker.md",)


def test_the_contract_is_still_a_value_not_a_gate(tmp_path: Path) -> None:
    """FR-G13 does not weaken because the rules became editable.

    A configurable contract is the obvious place for enforcement to creep back
    in — "surely *my* contract should block registration". `contract check`
    against a wholly non-compliant tree returns a result, not an error.
    """
    project = tmp_path / "bare"
    project.mkdir()

    config = _config(tmp_path, contract_required_files=("NOTHING-HERE.md",))
    ctx: AppContext = build_context(
        config=config,
        principal=Principal(
            identity_ref="local:t", kind="human", display_name="Tester"
        ),
    )

    result = contract_evaluate(ctx, ContractEvaluateParams(path=str(project)))
    assert result.status == NON_COMPLIANT
    assert result.criteria, "a bare boolean is what FR-G1 forbids"
    assert result.failing, "and the failures are named, not merely counted"


def test_a_contract_can_still_be_built_by_hand(tmp_path: Path) -> None:
    """`evaluate` takes a literal contract, and that stays true.

    It is what lets a test state the rules it is about in one line, and it is
    why `core` was kept free of a runtime dependency on configuration.
    """
    del tmp_path
    result = evaluate(Path("/nonexistent"), Contract(version="hand-rolled"))
    assert result.status == NOT_CHECKED, (
        "a path that cannot be read yields no verdict; nothing was evaluated"
    )
    assert result.contract_version == "hand-rolled"


def test_an_unreadable_path_is_not_checked_rather_than_non_compliant(
    tmp_path: Path,
) -> None:
    """WI-9's shape on the contract surface (#50).

    `non_compliant` for a root nothing could read is byte-identical to the
    honest answer for a project that genuinely fails its contract — and
    `contract check` records it on the project, so the estate acquires
    verdicts from reads that never happened.
    """
    result = evaluate(tmp_path / "not-here", DEFAULT_CONTRACT)
    assert result.status == NOT_CHECKED
    assert [c.rule for c in result.criteria] == ["path.exists"], (
        "no criterion was evaluated, so none is reported as failing"
    )
    assert "not 'does not comply'" in result.criteria[0].detail
