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
from collections.abc import Mapping
from dataclasses import dataclass, field, replace
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
    applicable: bool = True
    """Whether this criterion can apply to this project at all (FR-G19).

    `False` is a declaration somebody made and gave a reason for, not an
    inference: a Cargo workspace has no root `src/`, and reporting that as a
    failure says something about the contract rather than about the project.
    An inapplicable criterion is reported and is not counted as failing.
    """
    tracked: bool | None = None
    """Whether the repository carries this, where that could be asked.

    `None` means it could not be — an unregistered path, or a directory that
    is not a checkout. `False` on a criterion whose file is sitting right
    there is the case this field exists for (FR-G3): present on one disk,
    absent from every clone.
    """


@dataclass(frozen=True)
class ContractResult:
    """The outcome of evaluating a contract against a path."""

    contract_version: str
    path: str
    status: str
    criteria: tuple[CriterionResult, ...]

    @property
    def failing(self) -> tuple[CriterionResult, ...]:
        return tuple(c for c in self.criteria if not c.satisfied and c.applicable)

    @property
    def inapplicable(self) -> tuple[CriterionResult, ...]:
        return tuple(c for c in self.criteria if not c.applicable)


#: The three answers a compliance status can take. `not_checked` is
#: first-class and unembarrassing: nobody has looked, and saying so is more
#: honest than a silently refreshed `compliant` (DESIGN §5).
COMPLIANT = "compliant"
NON_COMPLIANT = "non_compliant"
NOT_CHECKED = "not_checked"

#: The fourth answer, and the only one that is never stored on a project row
#: (FR-G16): this project never adopted the contract, so there is nothing to
#: comply with. It is not a fault, and no view may present it as one.
NOT_APPLICABLE = "not_applicable"


def _criterion(
    rule: str, name: str, on_disk: bool, tracked: frozenset[str] | None
) -> CriterionResult:
    """One criterion, answered against the repository where there is one."""
    suffix = "/" if rule == "required_dir" else ""
    if not on_disk:
        return CriterionResult(
            rule=rule,
            target=name,
            satisfied=False,
            detail=f"{name}{suffix} is missing",
            tracked=None if tracked is None else False,
        )
    if tracked is None:
        return CriterionResult(
            rule=rule, target=name, satisfied=True, detail="present", tracked=None
        )
    if name in tracked:
        return CriterionResult(
            rule=rule, target=name, satisfied=True, detail="present", tracked=True
        )
    return CriterionResult(
        rule=rule,
        target=name,
        satisfied=False,
        detail=(
            f"{name}{suffix} is present in the working tree but not tracked, "
            "so no clone of this repository has it"
        ),
        tracked=False,
    )


def _exempted(
    criterion: CriterionResult, inapplicable: Mapping[tuple[str, str], str]
) -> CriterionResult:
    """Re-read a criterion somebody declared inapplicable here (FR-G19).

    The declaration does not make the criterion *satisfied* — the file is
    still absent, and pretending otherwise would be the silent exemption this
    is arranged against. It makes it not-counted, and it carries the reason
    given for that, so a reader sees both the fact and the argument.
    """
    reason = inapplicable.get((criterion.rule, criterion.target))
    if reason is None or criterion.satisfied:
        return criterion
    return replace(
        criterion,
        applicable=False,
        detail=f"declared inapplicable to this project: {reason}",
    )


