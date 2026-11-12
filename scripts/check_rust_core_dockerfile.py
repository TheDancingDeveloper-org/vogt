"""Check that the Rust core image copies every local path dependency it builds.

`deploy/rust-core.Dockerfile` builds `vogt-core` from a generated workspace.
Cargo resolves a `path = "../contract"` dependency relative to the copied
crate, so every local path dependency of the core build, transitively, must
also be in the build context and survive the Dockerfile's ignore file. A
missing copy is invisible to normal batch PRs because only the scheduled,
dispatched and rust-core-to-main runs build this image.

The check reads the Cargo manifests and the Dockerfile as text. It never
calls Docker, so it is cheap enough for every pull request.
"""

from __future__ import annotations

import argparse
import fnmatch
import re
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_DOCKERFILE = Path("deploy/rust-core.Dockerfile")
WORKSPACE_MANIFEST = Path("engine/Cargo.toml")
CORE_MANIFEST = Path("engine/core/Cargo.toml")

DEPENDENCY_TABLES = ("dependencies", "dev-dependencies", "build-dependencies")


@dataclass(frozen=True)
class Problem:
    """One path dependency the image cannot build."""

    dependency: Path
    reasons: tuple[str, ...]

    def render(self, root: Path) -> str:
        relative = self.dependency.relative_to(root).as_posix()
        detail = "; ".join(self.reasons)
        return f"{relative}: {detail}"


def _manifest(path: Path) -> dict[str, object]:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def _tables(manifest: dict[str, object]) -> list[dict[str, object]]:
    tables: list[dict[str, object]] = []
    for key in DEPENDENCY_TABLES:
        table = manifest.get(key)
        if isinstance(table, dict):
            tables.append(table)
    targets = manifest.get("target")
    if isinstance(targets, dict):
        for target in targets.values():
            if not isinstance(target, dict):
                continue
            for key in DEPENDENCY_TABLES:
                table = target.get(key)
                if isinstance(table, dict):
                    tables.append(table)
    return tables


def local_path_dependencies(workspace: Path) -> set[Path]:
    """Every local path dependency of `vogt-core`, transitively."""
    core = workspace / CORE_MANIFEST
    pending = [core.parent.resolve()]
    seen: set[Path] = set()
    found: set[Path] = set()
    while pending:
        crate = pending.pop()
        if crate in seen:
            continue
        seen.add(crate)
        manifest_path = crate / "Cargo.toml"
        if not manifest_path.is_file():
            continue
        for table in _tables(_manifest(manifest_path)):
            for spec in table.values():
                if not isinstance(spec, dict):
                    continue
                raw = spec.get("path")
                if not isinstance(raw, str):
                    continue
                dependency = (crate / raw).resolve()
                found.add(dependency)
                pending.append(dependency)
    return found


def _instructions(text: str) -> list[str]:
    """Dockerfile instructions with continuations joined and comments removed."""
    out: list[str] = []
    current = ""
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        current = f"{current} {line}".strip() if current else line
        if current.endswith("\\"):
            current = current[:-1].rstrip()
            continue
        out.append(current)
        current = ""
    if current:
        out.append(current)
    return out


def _tokens(instruction: str) -> list[str]:
    """Words of a Dockerfile instruction, with a JSON source list parsed."""
    command, _, rest = instruction.partition(" ")
    rest = rest.strip()
    if not rest:
        return [command]
    if rest.startswith("["):
        import json

        try:
            values = json.loads(rest)
        except ValueError:
            values = None
        if isinstance(values, list) and all(isinstance(value, str) for value in values):
            return [command, *values]
    return [command, *rest.split()]


def copy_sources(dockerfile: Path) -> list[str]:
    """Sources of COPY/ADD instructions that read the build context."""
    sources: list[str] = []
    for instruction in _instructions(dockerfile.read_text(encoding="utf-8")):
        words = _tokens(instruction)
        command = words[0].upper()
        if command not in ("COPY", "ADD"):
            continue
        arguments = words[1:]
        if any(argument.startswith("--from=") for argument in arguments):
            continue
        arguments = [
            argument for argument in arguments if not argument.startswith("--")
        ]
        if len(arguments) < 2:
            continue
        sources.extend(arguments[:-1])
    return sources


def _normalise_source(source: str) -> str:
    source = source.strip().strip("\"'")
    while source.startswith("./"):
        source = source[2:]
    return source.rstrip("/")


