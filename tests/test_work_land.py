"""`work.land` (WI-1328): land a reviewed branch only at a `no_blockers`
verdict for the exact tip, from a reviewer who is not the worker.

Verdict comments are inserted straight into the store where a test needs one
the comment gate (WI-1319) would refuse: the land re-checks the reviewer's
provenance itself and must not rely on that gate.
"""

from __future__ import annotations

import dataclasses
import json
from datetime import UTC, datetime
from typing import Any, Literal

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    GetWorkParams,
    LandWorkParams,
    ListAuditParams,
    ListEventsParams,
    RegisterProjectParams,
    UpdateProjectParams,
)
from vogt.application.services import (
    comment_work,
    get_work,
    land_work,
    list_audit,
    list_events,
    register_project,
    update_project,
)
from vogt.core.entities import (
    AgentPolicy,
    CodingSession,
    Comment,
    HandoffFields,
    LandingPolicy,
    VerdictFields,
)
from vogt.core.principal import Principal
from vogt.errors import InvalidRequest, LandRefused

from tests.conftest import native_work_item
from tests.test_sessions import ROOT, StandInEngine

WHY = "land test"
TIP = "abc1234def"


class LandingEngine(StandInEngine):
    """Answers `land` the way the engine does (WI-1341)."""

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if method == "POST" and url.endswith("/api/git/op"):
            payload = json.loads(body.decode("utf-8"))
            if payload.get("op") == "land" and self.git_refusal is None:
                self.sent.append({"url": url, "method": method, "body": payload})
                return 200, json.dumps(
                    {
                        "ok": True,
                        "onto_before": "base0000",
                        "onto_after": "land2222",
                        "commits": ["land1111", "land2222"],
                        "rebased": True,
                    }
                ).encode()
        return super().__call__(url, headers, body, method)


@pytest.fixture
def engine() -> LandingEngine:
    return LandingEngine()


def _policy(merge_by: Literal["oversight", "operator"] = "oversight") -> AgentPolicy:
    return AgentPolicy(
        forbidden_text=["do-not-ship"],
        landing=LandingPolicy(
            base_branch="main",
            worker_lands_by="push_branch",
            merge_by=merge_by,
            merge_mode="rebase",
        ),
    )


@pytest.fixture
def wired(instance: AppContext, engine: LandingEngine) -> tuple[AppContext, str, str]:
    """An oversight-merged project and an item a worker handed off."""
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    update_project(
        ctx, UpdateProjectParams(slug="vogt", agent_policy=_policy(), reason=WHY)
    )
    item = native_work_item(ctx, title="Land me", project="vogt")
    comment_work(
        ctx,
        CommentParams(
            ref=item.ref,
            body="done",
            kind="handoff",
            handoff=HandoffFields(
                branch="wi-1-land", head_sha=TIP, session="eng-ses_worker"
            ),
            reason=WHY,
        ),
    )
    with ctx.declared.read() as view:
        row = view.work_item_by_ref(item.ref)
    assert row is not None
    return ctx, item.ref, row.id


def _actor_of(ctx: AppContext) -> str:
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(ctx.principal.identity_ref)
    assert actor is not None
    return actor.id


def _session(
    ctx: AppContext,
    item_id: str,
    *,
    sid: str,
    purpose: str | None = "review",
    started_by: Literal["person", "oversight", "worker"] | None = "oversight",
) -> AppContext:
    """Record a session row; return a context acting as that session."""
    with ctx.declared.read() as view:
        project = view.project_by_slug("vogt")
    assert project is not None
    with ctx.declared.write() as txn:
        txn.insert_session(
            CodingSession(
                id=sid,
                engine_session_id=f"eng-{sid}",
                project_id=project.id,
                work_item_id=item_id,
                actor_id=_actor_of(ctx),
                cwd=str(ROOT),
                reason=WHY,
                started_at=datetime(2026, 10, 30, tzinfo=UTC),
                purpose=purpose,
                started_by_kind=started_by,
            )
        )
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=f"agent:session:{sid}", kind="agent", display_name=sid
        ),
    )


def _verdict(ctx: AppContext, item_id: str, **fields: Any) -> None:
    """Store a verdict as-is, past the comment gate."""
    base: dict[str, Any] = {"head_sha": TIP, "verdict": "no_blockers"}
    base.update(fields)
    with ctx.declared.write() as txn:
        txn.insert_comment(
            Comment(
                id=ctx.id_factory("cmt"),
                work_item_id=item_id,
                actor_id=_actor_of(ctx),
                actor_display_name="reviewer",
                body="review",
                created_at=ctx.clock(),
                kind="verdict",
                verdict=VerdictFields.model_validate(base),
            )
        )


def _overseer(ctx: AppContext, engine: LandingEngine, item_id: str) -> AppContext:
    caller = _session(ctx, item_id, sid="ses_over", purpose=None)
    engine.roles["eng-ses_over"] = "oversight"
    return caller


def _land(ctx: AppContext, ref: str, head: str = TIP) -> Any:
    return land_work(ctx, LandWorkParams(ref=ref, head_sha=head, reason=WHY))