def evaluate(
    path: Path,
    contract: Contract = DEFAULT_CONTRACT,
    *,
    tracked: frozenset[str] | None = None,
    inapplicable: Mapping[tuple[str, str], str] | None = None,
) -> ContractResult:
    """Check a folder or repository against a contract.

    Works on any path, registered or not (FR-G4), and blocks nothing
    (FR-G13). The result is a value to read.

    `tracked` is the set of top-level names the repository carries, from a
    caller that is allowed to run git — `None` where nobody could ask. A
    criterion satisfied on disk but absent from that set **fails**, because
    the criterion means "this repository carries its licence", not "this disk
    has a file with that name today". Two projects onboarded on 2026-08-16
    were recorded as satisfying `AGENTS.md` on an untracked 288-byte stub —
    one of sixty-three byte-identical copies a tool had dropped around the
    workspace — and one satisfied `docs/` on an empty untracked directory git
    cannot represent even in principle, while its real documentation sat in
    `documentation/`. Clone either repository and neither criterion holds.

    This module stays pure: the caller does the asking, and passing `None`
    keeps the old filesystem-only behaviour for a path with no repository
    behind it.

    `inapplicable` maps `(rule, target)` to the reason somebody declared that
    the criterion cannot apply here (FR-G19). Those criteria are still
    reported — an unmeetable criterion is a fact worth reading — but they are
    not counted as failures, and their reason travels with them.
    """
    root = Path(path).expanduser()
    criteria: list[CriterionResult] = []

    if not root.is_dir():
        # `not_checked`, not `non_compliant`. Nothing was read, so nothing
        # was evaluated, and a verdict from a read that never happened is
        # the same shape WI-9 fixed for `forge onboard`: an answer
        # indistinguishable from the honest one for a project that genuinely
        # fails its contract (#50). The status a project records for an
        # unreachable root now says nobody could look, which is true.
        criteria.append(
            CriterionResult(
                rule="path.exists",
                target=str(root),
                satisfied=False,
                detail=(
                    "the path does not exist or is not a directory, so no "
                    "criterion below was evaluated — this is 'not checked', "
                    "not 'does not comply'"
                ),
            )
        )
        return ContractResult(
            contract_version=contract.version,
            path=str(root),
            status=NOT_CHECKED,
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
    exempt = dict(inapplicable or {})
    for name in contract.required_files:
        criteria.append(
            _exempted(
                _criterion("required_file", name, (root / name).is_file(), tracked),
                exempt,
            )
        )
    for name in contract.required_dirs:
        criteria.append(
            _exempted(
                _criterion("required_dir", name, (root / name).is_dir(), tracked),
                exempt,
            )
        )

    failing = [c for c in criteria if not c.satisfied and c.applicable]
    return ContractResult(
        contract_version=contract.version,
        path=str(root),
        status=NON_COMPLIANT if failing else COMPLIANT,
        criteria=tuple(criteria),
    )


# -- recommendations (FR-G18, r14) -----------------------------------------
#
# Reporting a gap the product can close, without offering to close it, is the
# enforcing posture in a different costume. So an evaluation can say what
# would close each failing criterion — and it distinguishes the two kinds of
# remedy, because they are addressed to different readers.
#
# A *mechanical* remedy is one the scaffold already writes: `AGENTS.md` has
# template text, `docs/` is a directory. `project scaffold` performs it, and
# the recommendation names that operation.
#
# A *judgement* remedy is one nothing here should make. Which licence a
# project carries is its owner's decision — `default_scaffold` refuses to
# invent one for exactly this reason — and what belongs in `design/` is a
# question about the project, not about the contract. Those recommendations
# are instructions addressed to an actor: readable by a person, executable by
# an agent, applied by neither implicitly.


@dataclass(frozen=True)
class Recommendation:
    """What would close one failing criterion, and who has to decide it."""

    rule: str
    target: str
    #: `scaffold` where the product can write it; `judgement` where a person
    #: or an agent has to decide what it should contain.
    remedy: str
    instruction: str


#: Criteria whose content is a decision rather than a template. The scaffold
#: writes a placeholder for `LICENSE` and nothing for `design/`; neither is a
#: remedy, and calling them one would be the silent opinion r2 removed.
JUDGEMENT_TARGETS: dict[str, str] = {
    "LICENSE": (
        "choose a licence for this project and write its full text to "
        "LICENSE. Vogt deliberately does not pick one: a scaffold that "
        "silently writes MIT makes that decision for the owner."
    ),
    "design": (
        "decide what this project's design record is — diagrams, mockups, "
        "exploratory notes — and put it in design/. An empty directory "
        "satisfies the letter of the criterion and none of its point, and "
        "git cannot carry one anyway."
    ),
}


def recommendations(
    result: ContractResult, *, scaffold: Scaffold | None = None
) -> tuple[Recommendation, ...]:
    """What would close each failing criterion of an evaluation (FR-G18).

    Advisory output. Nothing here applies anything, and nothing that consumes
    it may treat it as authority: FR-G13's rule that compliance is never a
    precondition covers the remedy as surely as it covers the verdict.

    Criteria declared inapplicable are absent, because there is nothing to
    recommend about a criterion somebody has said cannot apply.
    """
    written = {file.path for file in (scaffold.files if scaffold else ())}
    directories = set(scaffold.directories if scaffold else ())

    advice: list[Recommendation] = []
    for criterion in result.failing:
        judgement = JUDGEMENT_TARGETS.get(criterion.target)
        if judgement is not None:
            advice.append(
                Recommendation(
                    rule=criterion.rule,
                    target=criterion.target,
                    remedy="judgement",
                    instruction=judgement,
                )
            )
            continue
        if criterion.target in written or criterion.target in directories:
            advice.append(
                Recommendation(
                    rule=criterion.rule,
                    target=criterion.target,
                    remedy="scaffold",
                    instruction=(
                        f"`project scaffold` writes {criterion.target}"
                        f"{'/' if criterion.rule == 'required_dir' else ''} "
                        "into this project without overwriting anything that "
                        "is already there."
                    ),
                )
            )
            continue
        if criterion.tracked is False:
            advice.append(
                Recommendation(
                    rule=criterion.rule,
                    target=criterion.target,
                    remedy="judgement",
                    instruction=(
                        f"{criterion.target} is in the working tree but no "
                        "clone carries it. Commit it, or decide it does not "
                        "belong in this repository."
                    ),
                )
            )
            continue
        advice.append(
            Recommendation(
                rule=criterion.rule,
                target=criterion.target,
                remedy="judgement",
                instruction=(
                    f"decide what {criterion.target} should contain for this "
                    "project and add it. Nothing here can write it for you."
                ),
            )
        )
    return tuple(advice)
