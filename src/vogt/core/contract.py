"""The default project contract.

At M1 this exists for one reason: `project create` scaffolds a compliant
skeleton (FR-G11), and it has to know what compliant *looks* like. Evaluating
the contract and recording a compliance status is M3 (FR-G1, FR-G3, FR-G14),
and this is where that lands — as configuration carrying a version
identifier, so a recorded status can name what it was evaluated against.

Nothing here gates anything, now or later. Registering a non-compliant folder
succeeds and reports its status; the contract is a value you read, not a
barrier you pass (FR-G13, DESIGN §2.1, §5).
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING

if TYPE_CHECKING:  # pragma: no cover - typing only, see `configured_contract`
    from vogt.config import VogtConfig

#: Bumped when the required set changes, so a recorded status can say which
#: contract produced it.
DEFAULT_CONTRACT_VERSION = "v1"


@dataclass(frozen=True)
class Contract:
    """What a compliant project looks like (DESIGN §5)."""

    version: str = DEFAULT_CONTRACT_VERSION
    required_files: tuple[str, ...] = ("AGENTS.md", "README.md", "LICENSE")
    required_dirs: tuple[str, ...] = ("docs", "design", "src")
    required_meta: tuple[str, ...] = ("name", "lifecycle_state", "owner")


DEFAULT_CONTRACT = Contract()


def contract_from_settings(
    *,
    version: str,
    required_files: tuple[str, ...],
    required_dirs: tuple[str, ...],
    required_meta: tuple[str, ...],
) -> Contract:
    """Build the contract this instance evaluates against (FR-G1).

    Takes primitives rather than a config object, so `core` keeps knowing
    nothing about configuration — the application layer and the collector
    each pass what they read.

    **The version is derived when it would otherwise lie.** FR-G1 asks for a
    contract sourced from configuration *and* for a version identifier a
    recorded status can name. Those two pull against each other: an operator
    who edits the rules and leaves the version alone produces statuses that
    claim to be the stock `v1` and are not, and every comparison across
    instances after that is wrong in a way nothing surfaces. So a contract
    that differs from the built-in default while still carrying the default
    version gets a short digest of its own rules appended — `v1+3f9a2c`.

    An operator who *names* their contract keeps that name untouched: saying
    `contract_version = "acme-2026.1"` is the explicit act this is inferring
    in its absence, and inferring over the top of it would be rude.
    """
    contract = Contract(
        version=version,
        required_files=tuple(required_files),
        required_dirs=tuple(required_dirs),
        required_meta=tuple(required_meta),
    )
    stock_rules = (
        contract.required_files == DEFAULT_CONTRACT.required_files
        and contract.required_dirs == DEFAULT_CONTRACT.required_dirs
        and contract.required_meta == DEFAULT_CONTRACT.required_meta
    )
    if stock_rules or version != DEFAULT_CONTRACT_VERSION:
        return contract
    return Contract(
        version=f"{version}+{rules_digest(contract)}",
        required_files=contract.required_files,
        required_dirs=contract.required_dirs,
        required_meta=contract.required_meta,
    )


def configured_contract(config: VogtConfig) -> Contract:
    """The contract this instance evaluates against, from configuration.

    The one place the four settings are read, so a call site cannot pick up
    three of them and miss the fourth. `VogtConfig` is imported for typing
    only: `core` stays free of a runtime dependency on configuration, which
    is what keeps `evaluate` callable from a test with a literal contract.
    """
    return contract_from_settings(
        version=config.contract_version,
        required_files=config.contract_required_files,
        required_dirs=config.contract_required_dirs,
        required_meta=config.contract_required_meta,
    )


def rules_digest(contract: Contract) -> str:
    """A short, stable fingerprint of a contract's rules.

    Order-insensitive within each set, because listing the same three files
    in a different order is the same contract and should not read as a
    different one.
    """
    material = "|".join(
        ",".join(sorted(part))
        for part in (
            contract.required_files,
            contract.required_dirs,
            contract.required_meta,
        )
    )
    return hashlib.sha256(material.encode("utf-8")).hexdigest()[:6]


@dataclass(frozen=True)
class ScaffoldFile:
    """One file `project create` writes, if it is not already there."""

    path: str
    content: str


@dataclass(frozen=True)
class Scaffold:
    """The skeleton `project create` lays down."""

    directories: tuple[str, ...] = field(default=DEFAULT_CONTRACT.required_dirs)
    files: tuple[ScaffoldFile, ...] = ()


def default_scaffold(*, name: str, owner: str, lifecycle_state: str) -> Scaffold:
    """Build a compliant skeleton for a new project.

    Deliberately thin. A scaffold that writes a lot of opinionated content is
    a scaffold people delete; this writes the minimum the contract requires
    and says, in each file, what it is for.
    """
    readme = (
        f"# {name}\n\n"
        "One line on what this is and who it is for.\n\n"
        "## Status\n\n"
        f"`{lifecycle_state}`.\n"
    )
    agents = (
        f"# {name} — Agent Guidance\n\n"
        "## Where things live\n\n"
        "- `docs/` — first-class documentation.\n"
        "- `design/` — diagrams, mockups, exploratory notes (may be messy).\n"
        "- `src/` — implementation.\n\n"
        "## Ground rules\n\n"
        "- Record decisions where the next reader will look for them.\n"
    )
    # No licence text is invented here: picking a licence is the owner's
    # decision, and a scaffold that silently writes MIT makes it for them.
    licence = (
        f"Copyright (c) {owner}\n\n"
        "TODO: choose a licence and replace this file with its full text.\n"
    )
    return Scaffold(
        directories=DEFAULT_CONTRACT.required_dirs,
        files=(
            ScaffoldFile("README.md", readme),
            ScaffoldFile("AGENTS.md", agents),
            ScaffoldFile("LICENSE", licence),
        ),
    )


# -- evaluation (M3) -------------------------------------------------------


@dataclass(frozen=True)
class CriterionResult:
    """One rule, evaluated, with its answer.

    FR-G3: a contract check returns the specific rules evaluated and the
    specific criteria that failed — never a bare pass/fail. A boolean tells
    you that something is wrong; this tells you what to do.
    """

    rule: str
    target: str
    satisfied: bool
    detail: str


@dataclass(frozen=True)
class ContractResult:
    """The outcome of evaluating a contract against a path."""

    contract_version: str
    path: str
    status: str
    criteria: tuple[CriterionResult, ...]

    @property
    def failing(self) -> tuple[CriterionResult, ...]:
        return tuple(c for c in self.criteria if not c.satisfied)


#: The three answers a compliance status can take. `not_checked` is
#: first-class and unembarrassing: nobody has looked, and saying so is more
#: honest than a silently refreshed `compliant` (DESIGN §5).
COMPLIANT = "compliant"
NON_COMPLIANT = "non_compliant"
NOT_CHECKED = "not_checked"


def evaluate(path: Path, contract: Contract = DEFAULT_CONTRACT) -> ContractResult:
    """Check a folder or repository against a contract.

    Works on any path, registered or not (FR-G4), and blocks nothing
    (FR-G13). The result is a value to read.
    """
    root = Path(path).expanduser()
    criteria: list[CriterionResult] = []

    if not root.is_dir():
        criteria.append(
            CriterionResult(
                rule="path.exists",
                target=str(root),
                satisfied=False,
                detail="the path does not exist or is not a directory",
            )
        )
        return ContractResult(
            contract_version=contract.version,
            path=str(root),
            status=NON_COMPLIANT,
            criteria=tuple(criteria),
        )

    criteria.append(
        CriterionResult(
            rule="path.exists",
            target=str(root),
            satisfied=True,
            detail="directory exists",
        )
    )
    for name in contract.required_files:
        present = (root / name).is_file()
        criteria.append(
            CriterionResult(
                rule="required_file",
                target=name,
                satisfied=present,
                detail="present" if present else f"{name} is missing",
            )
        )
    for name in contract.required_dirs:
        present = (root / name).is_dir()
        criteria.append(
            CriterionResult(
                rule="required_dir",
                target=name,
                satisfied=present,
                detail="present" if present else f"{name}/ is missing",
            )
        )

    failing = [c for c in criteria if not c.satisfied]
    return ContractResult(
        contract_version=contract.version,
        path=str(root),
        status=NON_COMPLIANT if failing else COMPLIANT,
        criteria=tuple(criteria),
    )
