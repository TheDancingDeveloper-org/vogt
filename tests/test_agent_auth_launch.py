"""The launch half of `engine/deploy/agent-auth.sh` (WI-927).

A session launch used to read each manifest secret with its own `infisical`
CLI run, and each run waited ~650 ms after the API answered on the vendor's
telemetry: fifteen in series made a launch take 10 s, and 85–145 s when that
egress was slow. The launch now reads each project once over the API. These
tests pin what that read must keep doing: the same values the CLI returned
(imports merged, the project's own keys winning, hidden values not trusted),
the token never on a command line, and the per-secret CLI as the fallback.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from typing import Any, ClassVar
from urllib.parse import parse_qs, urlparse

import pytest

SCRIPT = Path(__file__).resolve().parents[1] / "engine" / "deploy" / "agent-auth.sh"

pytestmark = pytest.mark.skipif(
    shutil.which("jq") is None or shutil.which("curl") is None,
    reason="the bulk read uses curl and jq",
)


def _secret(key: str, value: str, *, hidden: bool = False) -> dict[str, Any]:
    return {"secretKey": key, "secretValue": value, "secretValueHidden": hidden}


PROJECTS: dict[str, dict[str, Any]] = {
    "proj-a": {
        "secrets": [
            _secret("ALPHA", "alpha-value"),
            _secret("SHARED", "own-wins"),
            _secret("MULTI", "line one\nline two"),
            _secret("HIDDEN", "<hidden-by-infisical>", hidden=True),
        ],
        "imports": [
            {
                "secretPath": "/common",
                "secrets": [
                    _secret("SHARED", "import-loses"),
                    _secret("FROM_IMPORT", "imported"),
                ],
            }
        ],
    },
}


class _Infisical(BaseHTTPRequestHandler):
    seen: ClassVar[list[dict[str, Any]]] = []

    def do_GET(self) -> None:
        url = urlparse(self.path)
        query = {k: v[0] for k, v in parse_qs(url.query).items()}
        _Infisical.seen.append(
            {"path": url.path, "query": query, "auth": self.headers["Authorization"]}
        )
        project = PROJECTS.get(query.get("projectId", ""))
        if url.path != "/api/v4/secrets" or project is None:
            self.send_response(404)
            self.end_headers()
            return
        body = json.dumps(project).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        del args


@pytest.fixture
def infisical() -> Any:
    _Infisical.seen = []
    server = HTTPServer(("127.0.0.1", 0), _Infisical)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{server.server_port}"
    server.shutdown()


def _resolve(api_url: str, lookups: str, tmp_path: Path) -> str:
    """Source the helper and run `resolve_secret` calls; return stdout."""
    cli = tmp_path / "bin" / "infisical"
    cli.parent.mkdir(exist_ok=True)
    # The per-secret fallback: records its argv, prints a marker value.
    cli.write_text(
        f'#!/bin/sh\necho "$*" >> {tmp_path}/cli-calls\nprintf "cli-%s" "$3"\n',
        encoding="utf-8",
    )
    cli.chmod(0o755)
    script = f"""
        source {SCRIPT}
        INFISICAL_API_URL={api_url}/api
        {lookups}
    """
    proc = subprocess.run(
        ["bash", "-c", script],
        env={"PATH": f"{cli.parent}:/usr/bin:/bin", "HOME": str(tmp_path)},
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    return proc.stdout


def test_one_request_per_project_with_the_clis_values(
    infisical: str, tmp_path: Path
) -> None:
    out = _resolve(
        infisical,
        """
        for n in ALPHA SHARED MULTI FROM_IMPORT HIDDEN ABSENT; do
            resolve_secret v tok-123 proj-a "$n" "VAR_$n"
            printf '%s=[%s]\\n' "$n" "$v"
        done
        printf 'mode=%s names=%s\\n' \\
            "${PROJECT_MODE[proj-a]}" "${PROJECT_NAMES[proj-a]}"
        """,
        tmp_path,
    )
    assert "ALPHA=[alpha-value]" in out
    assert "SHARED=[own-wins]" in out, "the project's own key wins over an import"
    assert "MULTI=[line one\nline two]" in out
    assert "FROM_IMPORT=[imported]" in out
    assert "HIDDEN=[]" in out, "a hidden value is never handed out as the value"
    assert "ABSENT=[]" in out
    assert "mode=bulk" in out
    assert "VAR_ALPHA:ALPHA:1" in out and "VAR_ABSENT:ABSENT:0" in out
    # One request for six lookups, the token in a header only.
    assert len(_Infisical.seen) == 1
    request = _Infisical.seen[0]
    assert request["auth"] == "Bearer tok-123"
    assert request["query"]["projectId"] == "proj-a"
    assert request["query"]["expandSecretReferences"] == "true"
    assert request["query"]["includeImports"] == "true"
    assert not (tmp_path / "cli-calls").exists(), "no CLI run when the API answers"


def test_a_project_the_api_cannot_read_falls_back_to_the_cli(
    infisical: str, tmp_path: Path
) -> None:
    out = _resolve(
        infisical,
        """
        resolve_secret v tok-123 proj-unknown ONE VAR_ONE
        printf 'ONE=[%s] mode=%s\\n' "$v" "${PROJECT_MODE[proj-unknown]}"
        """,
        tmp_path,
    )
    assert "ONE=[cli-ONE] mode=cli" in out
    assert "secrets get ONE" in (tmp_path / "cli-calls").read_text(encoding="utf-8")
