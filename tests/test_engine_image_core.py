"""The engine image runs the Rust core it builds itself (WI-1078, I3).

Static checks on `engine/Dockerfile` and `engine/deploy/entrypoint.sh`. The
merged image no longer lifts a Python core from a published image or runs a uv
venv; `vogt-core` is built in `server-build` and the entrypoint starts it where
it used to start the Python `vogt`. The listen address and health URL are
unchanged, which is why the compose files do not change.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
ENGINE_DOCKERFILE = REPO_ROOT / "engine" / "Dockerfile"
ENTRYPOINT = REPO_ROOT / "engine" / "deploy" / "entrypoint.sh"

# The core-alone CI job deletes engine/, and these checks read the engine image.
pytestmark = pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(), reason="the engine tree is not in this checkout"
)


def _code_lines(path: Path) -> list[str]:
    """Lines that are not whole-line comments, so prose cannot satisfy a check."""
    return [
        line
        for line in path.read_text(encoding="utf-8").splitlines()
        if not line.lstrip().startswith("#")
    ]


def test_server_build_compiles_vogt_core_and_the_runtime_copies_it() -> None:
    text = ENGINE_DOCKERFILE.read_text(encoding="utf-8")
    assert "-p vogt-engine-server -p vogt-core" in text
    assert (
        "COPY --from=server-build /usr/local/bin/vogt-core /usr/local/bin/vogt-core"
        in text
    )


def test_the_engine_image_has_no_python_core_stage() -> None:
    code = "\n".join(_code_lines(ENGINE_DOCKERFILE))
    for retired in ("UV_IMAGE", "CORE_IMAGE", "AS core", "AS uv", "/opt/vogt/.venv"):
        assert retired not in code, f"engine/Dockerfile still refers to {retired}"
    assert not re.search(r"COPY --from=core\b", code)


def test_vogt_names_resolve_to_the_rust_binary() -> None:
    text = ENGINE_DOCKERFILE.read_text(encoding="utf-8")
    assert "ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt;" in text
    assert "ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt-mcp-remote;" in text


def test_the_entrypoint_runs_vogt_core_not_the_python_cli() -> None:
    lines = _code_lines(ENTRYPOINT)
    code = "\n".join(lines)
    # A bare `vogt init` or `vogt serve` command would be the Python CLI; the
    # `vogt-core` spelling has a hyphen, which the lookbehind rules out.
    assert not any(re.search(r"(?<![\w-])vogt (init|serve)\b", line) for line in lines)
    assert "if ! vogt-core init; then" in code
    assert 'vogt-core serve --host "$host" --port "$port"' in code


def test_no_workflow_passes_core_image_to_the_engine_dockerfile() -> None:
    # engine/Dockerfile declares no CORE_IMAGE, so a workflow passing one is a
    # dead build-arg that suggests the engine still lifts a core from an image.
    workflows = REPO_ROOT / ".github" / "workflows"
    offenders = [
        f"{path.name}:{number}"
        for path in sorted(workflows.glob("*.yml"))
        for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1)
        if "engine/Dockerfile" in line and "CORE_IMAGE" in line
    ]
    assert not offenders, f"CORE_IMAGE passed to engine/Dockerfile at {offenders}"
