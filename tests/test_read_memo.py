"""inbox.list, backlog and coverage share one answer while the stores hold still.

WI-2691. Each of the three was rebuilt on every call (0.9–2.1 s on prod).
The stored answer is now a `StoreMemo`, so a second call with nothing written
does not recompute it, and a write that the answer depends on — a declared
revision, an event, or an observed rebuild — does. The live half of the Inbox
stays live: one `list_sessions` per request, and the reachability line reuses
it instead of asking again.
"""

from __future__ import annotations

from dataclasses import replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, cast

from vogt.adapters.engine.client import EngineClient, EngineSession, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    CoverageParams,
    InboxListParams,
    RegisterProjectParams,
)
from vogt.application.services import (
    backlog,
    coverage,
    list_inbox,
    register_project,
)
from vogt.application.services.views import _gather
from vogt.collectors.base import finding
from vogt.storage.interface import ProjectUpdate

from tests.conftest import native_work_item

WHY = "test"


class _CountingEngine:
    """Stand-in engine that counts `list_sessions` and can refuse."""

    def __init__(self, *, fail: bool = False) -> None:
        self.calls = 0
        self.fail = fail

    def list_sessions(
        self, *, include_hidden: bool = False
    ) -> tuple[list[EngineSession], int]:
        del include_hidden
        self.calls += 1
        if self.fail:
            raise EngineUnavailable("down")
        return (
            [
                EngineSession(
                    id="eng-1",
                    name="worker",
                    activity="awaiting-approval",
                    cwd="/tmp",
                    alive=True,
                    activity_changed_at="2026-10-29T00:00:00Z",
                )
            ],
            0,
        )


def _wired(instance: AppContext, engine: _CountingEngine) -> AppContext:
    """`instance` with the stand-in as its engine.

    `EngineClient` is a frozen dataclass, so the stand-in cannot subclass it
    and keep a mutable call count. Inbox only ever calls `list_sessions` on
    the engine, which `_CountingEngine` provides, so the cast is safe here.
    """
    return replace(instance, engine=cast(EngineClient, engine))


def _project(ctx: AppContext, tmp_path: Path, name: str = "alpha") -> None:
    register_project(
        ctx,
        RegisterProjectParams(name=name, root_path=str(tmp_path / name), reason=WHY),
    )
    # A native item only ranks once its project is linked.
    with ctx.declared.write() as txn:
        project = next(
            p for p in txn.list_projects(limit=100, offset=0) if p.slug == name
        )
        txn.update_project(
            project.id, ProjectUpdate(link_state="linked"), at=ctx.clock()
        )


def _work(ctx: AppContext, title: str, kind: str = "chore") -> None:
    native_work_item(ctx, kind=kind, title=title, project="alpha")