def _covered(source: str, dependency: str) -> bool:
    source = _normalise_source(source)
    if source in ("", "."):
        return True
    if dependency == source or dependency.startswith(f"{source}/"):
        return True
    return fnmatch.fnmatchcase(dependency, source)


def _glob_regex(pattern: str) -> re.Pattern[str]:
    out: list[str] = ["^"]
    index = 0
    while index < len(pattern):
        char = pattern[index]
        if pattern.startswith("**/", index):
            out.append("(?:.*/)?")
            index += 3
        elif pattern.startswith("**", index):
            out.append(".*")
            index += 2
        elif char == "*":
            out.append("[^/]*")
            index += 1
        elif char == "?":
            out.append("[^/]")
            index += 1
        elif char == "[":
            end = pattern.find("]", index + 1)
            if end == -1:
                out.append(re.escape(char))
                index += 1
            else:
                out.append(pattern[index : end + 1])
                index = end + 1
        else:
            out.append(re.escape(char))
            index += 1
    out.append("$")
    return re.compile("".join(out))


def dockerignore_patterns(path: Path) -> list[tuple[bool, re.Pattern[str]]]:
    """`(is_exception, compiled pattern)` for one `.dockerignore` file."""
    patterns: list[tuple[bool, re.Pattern[str]]] = []
    if not path.is_file():
        return patterns
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        exception = line.startswith("!")
        if exception:
            line = line[1:].strip()
        pattern = line.lstrip("/").rstrip("/")
        if not pattern:
            continue
        if "/" not in pattern:
            pattern = f"**/{pattern}"
        patterns.append((exception, _glob_regex(pattern)))
    return patterns


def ignored(patterns: list[tuple[bool, re.Pattern[str]]], relative: str) -> bool:
    """Whether the last matching ignore pattern excludes `relative`."""
    excluded = False
    for exception, pattern in patterns:
        if pattern.match(relative):
            excluded = not exception
    return excluded


def _ignore_file(dockerfile: Path, context: Path) -> Path:
    specific = dockerfile.with_name(f"{dockerfile.name}.dockerignore")
    if specific.is_file():
        return specific
    return context / ".dockerignore"


def _required_paths(dependency: Path) -> list[Path]:
    paths = [dependency]
    manifest = dependency / "Cargo.toml"
    if manifest.is_file():
        paths.append(manifest)
    source = dependency / "src"
    if source.is_dir():
        paths.extend(path for path in source.rglob("*") if path.is_file())
    return paths


def check(workspace: Path = ROOT, dockerfile: Path | None = None) -> list[Problem]:
    workspace = workspace.resolve()
    dockerfile = dockerfile or workspace / DEFAULT_DOCKERFILE
    if not dockerfile.is_absolute():
        dockerfile = workspace / dockerfile
    sources = copy_sources(dockerfile)
    patterns = dockerignore_patterns(_ignore_file(dockerfile, workspace))
    problems: list[Problem] = []
    for dependency in sorted(local_path_dependencies(workspace)):
        try:
            relative = dependency.relative_to(workspace).as_posix()
        except ValueError:
            problems.append(Problem(dependency, ("it is outside the build context",)))
            continue
        reasons: list[str] = []
        if not any(_covered(source, relative) for source in sources):
            reasons.append("no COPY instruction copies it into the build context")
        excluded = [
            path.relative_to(workspace).as_posix()
            for path in _required_paths(dependency)
            if ignored(patterns, path.relative_to(workspace).as_posix())
        ]
        if excluded:
            reasons.append(
                f"its ignore file excludes {excluded[0]}"
                + (f" and {len(excluded) - 1} more" if len(excluded) > 1 else "")
            )
        if reasons:
            problems.append(Problem(dependency, tuple(reasons)))
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--dockerfile",
        default=str(DEFAULT_DOCKERFILE),
        help="Dockerfile to check, relative to the repository root",
    )
    arguments = parser.parse_args(argv)
    dockerfile = Path(arguments.dockerfile)
    problems = check(
        ROOT, dockerfile if dockerfile.is_absolute() else ROOT / dockerfile
    )
    for problem in problems:
        print(problem.render(ROOT), file=sys.stderr)
    if problems:
        print(
            f"{dockerfile}: {len(problems)} path dependenc"
            + ("y is" if len(problems) == 1 else "ies are")
            + " missing from the image",
            file=sys.stderr,
        )
        return 1
    print(f"{dockerfile}: every local path dependency is copied")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
