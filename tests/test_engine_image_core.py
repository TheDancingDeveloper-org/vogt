"""The engine image ships both cores and selects one at start (WI-2660).

Static checks on `engine/Dockerfile` and `engine/deploy/entrypoint.sh`. The
merged image builds `vogt-core` in `server-build` and also installs the Python
core under `/opt/vogt/.venv`. `VOGT_CORE_IMPL` (default `python`) chooses which
one the entrypoint supervises, and the same choice retargets the operator
names `vogt` and `vogt-mcp-remote`. The listen address and health URL are
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


def test_the_engine_image_does_not_lift_a_published_core() -> None:
    code = "\n".join(_code_lines(ENGINE_DOCKERFILE))
    for retired in ("UV_IMAGE", "CORE_IMAGE", "AS core", "AS uv"):
        assert retired not in code, f"engine/Dockerfile still refers to {retired}"
    assert not re.search(r"COPY --from=core\b", code)
    # The Python core is built in this image, not copied from a published one.
    assert "AS python-core" in code
    assert "COPY --from=python-core /opt/vogt /opt/vogt" in code


def test_operator_names_are_not_linked_to_rust_at_build_time() -> None:
    """A build-time link would make `vogt` the rust binary in python mode."""
    code = "\n".join(_code_lines(ENGINE_DOCKERFILE))
    assert "ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt" not in code
    assert "ln -sfn /usr/local/bin/vogt-core /usr/local/bin/vogt-mcp-remote" not in code
    assert "ghcr.io/thedancingdeveloper-org/vogt-base/python:3.13-slim@" in code


def test_the_image_puts_the_link_dir_first_on_path() -> None:
    """Every entry (supervisor, exec shell, vogt-mcp-auth) must see the links.

    /opt/vogt/bin is created for the image user. It has to precede
    /usr/local/bin, or `vogt` and `vogt-mcp-remote` resolve nowhere (or to a
    stale root-owned link) and the container cannot boot.
    """
    text = ENGINE_DOCKERFILE.read_text(encoding="utf-8")
    assert (
        "install -d -o ${SPROOTY_UID} -g ${SPROOTY_GID} -m 0755 /opt/vogt/bin" in text
    )
    path_lines = [line for line in text.splitlines() if line.startswith("ENV PATH=")]
    assert path_lines, "engine/Dockerfile sets no PATH"
    last = path_lines[-1]
    assert last.startswith('ENV PATH="/opt/vogt/bin:')
    # Nothing before /opt/vogt/bin in the final PATH assignment.
    assert last.split(":", 1)[0] == 'ENV PATH="/opt/vogt/bin'


def test_the_entrypoint_selects_the_core_and_retargets_the_names() -> None:
    lines = _code_lines(ENTRYPOINT)
    code = "\n".join(lines)
    # A bare `vogt init` or `vogt serve` command would ignore the selection.
    assert not any(re.search(r"(?<![\w-])vogt (init|serve)\b", line) for line in lines)
    assert '"$bin" init' in code
    assert '"$bin" serve --host "$host" --port "$port"' in code
    assert 'local impl="${VOGT_CORE_IMPL:-python}"' in code
    assert 'cli="${VOGT_PYTHON_BIN:-/opt/vogt/.venv/bin/vogt}"' in code
    assert 'mcp="${VOGT_PYTHON_MCP_BIN:-/opt/vogt/.venv/bin/vogt-mcp-remote}"' in code
    assert 'local dest="${VOGT_BIN_DIR:-/opt/vogt/bin}"' in code
    assert 'local dest="${VOGT_BIN_DIR:-/usr/local/bin}"' not in code
    assert 'ln -sfn "$cli" "$dest/vogt"' in code
    assert 'ln -sfn "$mcp" "$dest/vogt-mcp-remote"' in code
    assert "link_vogt_names" in code


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
