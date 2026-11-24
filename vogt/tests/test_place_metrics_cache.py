"""The shell's badge counts are shared while the stores stand still (WI-1224).

`place.metrics` ranked the whole backlog and rebuilt the Inbox for every tab
on every change, and the cache meant to share the Inbox half was keyed on the
store objects, which are built per request, so it never hit. These tests pin
what the shared answer is keyed on — every way the stores can move must
invalidate it — and that a burst of callers costs one computation.
"""

from __future__ import annotations

import dataclasses
import json
import threading
import time
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application import memo as memo_module
from vogt.application.context import AppContext, build_context
from vogt.application.memo import StoreMemo, store_state
from vogt.application.models import (
    CreateWorkParams,
    PlaceMetricsParams,
    RegisterProjectParams,
)
from vogt.application.services import create_work, place_metrics, register_project
from vogt.application.services import place as place_module
from vogt.collectors.base import finding

WHY = "badge cache test"


def _counting(monkeypatch: pytest.MonkeyPatch, target: object, name: str) -> list[int]:
    calls: list[int] = []
    inner: Callable[..., Any] = getattr(target, name)

    def counted(*args: Any, **kwargs: Any) -> Any:
        calls.append(1)
        return inner(*args, **kwargs)

    monkeypatch.setattr(target, name, counted)
    return calls


def _fresh(ctx: AppContext) -> AppContext:
    """A new context over the same stores, as the HTTP adapter builds per
    request — the case the old cache never hit."""
    return build_context(config=ctx.config, principal=ctx.principal)


def _record_issue(ctx: AppContext, project_slug: str, number: int) -> None:
    with ctx.declared.read() as view:
        project = next(
            p for p in view.list_projects(limit=100, offset=0) if p.slug == project_slug
        )
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector="test", scope=[project.id], at=now)
    ctx.observed.append(
        row.id,
        [
            finding(
                kind="forge.issue",
                subject_key=f"gh:{project_slug}#{number}",
                project=project,
                payload={"title": f"issue {number}", "state": "open"},
            )
        ],
        at=now,
    )
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={}, at=now)
    ctx.observed.rebuild_latest()


