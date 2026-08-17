"""`dep-refs` — which projects reference which, and nothing more.

r2 deleted the expensive half of dependency tooling deliberately (DESIGN
§3.5). This reads **manifests only** — `Cargo.toml`, `package.json`,
`pyproject.toml` — and extracts only **internal-looking references**: `path`,
`git`, `file:`, `link:`, `workspace:` and direct git URLs. Registry
dependencies (`serde`, `react`) are ignored entirely: Vogt has no reason to
hold an opinion about them, and parsing lockfiles would buy transitive
resolution, feature conditionals and per-ecosystem resolver semantics that
nothing in the product needs.

The trade, stated plainly: the graph tells you where the risk lives, and a
`git diff` tells you whether it has bitten.

**A reference is also classified by where it lands.** A Cargo workspace is
made of path dependencies between its own crates, and reporting each of them
as "a project nobody has registered" turned rustnzb's first sweep into thirty
proposals about a layout its owner had chosen on purpose. So every path
reference is resolved against the manifest that declares it and given a
`scope`:

- `internal` — resolves inside the project's own `root_path` and exists. A
  workspace member. Not drift, and never was.
- `broken` — resolves inside `root_path` and there is nothing there. A real
  finding, and a different one: the manifest is wrong, not the registry.
- `external` — resolves outside `root_path`, or is a git URL. The only kind
  that can mean "a project nobody has registered yet".

Containment is the test rather than workspace membership, because membership
gets it wrong: rustnzb's root manifest excludes `benchnzb` and `desktop`, and
`fuzz/` is not a member either, so those three are separate workspaces nested
in the same repository reaching back into the main one by relative path. Five
of the thirty would still have been flagged.

**Every project also gets one scan record**, whatever was found. Three
different zeros reach `deps` otherwise and none of them is distinguishable
from the others: a project that references nothing, a Go or Maven project
whose manifests this collector does not parse, and a project no sweep has
covered. The record names the manifests read, the ones that would not parse,
and the ones present in a format this does not read — so a zero says which
zero it is (FR-O4, and the audit #50 asked for after WI-9 fixed the same
shape for `forge onboard`).
"""

from __future__ import annotations

import json
import re
import tomllib
from collections.abc import Iterable
from pathlib import Path
from typing import Any

from vogt.collectors.base import CollectorContext, Finding, finding, walk_project
from vogt.core.entities import Project

KIND_DEP_REF = "dep_ref"

#: One record per project saying what the walk actually read (FR-O4).
#: Without it, three different zeros are indistinguishable at `deps`: a
#: project that genuinely references nothing, a project whose manifests are
#: in a format this collector does not parse, and a project nothing has
#: swept. `forge onboard` learned to tell its zeros apart in WI-9; this is
#: the same fix on the surface the same audit named next (#50).
KIND_DEP_SCAN = "dep_scan"

MANIFESTS = ("Cargo.toml", "package.json", "pyproject.toml")

#: Manifests this collector recognises and does not read. Named rather than
#: ignored: a Go or Maven project reporting no dependency references is
#: reporting the collector's reach, not the project's graph, and a reader
#: cannot tell the two apart from a bare zero. Extending `MANIFESTS` is how
#: one of these stops being listed here.
UNSUPPORTED_MANIFESTS = (
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
    "mix.exs",
    "requirements.txt",
    "Package.swift",
    "pubspec.yaml",
    "CMakeLists.txt",
)

#: The same point by suffix, for ecosystems that name the file after the
#: project rather than after the tool.
UNSUPPORTED_SUFFIXES = (".csproj", ".fsproj", ".vbproj")

#: Prefixes that mean "this is not a published package, it is that thing
#: over there". Everything else is a registry dependency and ignored.
INTERNAL_PREFIXES = ("file:", "link:", "workspace:", "portal:")

_GIT_URL = re.compile(r"^(https?://|git\+|git@|ssh://)")


class DepRefCollector:
    """Path, git and workspace references between projects."""

    @property
    def name(self) -> str:
        return "dep-refs"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        root = Path(project.root_path).expanduser()
        exclusions = tuple(project.exclusions)
        read: list[str] = []
        unreadable: list[str] = []
        unsupported: list[str] = []
        references = 0

        for path in walk_project(root, exclusions=exclusions):
            relative = path.relative_to(root).as_posix()
            if path.name not in MANIFESTS:
                if path.name in UNSUPPORTED_MANIFESTS or (
                    path.suffix in UNSUPPORTED_SUFFIXES
                ):
                    unsupported.append(relative)
                continue
            found = _references(path)
            if found is None:
                unreadable.append(relative)
                continue
            read.append(relative)
            for ref_kind, target in found:
                references += 1
                yield finding(
                    kind=KIND_DEP_REF,
                    subject_key=f"depref:{project.slug}/{relative}->{target}",
                    project=project,
                    payload={
                        "ref_kind": ref_kind,
                        "raw_target": target,
                        "manifest": relative,
                        "scope": _scope(ref_kind, target, manifest=path, root=root),
                    },
                )

        yield finding(
            kind=KIND_DEP_SCAN,
            subject_key=f"depscan:{project.slug}",
            project=project,
            payload={
                "manifests_read": len(read),
                "references": references,
                "unreadable_manifests": sorted(unreadable),
                "unsupported_manifests": sorted(unsupported),
                "root_exists": root.is_dir(),
            },
        )


