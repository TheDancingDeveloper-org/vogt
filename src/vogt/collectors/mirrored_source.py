"""`mirrored-source` — the same source in two places, reported and not judged.

FR-D8, and the shape it exists for (DESIGN §3.5): **rustnzb** vendors
`crates/nzb-core` as a path member of its own workspace, and `nzb-core` is
*also* a separately registered project whose repository is what publishes to
crates.io. Both facts are already true in Vogt — the path reference is a
`dep_ref` scoped `internal`, and the standalone crate is a project — and
nothing joined them, so the two read as unrelated things that happen to share
a name. The estate onboarding of 2026-08-17 produced eighteen of these
(`rustnzb`'s seven `nzb-*` crates, `rustTorrent`'s eleven `librtbit-*`) and
had to work each one out by hand.

**What this reports, and what it deliberately does not.** It reports the
relationship: this path inside that project declares the package that other
registered project publishes. It does not compare the two copies, does not
diff them, and never says one is behind — FR-D8 forbids all three, because
answering "have these diverged" needs resolved versions, and r2 deleted the
subsystem that would resolve them. The declared versions on each side are
recorded as the facts they are; nothing here subtracts one from the other.

**Why matching is on the declared package name.** It is the identity both
copies agree on, it is already in the manifests both sides carry, and it is
free. The alternative — hashing trees — is the content comparison FR-D8 rules
out. A name that two registered projects both claim is dropped rather than
guessed at: an ambiguous match is not a finding, and reporting one would put
Vogt in the business of asserting which copy is real.

**Why a collector of its own rather than more of `dep-refs`.** Coverage. A
sweep records which collector looked at what (FR-O3), and folding this into
`dep-refs` would leave no way to tell "no mirrors here" from "nothing looked
for mirrors" — the distinction the observation layer exists to keep (FR-O4).
It costs a second walk of the manifests and buys a row that says so.

The registered project list is handed down by the application layer, the same
way `session-outcomes` is handed its sessions: collectors never read declared
data themselves (FR-O2).
"""

from __future__ import annotations

import json
import tomllib
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from vogt.collectors.base import CollectorContext, Finding, finding, walk_project
from vogt.core.entities import Project

KIND_MIRRORED_SOURCE = "mirrored_source"

#: The manifests a package identity can be read from — the same three
#: `dep-refs` parses, for the same reason (DESIGN §3.5).
MANIFESTS = ("Cargo.toml", "package.json", "pyproject.toml")


@dataclass(frozen=True)
class RegisteredProject:
    """One registered project, flattened for a collector to read.

    Deliberately not a `Project`: what this collector is allowed to know is
    where a project's checkout is and what it is called, not the declared
    entity around it.
    """

    id: str
    slug: str
    root_path: str
    repo_url: str | None = None


class ProjectIndex(Protocol):
    """The registered project list, supplied by the application layer."""

    def registered(self) -> list[RegisteredProject]: ...


@dataclass(frozen=True)
class _Published:
    """A package a registered project's own root manifest declares."""

    project: RegisteredProject
    version: str | None