def _reviewed(
    wired: tuple[AppContext, str, str], **session: Any
) -> tuple[AppContext, str, str]:
    ctx, ref, item_id = wired
    _session(ctx, item_id, sid="ses_rev", **session)
    _verdict(ctx, item_id, reviewer_session="eng-ses_rev")
    return ctx, ref, item_id


def _lands(engine: LandingEngine) -> list[dict[str, Any]]:
    return [
        row["body"]
        for row in engine.sent
        if row["url"].endswith("/api/git/op") and row["body"].get("op") == "land"
    ]


# -- success --------------------------------------------------------------


def test_an_overseer_lands_at_an_oversight_reviewers_verdict(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = _reviewed(wired)
    result = _land(_overseer(ctx, engine, item_id), ref)
    assert result.onto_after == "land2222"
    assert result.commits == ["land1111", "land2222"]
    assert result.rebased is True
    assert _lands(engine) == [
        {
            "op": "land",
            "repo": ROOT,
            "branch": "wi-1-land",
            "head_sha": TIP,
            "onto": "main",
            "mode": "rebase",
            "forbidden_text": ["do-not-ship"],
            "linear_history": True,
        }
    ]
    notes = [c.body for c in get_work(ctx, GetWorkParams(ref=ref)).comments]
    assert "landed land2222 onto main (2 commits; verdict by eng-ses_rev)" in notes
    events = [
        e
        for e in list_events(ctx, ListEventsParams()).events
        if e.kind == "work.landed"
    ]
    assert events[-1].summary["reviewer_session"] == "eng-ses_rev"
    assert list_audit(ctx, ListAuditParams(operation="work.land")).records


def test_a_person_lands_at_a_persons_verdict(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = wired
    _verdict(ctx, item_id)
    assert _land(ctx, ref).onto_after == "land2222"


# -- refusals -------------------------------------------------------------


def _refused(ctx: AppContext, ref: str, rule: str, head: str = TIP) -> None:
    with pytest.raises(LandRefused, match=rule):
        _land(ctx, ref, head)
    audit = list_audit(ctx, ListAuditParams(operation="work.land")).records
    assert audit, "a refusal is audited"
    refused = [
        e
        for e in list_events(ctx, ListEventsParams()).events
        if e.kind == "work.land_refused"
    ]
    assert refused[-1].summary["refused"]


def test_a_worker_session_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = _reviewed(wired)
    worker = _session(ctx, item_id, sid="ses_worker", purpose="implement")
    _refused(worker, ref, r"lands a branch \(eng-ses_worker")
    assert _lands(engine) == []


def test_an_operator_merged_project_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = _reviewed(wired)
    update_project(
        ctx,
        UpdateProjectParams(
            slug="vogt", agent_policy=_policy(merge_by="operator"), reason=WHY
        ),
    )
    _refused(_overseer(ctx, engine, item_id), ref, "merges are the operator's")


def test_no_branch_is_refused(instance: AppContext, engine: LandingEngine) -> None:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    update_project(
        ctx, UpdateProjectParams(slug="vogt", agent_policy=_policy(), reason=WHY)
    )
    item = native_work_item(ctx, title="No branch", project="vogt")
    _refused(ctx, item.ref, "no hand-off names a branch")


def test_a_stale_sha_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = _reviewed(wired)
    _refused(_overseer(ctx, engine, item_id), ref, "branch moved", head="fffffff1")


def test_a_blockers_verdict_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = wired
    _verdict(ctx, item_id, verdict="blockers", blockers=["tests"])
    _refused(ctx, ref, "has blockers")


def test_the_workers_own_verdict_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, item_id = wired
    _session(ctx, item_id, sid="ses_worker")
    _verdict(ctx, item_id, reviewer_session="eng-ses_worker")
    _refused(ctx, ref, "the worker's own")


def test_a_worker_started_reviewer_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, _ = _reviewed(wired, started_by="worker")
    _refused(ctx, ref, "was started by worker")


def test_a_reviewer_of_another_item_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, _ = wired
    other = native_work_item(ctx, title="Other", project="vogt")
    with ctx.declared.read() as view:
        other_row = view.work_item_by_ref(other.ref)
    assert other_row is not None
    _session(ctx, other_row.id, sid="ses_rev")
    with ctx.declared.read() as view:
        item = view.work_item_by_ref(ref)
    assert item is not None
    _verdict(ctx, item.id, reviewer_session="eng-ses_rev")
    _refused(ctx, ref, "not bound to")


def test_a_non_review_session_verdict_is_refused(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, _ = _reviewed(wired, purpose="implement")
    _refused(ctx, ref, "is not a review")


def test_the_engines_refusal_is_surfaced_verbatim(
    wired: tuple[AppContext, str, str], engine: LandingEngine
) -> None:
    ctx, ref, _ = _reviewed(wired)
    engine.git_refusal = (400, "wi-1-land moved: tip is 999 not abc1234def")
    with pytest.raises(InvalidRequest, match="tip is 999 not abc1234def"):
        _land(ctx, ref)
    notes = [c.body for c in get_work(ctx, GetWorkParams(ref=ref)).comments]
    assert not any(n.startswith("landed ") for n in notes)