#: How a reference relates to the project that declares it.
SCOPE_INTERNAL = "internal"
SCOPE_EXTERNAL = "external"
SCOPE_BROKEN = "broken"


def _scope(ref_kind: str, target: str, *, manifest: Path, root: Path) -> str:
    """Where a reference lands, relative to the project that declares it."""
    if ref_kind != "path":
        # A git URL names something by address, not by location; an inherited
        # dependency (`foo.workspace = true`) names the workspace root, which
        # is this project by construction.
        return SCOPE_EXTERNAL if ref_kind == "git" else SCOPE_INTERNAL
    if target.startswith(INTERNAL_PREFIXES):
        # `workspace:*`, `file:../thing`, `link:./thing` — the prefix is a
        # protocol, not a path segment, and what follows it may be a glob.
        _, _, remainder = target.partition(":")
        target = remainder or "."
    try:
        resolved = (manifest.parent / target).resolve()
        inside = resolved.is_relative_to(root.resolve())
    except (OSError, ValueError):  # pragma: no cover - unresolvable path
        return SCOPE_EXTERNAL
    if not inside:
        return SCOPE_EXTERNAL
    # A glob member (`crates/*`) does not exist as a path and is not broken.
    if "*" in target or "?" in target:
        return SCOPE_INTERNAL
    return SCOPE_INTERNAL if resolved.exists() else SCOPE_BROKEN


def _references(path: Path) -> list[tuple[str, str]] | None:
    """Extract internal-looking references from one manifest.

    `None` means the manifest could not be read, which is a different answer
    from "it declares no internal references" and is reported as such in the
    scan record. A malformed manifest is still a fact about the project
    rather than a collector failure: the sweep stays `ok` and the file is
    named.
    """
    try:
        if path.name == "package.json":
            return _from_package_json(json.loads(path.read_text(encoding="utf-8")))
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, tomllib.TOMLDecodeError):
        return None
    if path.name == "Cargo.toml":
        return _from_cargo(document)
    return _from_pyproject(document)


def _from_cargo(document: dict[str, Any]) -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    tables = [
        document.get("dependencies"),
        document.get("dev-dependencies"),
        document.get("build-dependencies"),
        _nested(document, "workspace", "dependencies"),
        _nested(document, "patch", "crates-io"),
    ]
    for target in document.get("workspace", {}).get("members", []) or []:
        if isinstance(target, str):
            found.append(("path", target))
    for table in tables:
        if not isinstance(table, dict):
            continue
        for spec in table.values():
            found.extend(_from_spec(spec))
    return found


def _from_pyproject(document: dict[str, Any]) -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    sources = _nested(document, "tool", "uv", "sources")
    if isinstance(sources, dict):
        for spec in sources.values():
            found.extend(_from_spec(spec))
    poetry = _nested(document, "tool", "poetry", "dependencies")
    if isinstance(poetry, dict):
        for spec in poetry.values():
            found.extend(_from_spec(spec))
    for requirement in document.get("project", {}).get("dependencies", []) or []:
        if isinstance(requirement, str) and "@" in requirement:
            _, _, target = requirement.partition("@")
            target = target.strip()
            if _GIT_URL.match(target):
                found.append(("git", target))
            elif target.startswith(INTERNAL_PREFIXES):
                found.append(("path", target))
    return found


def _from_package_json(document: dict[str, Any]) -> list[tuple[str, str]]:
    found: list[tuple[str, str]] = []
    for key in ("dependencies", "devDependencies", "peerDependencies"):
        table = document.get(key)
        if not isinstance(table, dict):
            continue
        for spec in table.values():
            if not isinstance(spec, str):
                continue
            if spec.startswith(INTERNAL_PREFIXES):
                found.append(("path", spec))
            elif _GIT_URL.match(spec) or spec.startswith("github:"):
                found.append(("git", spec))
    for workspace in document.get("workspaces", []) or []:
        if isinstance(workspace, str):
            found.append(("path", workspace))
    return found


def _from_spec(spec: object) -> list[tuple[str, str]]:
    """A dependency spec is internal only if it names a path or a git URL."""
    if isinstance(spec, str):
        if spec.startswith(INTERNAL_PREFIXES):
            return [("path", spec)]
        if _GIT_URL.match(spec):
            return [("git", spec)]
        return []
    if not isinstance(spec, dict):
        return []
    found: list[tuple[str, str]] = []
    path = spec.get("path")
    if isinstance(path, str):
        found.append(("path", path))
    git = spec.get("git")
    if isinstance(git, str):
        found.append(("git", git))
    workspace = spec.get("workspace")
    if workspace is True:
        # Cargo dependency *inheritance* from the root `[workspace.dependencies]`
        # table — `opentelemetry.workspace = true`. It is not a path and must
        # never reach path resolution; rustnzb produced three of these and all
        # three were reported as unregistered projects called `workspace:.`.
        found.append(("inherited", "workspace:."))
    return found


def _nested(document: dict[str, Any], *keys: str) -> Any:
    current: Any = document
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current