class MirroredSourceCollector:
    """Path members that are also somebody else's published source (FR-D8)."""

    def __init__(self, projects: ProjectIndex) -> None:
        self._projects = projects
        # Built once and reused for the rest of the sweep: the answer cannot
        # change inside one sweep, and rebuilding it per project would read
        # every registered project's root manifest once per project.
        self._index: dict[str, _Published] | None = None

    @property
    def name(self) -> str:
        return "mirrored-source"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        index = self._published()
        if not index:
            return
        root = Path(project.root_path).expanduser()
        try:
            resolved_root = root.resolve()
        except OSError:  # pragma: no cover - unresolvable root
            return
        exclusions = tuple(project.exclusions)

        for path in walk_project(root, exclusions=exclusions):
            if path.name not in MANIFESTS:
                continue
            try:
                member = path.parent.resolve()
            except OSError:  # pragma: no cover - unresolvable member
                continue
            if member == resolved_root:
                # The project's own root manifest declares the project's own
                # package. A thing is not a mirror of itself.
                continue
            name, version = package_of(path)
            if name is None:
                continue
            published = index.get(name)
            if published is None or published.project.id == project.id:
                continue
            if _same_checkout(published.project, member):
                # The standalone project is checked out *at* this path — a
                # submodule, or one registration nested inside another. One
                # copy of the source, not two, and nothing to report.
                continue
            local_path = member.relative_to(resolved_root).as_posix()
            yield finding(
                kind=KIND_MIRRORED_SOURCE,
                subject_key=(
                    f"mirror:{project.slug}/{local_path}->{published.project.slug}"
                ),
                project=project,
                payload={
                    "package": name,
                    "local_path": local_path,
                    "manifest": path.relative_to(resolved_root).as_posix(),
                    "mirrors_project_id": published.project.id,
                    "mirrors_project_slug": published.project.slug,
                    "mirrors_repo_url": published.project.repo_url,
                    # Both sides' declared versions, as declared. Recorded
                    # because a reader asking "which copy am I looking at"
                    # is owed them; never compared here (FR-D8).
                    "local_version": version,
                    "published_version": published.version,
                    "matched_on": "package name declared by both manifests",
                },
            )

    def _published(self) -> dict[str, _Published]:
        """Package name -> the registered project whose root manifest declares it.

        A name two registered projects both claim is dropped: an ambiguous
        match is not evidence, and picking one would be an assertion about
        which copy is the real one — exactly what FR-D8 refuses to make.
        """
        if self._index is not None:
            return self._index
        index: dict[str, _Published] = {}
        ambiguous: set[str] = set()
        for candidate in self._projects.registered():
            name, version = root_package_of(Path(candidate.root_path).expanduser())
            if name is None:
                continue
            if name in index and index[name].project.id != candidate.id:
                ambiguous.add(name)
                continue
            index[name] = _Published(project=candidate, version=version)
        for name in ambiguous:
            index.pop(name, None)
        self._index = index
        return index


def _same_checkout(project: RegisteredProject, member: Path) -> bool:
    try:
        return Path(project.root_path).expanduser().resolve() == member
    except OSError:  # pragma: no cover - unresolvable root
        return False


def root_package_of(root: Path) -> tuple[str | None, str | None]:
    """The package a project's own root declares, from the first manifest that does."""
    for name in MANIFESTS:
        package, version = package_of(root / name)
        if package is not None:
            return package, version
    return None, None


def package_of(path: Path) -> tuple[str | None, str | None]:
    """The package name and version one manifest declares, if it declares one.

    A manifest that declares no package — a virtual Cargo workspace root, a
    `package.json` that only lists workspaces — has no identity to match on
    and yields nothing. So does one that cannot be read: an unreadable
    manifest is a fact about the project, not a collector failure, and it is
    the same call `dep-refs` makes.
    """
    try:
        if path.name == "package.json":
            parsed = json.loads(path.read_text(encoding="utf-8"))
            if not isinstance(parsed, dict):
                return None, None
            return _string(parsed, "name"), _string(parsed, "version")
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, tomllib.TOMLDecodeError):
        return None, None
    if path.name == "Cargo.toml":
        package = document.get("package")
        if not isinstance(package, dict):
            return None, None
        # `version.workspace = true` inherits from the workspace root. The
        # name is what identifies the package; an inherited version is
        # recorded as unknown rather than as the literal table.
        return _string(package, "name"), _string(package, "version")
    project = document.get("project")
    if isinstance(project, dict) and _string(project, "name"):
        return _string(project, "name"), _string(project, "version")
    poetry = document.get("tool")
    if isinstance(poetry, dict):
        table = poetry.get("poetry")
        if isinstance(table, dict):
            return _string(table, "name"), _string(table, "version")
    return None, None


def _string(document: dict[str, Any], key: str) -> str | None:
    value = document.get(key)
    return value if isinstance(value, str) and value else None


__all__ = [
    "KIND_MIRRORED_SOURCE",
    "MirroredSourceCollector",
    "ProjectIndex",
    "RegisteredProject",
    "package_of",
    "root_package_of",
]
