"""Budget stops leave a hand-off and continue from it (WI-1833)."""

from __future__ import annotations

import dataclasses
import json
import subprocess
from pathlib import Path

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    GetWorkParams,
    PreferenceSetParams,
    RegisterProjectParams,
    StartSessionParams,
)
from vogt.application.services import (
    comment_work,
    get_work,
    register_project,
    set_preference,
    start_session,
)
from vogt.application.services.budget_handoff import (
    RESTART_NOTE,
    read_git_state,
    settle_budget_stops,
)
from vogt.core import dispatch
from vogt.core.entities import HandoffFields

from tests.conftest import native_work_item
from tests.test_sessions import ROOT, StandInEngine

WHY = "budget hand-off test"


class BudgetEngine(StandInEngine):
    """Lists the sessions in `stopped` as hibernated at their budget."""

    def __init__(self) -> None:
        super().__init__()
        self.stopped: dict[str, str] = {}
        self.budgets: dict[str, dict[str, int]] = {}

    def stop_at_budget(self, engine_id: str, cwd: str = ROOT) -> None:
        self.alive.pop(engine_id, None)
        self.stopped[engine_id] = cwd

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        status, raw = super().__call__(url, headers, body, method)
        if method == "GET" and url.split("?", 1)[0].endswith("/api/sessions"):
            payload = json.loads(raw)
            payload["sessions"] += [
                {
                    "id": key,
                    "name": key,
                    "activity": "hibernated",
                    "cwd": cwd,
                    "alive": False,
                    "budget": self.budgets.get(key),
                    "hibernation": {
                        "at": "2026-10-18T00:00:00Z",
                        "trigger": "budget",
                        "reason": "turns 120 of 120",
                    },
                }
                for key, cwd in self.stopped.items()
            ]
            return status, json.dumps(payload).encode()
        return status, raw


def _setup(instance: AppContext) -> tuple[AppContext, BudgetEngine, str]:
    engine = BudgetEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title="Long loop", project="vogt")
    start_session(
        ctx,
        StartSessionParams(
            work_item=item.ref, template="claude", task="loop on CI", reason=WHY
        ),
    )
    return ctx, engine, item.ref


def _comments(ctx: AppContext, ref: str) -> list:  # type: ignore[type-arg]
    return get_work(ctx, GetWorkParams(ref=ref, comment_limit=50)).comments


def _repo(tmp_path: Path) -> Path:
    tmp_path = tmp_path / "repo"
    tmp_path.mkdir()

    def git(*args: str) -> None:
        subprocess.run(["git", *args], cwd=tmp_path, check=True, capture_output=True)

    git("init", "-q", "-b", "wi-9-loop")
    git(
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "x",
    )
    (tmp_path / "half.py").write_text("print(1)\n")
    return tmp_path


def test_git_state_reads_branch_head_and_dirty_paths(tmp_path: Path) -> None:
    state = read_git_state(str(_repo(tmp_path)))
    assert state is not None
    assert state.branch == "wi-9-loop"
    assert state.head_sha and len(state.head_sha) == 40
    assert state.dirty == ["half.py"]
    assert read_git_state(str(tmp_path / "missing")) is None


def test_a_budget_stop_without_a_handoff_gets_one_from_git(
    instance: AppContext, tmp_path: Path
) -> None:
    ctx, engine, ref = _setup(instance)
    set_preference(
        ctx,
        PreferenceSetParams(
            key=dispatch.BUDGET_RESTARTS_KEY, value={"value": 0}, reason=WHY
        ),
    )
    engine.stop_at_budget("eng-1", str(_repo(tmp_path)))

    first = settle_budget_stops(ctx)
    again = settle_budget_stops(ctx)

    assert first.handoffs_written == [ref]
    assert again.handoffs_written == []
    assert first.restarted == []
    handoffs = [one for one in _comments(ctx, ref) if one.kind == "handoff"]
    assert len(handoffs) == 1
    fields = handoffs[0].handoff
    assert fields is not None
    assert (fields.branch, fields.session) == ("wi-9-loop", "eng-1")
    assert fields.touch_points == ["half.py"]
    assert "turns 120 of 120" in handoffs[0].body


def test_the_agents_own_handoff_is_kept_and_the_item_restarts_once(
    instance: AppContext,
) -> None:
    ctx, engine, ref = _setup(instance)
    comment_work(
        ctx,
        CommentParams(
            ref=ref,
            body="rebased, CI red on lint",
            kind="handoff",
            handoff=HandoffFields(branch="wi-9-loop", remaining=["fix lint"]),
            reason=WHY,
        ),
    )
    engine.stop_at_budget("eng-1")

    first = settle_budget_stops(ctx)
    assert first.handoffs_written == []
    assert first.restarted == [ref]
    spec = engine.last_spec
    assert "rebased, CI red on lint" in spec["prompt"]

    # The restart is itself stopped at its budget: the cap of one is spent.
    engine.stop_at_budget("eng-2")
    second = settle_budget_stops(ctx)
    assert second.handoffs_written == [ref]
    assert second.restarted == []
    notes = [one for one in _comments(ctx, ref) if one.body.startswith(RESTART_NOTE)]
    assert len(notes) == 1
    assert engine.counter == 2


def test_a_purpose_start_on_haiku_gets_the_larger_budget(instance: AppContext) -> None:
    engine = BudgetEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title="Haiku", project="vogt")
    start_session(
        ctx,
        StartSessionParams(
            work_item=item.ref,
            purpose="implement",
            model="haiku",
            task="do it",
            reason=WHY,
        ),
    )
    assert engine.last_spec["budget"]["turns"] == 200


def test_a_purpose_started_session_restarts_on_its_own_model_and_budget(
    instance: AppContext,
) -> None:
    engine = BudgetEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title="Haiku loop", project="vogt")
    start_session(
        ctx,
        StartSessionParams(
            work_item=item.ref,
            purpose="implement",
            model="haiku",
            effort="high",
            task="loop on CI",
            reason=WHY,
        ),
    )
    engine.budgets["eng-1"] = {"turns": 200, "wall_clock_minutes": 90}
    engine.stop_at_budget("eng-1")

    settled = settle_budget_stops(ctx)

    assert settled.restarted == [item.ref]
    spec = engine.last_spec
    assert spec["budget"] == {"turns": 200, "wall_clock_minutes": 90}
    assert spec.get("model") == "haiku"
    assert spec.get("effort") == "high"
