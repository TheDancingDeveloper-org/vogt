"""The workspace-volume row on engine status, sweep and status (WI-1304).

The engine reports its workspace volume on `GET /api/status`. Core carries
it as `disk` on `engine.status`, `sweep` and `status`; without an engine, or
when the engine does not answer, the row is absent rather than "fine".
"""

from __future__ import annotations

import dataclasses
from typing import Any

import pytest

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import EngineStatusParams, StatusParams, SweepParams
from vogt.application.services.collect import sweep
from vogt.application.services.engine_status import engine_status
from vogt.application.services.instance import status

DISK = {
    "path": "/w",
    "total_bytes": 1000,
    "free_bytes": 100,
    "used_percent": 90,
    "threshold_percent": 85,
    "needs_attention": True,
}


class _Engine:
    """Answers `operational_status` only; anything else is not this test's."""

    def __init__(self, payload: dict[str, Any] | Exception) -> None:
        self.payload = payload

    def operational_status(self) -> dict[str, Any]:
        if isinstance(self.payload, Exception):
            raise self.payload
        return self.payload

    def __getattr__(self, name: str) -> Any:
        raise EngineUnavailable(f"{name} is not faked here")


def _with(instance: AppContext, engine: _Engine | None) -> AppContext:
    return dataclasses.replace(instance, engine=engine)  # type: ignore[arg-type]


def test_engine_status_carries_the_disk_row(instance: AppContext) -> None:
    ctx = _with(instance, _Engine({"version": "1", "disk": DISK}))
    disk = engine_status(ctx, EngineStatusParams()).disk
    assert disk is not None
    assert disk.used_percent == 90
    assert disk.threshold_percent == 85
    assert disk.needs_attention is True


def test_engine_status_without_a_disk_row_says_none(instance: AppContext) -> None:
    ctx = _with(instance, _Engine({"version": "1"}))
    assert engine_status(ctx, EngineStatusParams()).disk is None
    malformed = _with(
        instance, _Engine({"version": "1", "disk": {"used_percent": "x"}})
    )
    assert engine_status(malformed, EngineStatusParams()).disk is None


@pytest.mark.parametrize("surface", ["status", "sweep"])
def test_status_and_sweep_carry_the_disk_row(
    instance: AppContext, surface: str
) -> None:
    ctx = _with(instance, _Engine({"disk": DISK}))
    if surface == "status":
        disk = status(ctx, StatusParams()).disk
    else:
        disk = sweep(ctx, SweepParams(reason="test")).disk
    assert disk is not None
    assert disk.needs_attention is True
    assert disk.free_bytes == 100


@pytest.mark.parametrize("surface", ["status", "sweep"])
@pytest.mark.parametrize(
    "engine",
    [None, _Engine(EngineUnavailable("down")), _Engine(RuntimeError("boom"))],
    ids=["no-engine", "engine-unavailable", "engine-error"],
)
def test_the_row_is_absent_when_the_engine_cannot_say(
    instance: AppContext, surface: str, engine: _Engine | None
) -> None:
    ctx = _with(instance, engine)
    if surface == "status":
        assert status(ctx, StatusParams()).disk is None
    else:
        assert sweep(ctx, SweepParams(reason="test")).disk is None
