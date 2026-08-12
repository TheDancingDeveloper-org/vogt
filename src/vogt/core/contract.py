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
