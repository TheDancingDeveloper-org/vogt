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

from dataclasses import dataclass, field
from pathlib import Path

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
