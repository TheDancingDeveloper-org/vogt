"""The M0 demo, as an executable acceptance test.

From `ROADMAP.md`:

    `vogt init`, register a project record from the CLI with a reason,
    `status` shows revision 1, the audit row carries actor + reason, and
    `/events` returns exactly one row at `seq=1`.

A stage is done when its demo runs. Keeping the demo here means it keeps
running, rather than being something that worked once on the day.
"""

from __future__ import annotations

import json
import shlex
from pathlib import Path
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.cli.main import EXIT_OK, run
from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.application.context import AppContext


def _cli(context: AppContext, *argv: str) -> Any:
    result = run(["--json", *argv], context=context)
    assert result.exit_code == EXIT_OK, result.stderr
    return json.loads(result.stdout)


def test_m0_demo(context: AppContext) -> None:
    # vogt init
    init = _cli(context, "init")
    assert init["created"] is True
    assert Path(init["data_dir"]).is_dir()

    # register a project record from the CLI with a reason
    reason = "M0 demo: the estate's own tracker is its first project"
    registered = _cli(
        context,
        *shlex.split("project register --name Vogt --root-path /srv/vogt"),
        "--reason",
        reason,
    )
    project_id = registered["project"]["id"]
    assert registered["project"]["slug"] == "vogt"

    # status shows revision 1
    status = _cli(context, "status")
    assert status["revision"] == 1
    assert status["counts"] == {
        "projects": 1,
        "actors": 1,
        "events": 1,
        "audit": 2,
        "work_items": 0,
        "initiatives": 0,
    }

    # the audit row carries actor + reason
    audit = _cli(context, "audit", "list")["records"]
    latest = audit[0]
    assert latest["operation"] == "project.register"
    assert latest["actor_identity_ref"] == "local:test-user"
    assert latest["reason"] == reason
    assert latest["entity_id"] == project_id
    assert latest["revision"] == 1

    # /events returns exactly one row at seq=1
    events = _cli(context, "events", "list")["events"]
    assert len(events) == 1
    assert events[0]["seq"] == 1
    assert events[0]["entity_id"] == project_id

    # and the same answers come back over REST
    with TestClient(build_app(context_factory=lambda: context)) as client:
        assert client.get(f"{API_PREFIX}/status").json() == status
        feed = client.get(f"{API_PREFIX}/events").json()
        assert feed["events"] == events
        assert feed["next_cursor"] == 1


def test_the_demo_leaves_both_stores_on_disk(context: AppContext) -> None:
    _cli(context, "init")
    assert context.config.declared_db_path.exists()
    assert context.config.observed_db_path.exists()


@pytest.mark.parametrize("command", [("status",), ("events", "list")])
def test_reads_before_init_do_not_create_anything(
    context: AppContext, command: tuple[str, ...]
) -> None:
    result = run(list(command), context=context)
    assert result.exit_code != EXIT_OK
    assert not context.config.declared_db_path.exists()