def test_the_counts_are_shared_across_per_request_contexts(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    ranked = _counting(monkeypatch, place_module, "backlog")
    first = place_metrics(_fresh(instance), PlaceMetricsParams())
    second = place_metrics(_fresh(instance), PlaceMetricsParams())
    assert len(ranked) == 1
    assert first.backlog_total_considered == second.backlog_total_considered


def test_a_declared_write_invalidates_the_counts(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    before = place_metrics(_fresh(instance), PlaceMetricsParams())
    ranked = _counting(monkeypatch, place_module, "backlog")
    create_work(instance, CreateWorkParams(kind="bug", title="a new bug", reason=WHY))
    after = place_metrics(_fresh(instance), PlaceMetricsParams())
    assert len(ranked) == 1
    assert after.revision > before.revision
    assert after.work_total == (before.work_total or 0) + 1
    assert after.backlog_total_considered == (before.backlog_total_considered or 0) + 1


def test_a_rebuilt_observed_projection_invalidates_the_counts(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A sweep writes no declared revision; the projection generation is
    what tells the memo the observed half moved."""
    register_project(
        instance,
        RegisterProjectParams(name="Linked", root_path=str(tmp_path), reason=WHY),
    )
    before = place_metrics(_fresh(instance), PlaceMetricsParams())
    generation = instance.observed.latest_generation()
    revision = before.revision
    ranked = _counting(monkeypatch, place_module, "backlog")

    _record_issue(instance, "linked", 1)

    assert instance.observed.latest_generation() == generation + 1
    after = place_metrics(_fresh(instance), PlaceMetricsParams())
    assert after.revision == revision, "nothing declared moved"
    assert len(ranked) == 1, "the observed projection moved, so it recomputed"


def test_an_observed_side_event_invalidates_the_counts(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    place_metrics(_fresh(instance), PlaceMetricsParams())
    ranked = _counting(monkeypatch, place_module, "backlog")
    instance.declared.publish_event(
        kind="sweep.completed",
        entity_kind="sweep",
        entity_id="swp_test",
        summary={},
        at=instance.clock(),
    )
    place_metrics(_fresh(instance), PlaceMetricsParams())
    assert len(ranked) == 1


def test_an_answer_with_a_provider_missing_is_not_remembered(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    def broken(*args: Any, **kwargs: Any) -> Any:
        raise RuntimeError("backlog unavailable")

    monkeypatch.setattr(place_module, "backlog", broken)
    assert (
        place_metrics(_fresh(instance), PlaceMetricsParams()).backlog_total_considered
        is None
    )
    monkeypatch.undo()
    healed = place_metrics(_fresh(instance), PlaceMetricsParams())
    assert healed.backlog_total_considered is not None


def test_two_data_directories_never_share_an_answer(
    instance: AppContext, tmp_path: Path
) -> None:
    from vogt.application.models import InitParams
    from vogt.application.services import init_instance
    from vogt.config import VogtConfig

    create_work(instance, CreateWorkParams(kind="bug", title="only here", reason=WHY))
    other = build_context(
        config=VogtConfig(data_dir=tmp_path / "other"), principal=instance.principal
    )
    init_instance(other, InitParams())
    here = place_metrics(_fresh(instance), PlaceMetricsParams())
    there = place_metrics(other, PlaceMetricsParams())
    assert here.work_total == 1
    assert there.work_total == 0


def test_a_burst_of_callers_costs_one_computation(instance: AppContext) -> None:
    memo: StoreMemo[int] = StoreMemo()
    computed: list[int] = []
    gate = threading.Event()

    def slow() -> int:
        computed.append(1)
        gate.wait(5)
        return 42

    answers: list[int] = []

    def caller() -> None:
        answers.append(memo.get(_fresh(instance), slow))

    threads = [threading.Thread(target=caller) for _ in range(8)]
    for thread in threads:
        thread.start()
    time.sleep(0.2)
    gate.set()
    for thread in threads:
        thread.join(5)
    assert answers == [42] * 8
    assert len(computed) == 1


def test_waiters_take_a_computation_that_started_after_they_arrived(
    instance: AppContext,
) -> None:
    """The store moving mid-computation does not make every waiter recompute
    in turn: an answer read after a caller arrived is as fresh as one it would
    have computed itself."""
    memo: StoreMemo[int] = StoreMemo()
    computed: list[int] = []
    started = threading.Event()
    gate = threading.Event()

    def slow_then_write() -> int:
        computed.append(1)
        started.set()
        gate.wait(5)
        return len(computed)

    first: list[int] = []
    leader = threading.Thread(
        target=lambda: first.append(memo.get(_fresh(instance), slow_then_write))
    )
    leader.start()
    started.wait(5)
    # A caller that arrives while the leader computes, then the store moves.
    late: list[int] = []
    follower = threading.Thread(
        target=lambda: late.append(memo.get(_fresh(instance), slow_then_write))
    )
    follower.start()
    time.sleep(0.1)
    create_work(instance, CreateWorkParams(kind="bug", title="moves", reason=WHY))
    gate.set()
    leader.join(5)
    follower.join(5)
    # The follower arrived *before* the leader's read: it must not take the
    # leader's answer, so it computes again, against the moved store.
    assert first == [1]
    assert late == [2]

    # A caller arriving now, with nothing moved since, is answered from memory.
    assert memo.get(_fresh(instance), slow_then_write) == 2
    assert len(computed) == 2


def test_the_key_moves_with_each_kind_of_change(instance: AppContext) -> None:
    start = store_state(instance)
    create_work(instance, CreateWorkParams(kind="bug", title="x", reason=WHY))
    declared = store_state(instance)
    assert declared.revision > start.revision
    instance.observed.rebuild_latest()
    observed = store_state(instance)
    assert observed.latest_generation == declared.latest_generation + 1
    assert observed.revision == declared.revision


def test_memo_forgets_the_least_recent_stores(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    from vogt.application.models import InitParams
    from vogt.application.services import init_instance
    from vogt.config import VogtConfig

    monkeypatch.setattr(memo_module, "MEMO_STORES_MAX", 2)
    memo: StoreMemo[str] = StoreMemo()
    contexts = []
    for index in range(3):
        ctx = build_context(
            config=VogtConfig(data_dir=tmp_path / f"i{index}"),
            principal=instance.principal,
        )
        init_instance(ctx, InitParams())
        contexts.append(ctx)
        memo.get(ctx, lambda: "answer")
    recomputed: list[int] = []

    def again() -> str:
        recomputed.append(1)
        return "again"

    assert memo.get(contexts[0], again) == "again"
    assert recomputed == [1]


# -- the Inbox half ------------------------------------------------------------


class _Engine:
    """An engine whose session list the test changes between badge reads."""

    def __init__(self) -> None:
        self.rows: list[dict[str, Any]] = []
        self.listed = 0

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.removeprefix("http://127.0.0.1:8910").partition("?")[0]
        if method == "GET" and path == "/api/sessions":
            self.listed += 1
            return 200, json.dumps(self.rows).encode()
        return 404, b""


def test_the_live_inbox_half_is_read_on_every_badge(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A session that starts waiting for a person moves no store; the badge
    still counts it on the next read, because only the stored half is shared."""
    engine = _Engine()
    wired = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    stored = _counting(monkeypatch, wired.observed, "latest")
    quiet = place_metrics(wired, PlaceMetricsParams())
    built = len(stored)
    engine.rows.append(
        {
            "id": "00000000-0000-4000-8000-000000000001",
            "name": "worker",
            "activity": "waiting-for-input",
            "alive": True,
            "cwd": "/srv",
            "created_at": "2026-10-05T00:00:00Z",
            "last_output_at": "2026-10-05T00:00:00Z",
            "activity_changed_at": "2026-10-05T00:00:00Z",
        }
    )
    asking = place_metrics(wired, PlaceMetricsParams())
    assert asking.inbox_active == (quiet.inbox_active or 0) + 1
    assert len(stored) == built, "the stored half was not rebuilt"
    assert engine.listed == 2