def test_inbox_asks_the_engine_once_and_reuses_the_stored_half(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    _project(instance, tmp_path)
    engine = _CountingEngine()
    wired = _wired(instance, engine)
    stored_calls = {"n": 0}
    real = _gather  # keep a name so the patch below is the only one

    def counting(ctx: AppContext, view: Any) -> Any:
        del ctx, view
        stored_calls["n"] += 1

    from vogt.application.services import inbox as inbox_mod

    real_stored = inbox_mod._collect_stored

    def counting_stored(ctx: AppContext, view: Any) -> Any:
        stored_calls["n"] += 1
        return real_stored(ctx, view)

    monkeypatch.setattr(inbox_mod, "_collect_stored", counting_stored)

    first = list_inbox(wired, InboxListParams())
    second = list_inbox(wired, InboxListParams())

    assert engine.calls == 2  # one per request, not two
    assert stored_calls["n"] == 1  # the second call reused the memo
    assert first.engine_status == "available"
    assert second.counts == first.counts
    assert any(entry.source == "agent" for entry in second.entries)
    del real


def test_inbox_reachability_reuses_the_failed_probe(
    instance: AppContext, tmp_path: Path
) -> None:
    _project(instance, tmp_path)
    engine = _CountingEngine(fail=True)
    wired = _wired(instance, engine)

    result = list_inbox(wired, InboxListParams())

    assert engine.calls == 1
    assert result.engine_status == "unreachable"
    assert result.engine_detail == "down"


def test_a_declared_write_rebuilds_the_inbox(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    _project(instance, tmp_path)
    from vogt.application.services import inbox as inbox_mod

    calls = {"n": 0}
    real = inbox_mod._collect_stored

    def counting(ctx: AppContext, view: Any) -> Any:
        calls["n"] += 1
        return real(ctx, view)

    monkeypatch.setattr(inbox_mod, "_collect_stored", counting)
    list_inbox(instance, InboxListParams())
    register_project(
        instance,
        RegisterProjectParams(
            name="beta", root_path=str(tmp_path / "beta"), reason=WHY
        ),
    )
    list_inbox(instance, InboxListParams())
    assert calls["n"] == 2


def test_backlog_is_shared_until_a_work_item_is_written(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    _project(instance, tmp_path)
    calls = {"n": 0}
    real = _gather

    def counting(*args: Any, **kwargs: Any) -> Any:
        calls["n"] += 1
        return real(*args, **kwargs)

    monkeypatch.setattr("vogt.application.services.views._gather", counting)
    first = backlog(instance, BacklogParams())
    second = backlog(instance, BacklogParams(limit=5, offset=0))
    assert calls["n"] == 1
    assert second.total_considered == first.total_considered

    _work(instance, "one")
    third = backlog(instance, BacklogParams())
    assert calls["n"] == 2
    assert third.total_considered == first.total_considered + 1


def test_a_different_backlog_filter_is_its_own_answer(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    _project(instance, tmp_path)
    _work(instance, "chore one")
    _work(instance, "a bug", kind="bug")
    calls = {"n": 0}
    real = _gather

    def counting(*args: Any, **kwargs: Any) -> Any:
        calls["n"] += 1
        return real(*args, **kwargs)

    monkeypatch.setattr("vogt.application.services.views._gather", counting)
    chores = backlog(instance, BacklogParams(kinds=["chore"]))
    bugs = backlog(instance, BacklogParams(kinds=["bug"]))
    again = backlog(instance, BacklogParams(kinds=["chore"]))
    assert calls["n"] == 2
    assert [row.kind for row in chores.items] == ["chore"]
    assert [row.kind for row in bugs.items] == ["bug"]
    assert again.total_considered == chores.total_considered


def test_coverage_is_shared_until_a_sweep_lands_and_ages_anyway(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    _project(instance, tmp_path)
    with instance.declared.read() as view:
        project = view.list_projects(limit=10, offset=0)[0]
    now = instance.clock()
    row = instance.observed.begin_sweep(
        collector="git-local", scope=[project.id], at=now
    )
    instance.observed.append(
        row.id,
        [finding(kind="marker", subject_key="m:1", project=project, payload={})],
        at=now,
    )
    instance.observed.finish_sweep(row.id, outcome="ok", stats={}, at=now)
    instance.observed.rebuild_latest()

    from vogt.application.services import collect as collect_mod

    calls = {"n": 0}
    real = collect_mod._coverage_now

    def counting(ctx: AppContext) -> Any:
        calls["n"] += 1
        return real(ctx)

    monkeypatch.setattr(collect_mod, "_coverage_now", counting)
    first = coverage(instance, CoverageParams())
    second = coverage(instance, CoverageParams())
    assert calls["n"] == 1
    assert second.swept_project_ids == first.swept_project_ids

    later = replace(instance, clock=lambda: datetime(2099, 1, 1, tzinfo=UTC))
    aged = coverage(later, CoverageParams())
    assert calls["n"] == 1
    ages = {entry.collector: entry.age_seconds for entry in aged.collectors}
    assert ages["git-local"] is not None and ages["git-local"] > 10_000

    row = instance.observed.begin_sweep(
        collector="git-local", scope=[project.id], at=now
    )
    instance.observed.finish_sweep(row.id, outcome="ok", stats={}, at=now)
    instance.observed.rebuild_latest()
    coverage(instance, CoverageParams())
    assert calls["n"] == 2
