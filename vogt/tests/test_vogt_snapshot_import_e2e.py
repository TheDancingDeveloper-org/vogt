"""End to end: the snapshot importer against two throwaway local cores (WI-4125).

Neither a prod nor a dev instance is touched.  The source core is seeded
through the CLI, exported through its own `export` operation, then the
target core is served on loopback and the importer writes into it through
the real MCP HTTP transport.
"""

from __future__ import annotations

import json
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

import pytest
from tools.vogt_snapshot_import import main

JSON = dict[str, Any]
REASON = "e2e: WI-4125 throwaway cores"


def _vogt(data_dir: Path, *arguments: str) -> JSON:
    completed = subprocess.run(
        [
            sys.executable,
            "-m",
            "vogt",
            "--data-dir",
            str(data_dir),
            "--json",
            *arguments,
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    parsed: Any = json.loads(completed.stdout)
    assert isinstance(parsed, dict)
    return parsed


def _seed_source(data_dir: Path, repo_root: Path) -> None:
    _vogt(data_dir, "init")
    _vogt(
        data_dir,
        "actor",
        "create",
        "--identity-ref",
        "agent:pod:source",
        "--display-name",
        "source worker",
        "--kind",
        "agent",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "project",
        "register",
        "--name",
        "e2e-alpha",
        "--root-path",
        str(repo_root / "e2e-alpha"),
        "--lifecycle-state",
        "active",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "label",
        "create",
        "--name",
        "bug",
        "--color",
        "#d73a4a",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "initiative",
        "create",
        "--title",
        "launch",
        "--body",
        "the launch",
        "--weight",
        "2",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "work",
        "create",
        "--kind",
        "bug",
        "--title",
        "first item",
        "--body",
        "first body",
        "--project",
        "e2e-alpha",
        "--priority",
        "p1",
        "--labels",
        "bug",
        "--initiative",
        "launch",
        "--assignee",
        "agent:pod:source",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "work",
        "create",
        "--kind",
        "chore",
        "--title",
        "second item",
        "--body",
        "second body",
        "--priority",
        "p3",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "work",
        "relate",
        "--ref",
        "WI-1",
        "--kind",
        "depends_on",
        "--target",
        "WI-2",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "work",
        "transition",
        "--ref",
        "WI-2",
        "--to-state",
        "in_progress",
        "--reason",
        REASON,
    )
    _vogt(
        data_dir,
        "work",
        "comment",
        "--ref",
        "WI-1",
        "--body",
        "a prod note",
        "--reason",
        REASON,
    )


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _wait_for_core(port: int, process: subprocess.Popen[bytes]) -> None:
    deadline = time.monotonic() + 30
    url = f"http://127.0.0.1:{port}/health/live"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(f"target core exited with {process.returncode}")
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if response.status == 200:
                    return
        except (urllib.error.URLError, TimeoutError):
            time.sleep(0.15)
    raise AssertionError("target core did not become ready")


@pytest.mark.estate
def test_import_between_two_throwaway_cores(tmp_path: Path) -> None:
    source_dir = tmp_path / "source"
    target_dir = tmp_path / "target"
    repo_root = tmp_path / "repos"
    _seed_source(source_dir, repo_root)
    export_path = tmp_path / "export.json"
    _vogt(
        source_dir,
        "export",
        "--destination",
        str(export_path),
        "--reason",
        REASON,
    )

    _vogt(target_dir, "init")
    _vogt(
        target_dir,
        "actor",
        "create",
        "--identity-ref",
        "local:importer",
        "--display-name",
        "importer",
        "--kind",
        "human",
        "--reason",
        REASON,
    )
    issued = _vogt(
        target_dir,
        "token",
        "issue",
        "--actor",
        "local:importer",
        "--name",
        "e2e",
        "--scopes",
        "admin,work.write,project.write,read",
        "--reason",
        REASON,
    )
    token_file = tmp_path / "token"
    token_file.write_text(str(issued["secret"]), encoding="utf-8")
    token_file.chmod(0o600)

    port = _free_port()
    process = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "vogt",
            "--data-dir",
            str(target_dir),
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            str(port),
            "--no-schedule",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        _wait_for_core(port, process)
        code = main(
            [
                "--source",
                str(export_path),
                "--target-url",
                f"http://127.0.0.1:{port}",
                "--token-file",
                str(token_file),
                "--apply",
                "--confirm",
                "--state-file",
                str(tmp_path / "state.json"),
                "--reason",
                REASON,
                "--json",
            ]
        )
        assert code == 0
    finally:
        process.terminate()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)

    listed = _vogt(target_dir, "work", "list", "--include-finished", "--mode", "full")
    items = {row["ref"]: row for row in listed["items"]}
    assert sorted(items) == ["WI-1", "WI-2"]
    assert items["WI-1"]["title"] == "first item"
    assert items["WI-1"]["state"] == "open"
    assert items["WI-1"]["labels"] == ["bug"]
    assert items["WI-1"]["initiative_id"] is not None
    assert items["WI-2"]["state"] == "in_progress"

    fetched = _vogt(target_dir, "work", "get", "--ref", "WI-1")
    assert [relation["kind"] for relation in fetched["item"]["relations"]] == [
        "depends_on"
    ]
    assert [comment["body"] for comment in fetched["comments"]] == [
        "_prod comment by source worker at 2026-11-12T00:00:00Z_\n\na prod note"
    ] or fetched["comments"][0]["body"].endswith("a prod note")

    project = _vogt(target_dir, "project", "get", "--slug", "e2e-alpha")["project"]
    assert project["write_back"] == "none"
    assert project["link_state"] == "unlinked"
    assert project["root_path"] == str(Path(project["root_path"]))
