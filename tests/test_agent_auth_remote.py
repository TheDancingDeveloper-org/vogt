"""Remote broker transport for the shipped agent-auth helper."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "engine" / "deploy" / "agent-auth.sh"


def test_remote_broker_uses_token_file_and_extra_ca_without_disabling_tls(
    tmp_path: Path,
) -> None:
    token = "mounted-broker-token"
    token_file = tmp_path / "broker-token"
    token_file.write_text(token, encoding="utf-8")
    ca_file = tmp_path / "pod-ca.pem"
    ca_file.write_text("test-ca", encoding="utf-8")
    capture = tmp_path / "curl.json"
    fake_curl = tmp_path / "curl"
    fake_curl.write_text(
        """#!/usr/bin/env python3
import json, os, sys

args = sys.argv[1:]
header = ""
for index, value in enumerate(args):
    if value == "-H" and index + 1 < len(args) and args[index + 1].startswith("@"):
        with open(args[index + 1][1:], encoding="utf-8") as source:
            header = source.read()
with open(os.environ["CAPTURE"], "w", encoding="utf-8") as target:
    json.dump({"args": args, "header": header}, target)
""",
        encoding="utf-8",
    )
    fake_curl.chmod(0o755)

    command = f"source {SCRIPT}; broker_curl -sS https://192.168.1.75:18197/health"
    proc = subprocess.run(
        ["bash", "-c", command],
        env={
            "PATH": f"{tmp_path}:/usr/bin:/bin",
            "HOME": str(tmp_path),
            "CAPTURE": str(capture),
            "VOGT_ENGINE_BROKER_TOKEN_FILE": str(token_file),
            "VOGT_ENGINE_EXTRA_CA_FILE": str(ca_file),
        },
        capture_output=True,
        text=True,
        check=False,
    )

    assert proc.returncode == 0, proc.stderr
    call = json.loads(capture.read_text(encoding="utf-8"))
    assert call["args"][-1] == "https://192.168.1.75:18197/health"
    assert call["args"][:2] == ["--cacert", str(ca_file)]
    assert call["header"] == f"Authorization: Bearer {token}\n"
    assert token not in "\0".join(call["args"])
    assert "--insecure" not in call["args"] and "-k" not in call["args"]
    assert token not in proc.stdout and token not in proc.stderr
