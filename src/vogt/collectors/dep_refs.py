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

MANIFESTS = ("Cargo.toml", "package.json", "pyproject.toml")

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

        for path in walk_project(root, exclusions=exclusions):
            if path.name not in MANIFESTS:
                continue
            relative = path.relative_to(root).as_posix()
            for ref_kind, target in _references(path):
                yield finding(
                    kind=KIND_DEP_REF,
                    subject_key=f"depref:{project.slug}/{relative}->{target}",
                    project=project,
                    payload={
                        "ref_kind": ref_kind,
                        "raw_target": target,
                        "manifest": relative,
                    },
                )


def _references(path: Path) -> list[tuple[str, str]]:
    """Extract internal-looking references from one manifest."""
    try:
        if path.name == "package.json":
            return _from_package_json(json.loads(path.read_text(encoding="utf-8")))
        with path.open("rb") as handle:
            document = tomllib.load(handle)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, tomllib.TOMLDecodeError):
        # A malformed manifest is a fact about the project, not a collector
        # failure. It yields no references and no error.
        return []
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
        found.append(("path", "workspace:."))
    return found


def _nested(document: dict[str, Any], *keys: str) -> Any:
    current: Any = document
    for key in keys:
        if not isinstance(current, dict):
            return None
        current = current.get(key)
    return current
