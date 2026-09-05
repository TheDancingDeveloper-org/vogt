"""What one Inbox page costs the observed store (#580).

The projection used to re-read every latest check of a project, and roll it
up again, once *per failing check* in that project — Σ failing × checks per
project. On a two-week-old estate that was 730 queries and 732,425 observation
objects for `limit=1`, the read crossed the PWA's 8 s deadline, and every badge
refresh became a retry that the core still ran to completion. These tests pin
the shape: the store is read a fixed number of times however many checks fail,
and triage is one lookup for the page rather than one per entry.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from vogt.application.context import AppContext
from vogt.application.models import (
    InboxArchiveParams,
    InboxListParams,
    RegisterProjectParams,
)
from vogt.application.services import archive_inbox, list_inbox, register_project
from vogt.collectors.base import finding
from vogt.core.entities import InboxTriage, Project
from vogt.storage.sqlite.declared import SqliteReadView

WHY = "test"


def _projects(ctx: AppContext, tmp_path: Path, names: list[str]) -> list[Project]:
    for name in names:
        register_project(
            ctx,
            RegisterProjectParams(
                name=name, root_path=str(tmp_path / name), reason=WHY
            ),
        )
    with ctx.declared.read() as view:
        by_name = {p.name: p for p in view.list_projects(limit=100, offset=0)}
    return [by_name[name] for name in names]


def _record_checks(
    ctx: AppContext, project: Project, runs: list[tuple[str, str, str, str]]
) -> None:
    """Seed CI check observations the way `gh-actions` would."""
    findings = [
        finding(
            kind="ci.check",
            subject_key=f"ci:{project.slug}@{revision}:{name}",
            project=project,
            payload={
                "revision": revision,
                "check": name,
                "conclusion": conclusion,
                "updated_at": ran_at,
            },
        )
        for revision, name, conclusion, ran_at in runs
    ]
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector="forge-checks", scope=[project.id], at=now)
    ctx.observed.append(row.id, findings, at=now)
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={"projects": 1}, at=now)
    ctx.observed.rebuild_latest()


class _CountingLatest:
    """Count `latest()` reads without changing what they answer."""

    def __init__(self, ctx: AppContext) -> None:
        self.calls = 0
        self._inner = ctx.observed.latest

    def __call__(self, *args: Any, **kwargs: Any) -> Any:
        self.calls += 1
        return self._inner(*args, **kwargs)


def test_the_store_is_read_a_fixed_number_of_times_however_many_checks_fail(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    busy, quiet = _projects(instance, tmp_path, ["busy", "quiet"])
    # `busy`: an old revision full of failures (history, not the verdict) and
    # a newest revision with two failures and one pass. Every failing row is
    # one iteration of the loop that used to re-read the project.
    old = [
        (f"0000{i:03d}", f"check-{i}", "failure", f"2026-08-0{1 + i % 9}T00:00:00Z")
        for i in range(40)
    ]
    newest = [
        ("9fe53d8", "build", "failure", "2026-08-20T09:00:00Z"),
        ("9fe53d8", "lint", "failure", "2026-08-20T09:01:00Z"),
        ("9fe53d8", "test", "success", "2026-08-20T09:02:00Z"),
    ]
    _record_checks(instance, busy, [*old, *newest])
    _record_checks(
        instance, quiet, [("abc1234", "build", "failure", "2026-08-21T00:00:00Z")]
    )

    counter = _CountingLatest(instance)
    monkeypatch.setattr(instance.observed, "latest", counter)

    result = list_inbox(instance, InboxListParams(limit=1))

    # notifications, checks, task runs — and nothing per failing check.
    assert counter.calls == 3
    assert result.counts["active"] == 3
    with_ci = [e for e in _all_entries(instance) if e.source == "ci"]
    assert sorted(e.title for e in with_ci) == [
        "CI failing: build",
        "CI failing: build",
        "CI failing: lint",
    ]
    # History stays history: nothing from the forty old failures is a verdict.
    assert all("0000" not in (e.summary or "") for e in with_ci)


def test_triage_is_applied_from_one_lookup_for_the_whole_page(
    instance: AppContext, tmp_path: Path, monkeypatch: Any
) -> None:
    (project,) = _projects(instance, tmp_path, ["one"])
    _record_checks(
        instance,
        project,
        [
            ("9fe53d8", "build", "failure", "2026-08-20T09:00:00Z"),
            ("9fe53d8", "lint", "failure", "2026-08-20T09:01:00Z"),
        ],
    )
    entries = _all_entries(instance)
    assert len(entries) == 2
    archive_inbox(
        instance, InboxArchiveParams(entry_key=entries[0].entry_key, reason=WHY)
    )

    def refuse(_self: SqliteReadView, _key: str) -> InboxTriage | None:
        msg = "triage must be one lookup per page, not one per entry (#580)"
        raise AssertionError(msg)

    monkeypatch.setattr(SqliteReadView, "inbox_triage_by_key", refuse)
    result = list_inbox(
        instance, InboxListParams(limit=10, triage_states=["active", "archived"])
    )

    assert result.counts == {"active": 1, "archived": 1, "snoozed": 0}


def test_a_batched_triage_lookup_survives_the_bound_parameter_ceiling(
    instance: AppContext,
) -> None:
    """More keys than one SQLite statement may carry, in one call."""
    keys = [f"drift:key-{i}" for i in range(1200)]
    with instance.declared.read() as view:
        assert view.inbox_triage_by_keys(keys) == {}
        assert view.inbox_triage_by_keys([]) == {}


def _all_entries(ctx: AppContext) -> list[Any]:
    return list(list_inbox(ctx, InboxListParams(limit=100)).entries)
