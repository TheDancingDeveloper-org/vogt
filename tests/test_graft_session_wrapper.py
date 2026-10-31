"""The `g` graft wrapper every engine-launched session has on PATH (WI-2755).

A Codex session runs each command in a fresh shell, so a `g` shell function
never reached it. The wrapper is an executable that resolves the session's
endpoint and token on every call; these tests run it against a fake graft.
"""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
WRAPPER = ROOT / "engine" / "deploy" / "graft-session.sh"
DOCKERFILE = ROOT / "engine" / "Dockerfile"

FAKE_GRAFT = """#!/usr/bin/env bash
printf 'endpoint=%s\\n' "${GRAFT_ENDPOINT:-}"
printf 'arg=%s\\n' "$@"
"""


@pytest.fixture
def home(tmp_path: Path) -> Path:
    graft = tmp_path / ".local" / "bin" / "graft"
    graft.parent.mkdir(parents=True)
    graft.write_text(FAKE_GRAFT)
    graft.chmod(0o755)
    return tmp_path


def run(home: Path, *args: str, **env: str) -> subprocess.CompletedProcess[str]:
    base = {"HOME": str(home), "PATH": os.environ["PATH"]}
    return subprocess.run(
        ["bash", str(WRAPPER), *args],
        env={**base, **env},
        capture_output=True,
        text=True,
        check=False,
    )


def token(home: Path, session: str, name: str = "trial-1.token") -> Path:
    path = home / ".config" / "graft" / "sessions" / session / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("secret-value\n")
    return path


def test_session_token_and_endpoint_reach_graft(home: Path) -> None:
    path = token(home, "ses_A")
    out = run(home, "land", VOGT_SESSION_ID="ses_A", VOGT_GRAFT_ENDPOINT="http://g:1")
    assert out.returncode == 0, out.stderr
    assert out.stdout.splitlines() == [
        "endpoint=http://g:1",
        "arg=--token-file",
        f"arg={path}",
        "arg=land",
    ]
    assert "secret-value" not in out.stdout + out.stderr


def test_explicit_endpoint_wins_over_deployment_setting(home: Path) -> None:
    token(home, "ses_A")
    out = run(
        home,
        "sync",
        VOGT_SESSION_ID="ses_A",
        GRAFT_ENDPOINT="http://mine:2",
        VOGT_GRAFT_ENDPOINT="http://g:1",
    )
    assert "endpoint=http://mine:2" in out.stdout


def test_engine_session_id_is_the_fallback_and_a_single_token_is_used(
    home: Path,
) -> None:
    path = token(home, "uuid-1", "other.token")
    out = run(
        home, "clone", VOGT_SESSION_ID="ses_none", VOGT_ENGINE_SESSION_ID="uuid-1"
    )
    assert out.returncode == 0, out.stderr
    assert f"arg={path}" in out.stdout


def test_several_tokens_without_trial_1_is_refused(home: Path) -> None:
    token(home, "ses_A", "a.token")
    token(home, "ses_A", "b.token")
    out = run(home, "land", VOGT_SESSION_ID="ses_A")
    assert out.returncode == 64
    assert "GRAFT_TOKEN_FILE" in out.stderr


@pytest.mark.parametrize(
    ("args", "env"),
    [
        (("--token-file", "/x", "land"), {}),
        (("land",), {"GRAFT_TOKEN": "t"}),
    ],
)
def test_caller_supplied_token_is_left_alone(
    home: Path, args: tuple[str, ...], env: dict[str, str]
) -> None:
    token(home, "ses_A")
    out = run(home, *args, VOGT_SESSION_ID="ses_A", **env)
    assert out.stdout.count("arg=--token-file") == (1 if "--token-file" in args else 0)


def test_no_session_token_runs_graft_unchanged(home: Path) -> None:
    out = run(home, "version")
    assert out.returncode == 0, out.stderr
    assert out.stdout.splitlines()[1:] == ["arg=version"]


def test_image_installs_wrapper_as_g_on_path() -> None:
    assert (
        "COPY --chmod=755 engine/deploy/graft-session.sh /usr/local/bin/g"
        in DOCKERFILE.read_text()
    )
