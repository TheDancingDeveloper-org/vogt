"""The `vogt-mcp` launcher's token sources (WI-4310).

A `claude` started without the session's token used to fall through to the
secrets-manager broker, which a brokered deployment cannot use, and failed on
an unset INFISICAL_API_URL. The launcher now asks the engine's secret broker
first and, with nothing left to try, says what is missing.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
WRAPPER = REPO_ROOT / "engine" / "deploy" / "vogt-mcp-auth.sh"
TOKEN = "fetched-token-value"

pytestmark = pytest.mark.skipif(
    not WRAPPER.is_file(),
    reason="needs engine/deploy (absent in the core-alone job)",
)

RECORD_SHIM = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SHIM_OUT"], "w") as fh:
    json.dump({"argv": sys.argv, "env": dict(os.environ)}, fh)
"""

# Records every call; answers `fetch VOGT_HTTP_TOKEN` with BROKER_VALUE (or
# fails when it is empty) and `run -- cmd...` by running cmd.
AGENT_AUTH_SHIM = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["BROKER_OUT"], "a") as fh:
    fh.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1:] == ["fetch", "VOGT_HTTP_TOKEN"]:
    value = os.environ.get("BROKER_VALUE", "")
    if not value:
        sys.stderr.write("refused (HTTP 403)\\n")
        sys.exit(1)
    sys.stdout.write(value + "\\n")
    sys.exit(0)
if sys.argv[1:3] == ["run", "--"]:
    os.execvp(sys.argv[3], sys.argv[3:])
sys.exit(2)
"""


def run(tmp_path: Path, env: dict[str, str]) -> tuple[Any, Any, list[Any]]:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    shims = (("vogt-mcp-remote", RECORD_SHIM), ("vogt-agent-auth", AGENT_AUTH_SHIM))
    for name, body in shims:
        path = bindir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
    out, calls = tmp_path / "bridge.json", tmp_path / "calls.jsonl"
    proc = subprocess.run(
        ["bash", str(WRAPPER)],
        env={
            "PATH": f"{bindir}:/usr/bin:/bin",
            "HOME": str(tmp_path),
            "SHIM_OUT": str(out),
            "BROKER_OUT": str(calls),
            "VOGT_AGENT_AUTH": str(bindir / "vogt-agent-auth"),
            **env,
        },
        capture_output=True,
        text=True,
        check=False,
    )
    bridge = json.loads(out.read_text()) if out.exists() else None
    lines = calls.read_text().splitlines() if calls.exists() else []
    seen = [json.loads(line) for line in lines]
    return proc, bridge, seen


BROKER = {
    "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:8910",
    "VOGT_ENGINE_BROKER_TOKEN": "grant",
}


def test_session_token_is_used_without_the_broker(tmp_path: Path) -> None:
    proc, bridge, seen = run(
        tmp_path, {**BROKER, "VOGT_SESSION_ID": "ses_x", "VOGT_HTTP_TOKEN": "own"}
    )
    assert proc.returncode == 0, proc.stderr
    assert bridge["env"]["VOGT_HTTP_TOKEN"] == "own"
    assert seen == []


def test_no_token_fetches_from_the_engine_broker(tmp_path: Path) -> None:
    proc, bridge, seen = run(tmp_path, {**BROKER, "BROKER_VALUE": TOKEN})
    assert proc.returncode == 0, proc.stderr
    assert seen == [["fetch", "VOGT_HTTP_TOKEN"]]
    assert bridge["env"]["VOGT_HTTP_TOKEN"] == TOKEN
    assert bridge["env"]["VOGT_URL"] == "http://127.0.0.1:8910"
    assert TOKEN not in json.dumps(bridge["argv"])
    assert TOKEN not in proc.stderr and proc.stdout == ""


def test_broker_refusal_without_identity_fails_clearly(tmp_path: Path) -> None:
    proc, bridge, seen = run(tmp_path, dict(BROKER))
    assert proc.returncode == 64
    assert bridge is None
    assert seen == [["fetch", "VOGT_HTTP_TOKEN"]]
    assert "no Vogt token" in proc.stderr


def test_no_broker_and_no_identity_fails_clearly(tmp_path: Path) -> None:
    proc, bridge, seen = run(tmp_path, {})
    assert proc.returncode == 64
    assert bridge is None and seen == []
    assert "no Vogt token" in proc.stderr


def test_identity_still_falls_back_to_agent_auth_run(tmp_path: Path) -> None:
    proc, bridge, seen = run(
        tmp_path,
        {
            "INFISICAL_API_URL": "https://infisical.example",
            "INFISICAL_CLIENT_ID": "id",
            "INFISICAL_CLIENT_SECRET": "s",
        },
    )
    assert proc.returncode == 0, proc.stderr
    assert seen and seen[0][:2] == ["run", "--"]
    assert bridge is not None


def test_never_falls_through_to_prod_when_the_dev_token_is_missing(
    tmp_path: Path,
) -> None:
    proc, bridge, seen = run(
        tmp_path,
        {**BROKER, "VOGT_PROD_TOKEN": "prod-secret", "VOGT_URL": "http://dev:8910"},
    )
    assert proc.returncode == 64
    assert bridge is None  # the bridge was never launched, for any endpoint
    assert seen == [["fetch", "VOGT_HTTP_TOKEN"]]  # only the broker was asked
    assert "VOGT_TOKEN_FILE" in proc.stderr and "session's own shell" in proc.stderr
    assert "prod-secret" not in proc.stderr
