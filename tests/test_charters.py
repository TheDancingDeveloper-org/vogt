"""Charters: standing instructions per scope, injected into briefs (WI-2038)."""

from __future__ import annotations

import dataclasses
from typing import Any

import pytest

from vogt.adapters.engine.client import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    CharterGetParams,
    CharterSetParams,
    CharterSetResult,
    CreateInitiativeParams,
    ListAuditParams,
    RegisterProjectParams,
    StartSessionParams,
    UpdateWorkParams,
)
from vogt.application.services import (
    create_initiative,
    get_charter,
    list_audit,
    register_project,
    set_charter,
    start_session,
    update_work,
)
from vogt.errors import CharterVersionConflict, InvalidRequest, NotFound

from tests.conftest import native_work_item
from tests.test_sessions import ROOT, StandInEngine

WHY = "charter test"


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def wired(instance: AppContext, engine: StandInEngine) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    native_work_item(ctx, kind="bug", title="A bug", project="vogt")
    return ctx


def _set(
    ctx: AppContext, scope: str, body: str, ref: str | None = None, **kw: Any
) -> CharterSetResult:
    return set_charter(
        ctx,
        CharterSetParams(scope=scope, ref=ref, body=body, reason=WHY, **kw),  # type: ignore[arg-type]
    )


def test_each_set_is_a_new_version_with_history_and_audit(wired: AppContext) -> None:
    first = _set(wired, "project", "Commit as the bot.", ref="vogt")
    second = _set(wired, "project", "Commit as the bot. Bump crates.", ref="vogt")
    assert (first.charter.version, second.charter.version) == (1, 2)
    assert second.charter.author == wired.principal.identity_ref

    got = get_charter(
        wired, CharterGetParams(scope="project", ref="vogt", history=True)
    )
    assert got.current is not None
    assert got.current.body == "Commit as the bot. Bump crates."
    assert [one.version for one in got.history] == [2, 1]
    assert (
        get_charter(wired, CharterGetParams(scope="project", ref="vogt")).history == []
    )

    audit = list_audit(wired, ListAuditParams())
    assert sum(row.operation == "charter.set" for row in audit.records) == 2


def test_expected_version_refuses_a_stale_write(wired: AppContext) -> None:
    _set(wired, "workspace", "Stop at push.", expected_version=0)
    with pytest.raises(CharterVersionConflict):
        _set(wired, "workspace", "Other.", expected_version=0)
    assert _set(wired, "workspace", "Other.", expected_version=1).charter.version == 2


def test_scope_refs_are_checked(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        _set(wired, "workspace", "x", ref="vogt")
    with pytest.raises(InvalidRequest):
        _set(wired, "project", "x")
    with pytest.raises(NotFound):
        _set(wired, "initiative", "x", ref="nope")
    assert get_charter(wired, CharterGetParams(scope="workspace")).current is None


def test_the_brief_carries_charters_broadest_first(
    wired: AppContext, engine: StandInEngine
) -> None:
    create_initiative(wired, CreateInitiativeParams(title="Port", reason=WHY))
    update_work(wired, UpdateWorkParams(ref="WI-1", initiative="port", reason=WHY))
    _set(wired, "workspace", "WORKSPACE RULE")
    _set(wired, "project", "PROJECT RULE", ref="vogt")
    _set(wired, "initiative", "INITIATIVE RULE", ref="port")

    start_session(
        wired, StartSessionParams(work_item="WI-1", task="TASK TEXT", reason=WHY)
    )
    brief = engine.last_spec["prompt"]
    order = [
        brief.index(text)
        for text in (
            "## Charters",
            "WORKSPACE RULE",
            "PROJECT RULE",
            "INITIATIVE RULE",
            "## Project policy",
            "TASK TEXT",
        )
        if text in brief
    ]
    assert len(order) >= 5 and order == sorted(order)
    assert "### Project — vogt (v1)" in brief
    assert "### Initiative — port (v1)" in brief
    # The same text travels apart, for Codex `developer_instructions`.
    charter = engine.last_spec["charter"]
    assert charter.index("WORKSPACE RULE") < charter.index("INITIATIVE RULE")
    assert "TASK TEXT" not in charter


def test_a_cleared_or_absent_charter_adds_nothing(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "## Charters" not in engine.last_spec["prompt"]
    assert "charter" not in engine.last_spec

    _set(wired, "project", "PROJECT RULE", ref="vogt")
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "PROJECT RULE" in engine.last_spec["prompt"]

    _set(wired, "project", "", ref="vogt")
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "## Charters" not in engine.last_spec["prompt"]
