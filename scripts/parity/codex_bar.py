#!/usr/bin/env python3
"""Collect engine-visible evidence for the WI-1150 Codex parity bar.

Reads docs/parity/WI-1150-codex-bar.md for the pass/fail rules. This script
does not start a Codex session and does not touch vogt-dev unless --execute
is given. The default is a dry run that prints the calls it would make.

  VOGT_DEV_ENGINE   default http://100.101.101.57:8910
  VOGT_DEV_TOKEN    bearer token; required with --execute; never logged

Items 5, 7, 8 and 10 need a shell on the engine host. They are listed as
manual and left BLOCKED.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from typing import Any

DEFAULT_ENGINE = "http://100.101.101.57:8910"

MANUAL = {
    "5": "gitguard: run the destructive git list only in /tmp/codex-parity-{a,b}",
    "7": "bwrap: codex sandbox linux inside the pod",
    "8": "codex app-server initialize and codex-acp handshake",
    "10": "rebase the fork onto the next upstream stable tag; do not push",
}


def _request(
    engine: str,
    token: str,
    method: str,
    path: str,
    body: dict[str, Any] | None = None,
) -> dict[str, Any]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(
        engine.rstrip("/") + path,
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": "application/json",
            "Content-Type": "application/json",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        raw = resp.read().decode()
    parsed: dict[str, Any] = json.loads(raw) if raw else {}
    return parsed


def plan() -> list[str]:
    return [
        "GET /api/health (or /api/config) — record engine version",
        "POST session.start template=codex model=grok-4.7 permission_mode=default",
        "POST session.start permission_mode=accept_edits",
        "POST session.start autopilot=true (two-step task)",
        "GET session screen until ready; record title and approval",
        "POST session.hibernate then session wake by the same id",
        "MCP board_list and cadastre lookup from the driven session",
        "MCP agent_cli_list — kind github-release must not 500",
        "chat.create only when ENGINE_CHAT_DRIVER=codex (else BLOCKED)",
    ]


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--execute",
        action="store_true",
        help="call the engine; refused unless VOGT_DEV_CONFIRM=1 as well",
    )
    args = parser.parse_args(argv)
    engine = os.environ.get("VOGT_DEV_ENGINE", DEFAULT_ENGINE)
    print(f"engine {engine}")
    print("checklist docs/parity/WI-1150-codex-bar.md")
    for line in plan():
        print(f"  - {line}")
    for item, text in MANUAL.items():
        print(f"  - item {item} MANUAL: {text}")
    if not args.execute:
        print("dry-run: not calling the engine (pass --execute after the fork is live)")
        return 0
    if os.environ.get("VOGT_DEV_CONFIRM") != "1":
        print("refusing: set VOGT_DEV_CONFIRM=1 to call vogt-dev", file=sys.stderr)
        return 2
    token = os.environ.get("VOGT_DEV_TOKEN", "")
    if not token:
        print("refusing: VOGT_DEV_TOKEN is empty", file=sys.stderr)
        return 2
    try:
        health = _request(engine, token, "GET", "/api/config")
    except urllib.error.HTTPError as exc:
        print(f"FAIL health: HTTP {exc.code}", file=sys.stderr)
        return 1
    version = health.get("version") or health.get("product_version")
    print(f"engine config reachable, version={version!r}")
    print(
        "session starts are not issued by this collector; drive them from the checklist"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
