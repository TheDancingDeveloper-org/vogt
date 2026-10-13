"""Hand-off comments (design C11).

A hand-off is a comment with a kind and structured fields, not a new entity.
A fresh session starts from the latest one; an upstream item cannot carry one,
because the forge thread has no kind to keep.
"""

from __future__ import annotations

import dataclasses
from pathlib import Path

import pytest
from pydantic import ValidationError

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    ForgeLinkParams,
    GetWorkParams,
    OnboardParams,
    RegisterProjectParams,
    SetWriteBackParams,
    StartSessionParams,
)
from vogt.application.services import (
    comment_work,
    get_work,
    link_project,
    onboard,
    register_project,
    set_write_back,
    start_session,
)
from vogt.application.services._brief import (
    brief_for_work_item,
    review_brief_for_work_item,
)
from vogt.core.entities import HandoffFields
from vogt.errors import InvalidRequest

from tests.conftest import native_work_item
from tests.test_sessions import ROOT, StandInEngine
from tests.test_upstream_truth import REPO, RecordingForge, _instance

WHY = "hand-off test"


def _fields(**overrides: object) -> HandoffFields:
    base: dict[str, object] = {
        "branch": "wi-1244-wave2",
        "head_sha": "abc123",
        "done": ["kind round-trips"],
        "remaining": ["phase 2"],
    }
    base.update(overrides)
    return HandoffFields.model_validate(base)


def test_a_handoff_round_trips_and_a_note_stays_a_note(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(instance, title="Hand-offs", project="vogt")

    noted = comment_work(
        instance, CommentParams(ref=item.ref, body="a remark", reason=WHY)
    )
    handed = comment_work(
        instance,
        CommentParams(
            ref=item.ref,
            body="stopped after the storage write",
            kind="handoff",
            handoff=_fields(),
            reason=WHY,
        ),
    )

    assert noted.comment.kind == "note"
    assert noted.comment.handoff is None
    assert handed.comment.kind == "handoff"
    assert handed.comment.handoff is not None
    assert handed.comment.handoff.branch == "wi-1244-wave2"
    assert handed.comment.handoff.done == ["kind round-trips"]

    got = get_work(instance, GetWorkParams(ref=item.ref))
    by_body = {one.body: one for one in got.comments}
    assert by_body["a remark"].kind == "note"
    assert by_body["stopped after the storage write"].handoff is not None
    handed_back = by_body["stopped after the storage write"].handoff
    assert handed_back is not None
    assert handed_back.head_sha == "abc123"


def test_a_handoff_without_fields_is_refused_and_fields_on_a_note_are_too() -> None:
    with pytest.raises(ValidationError, match="requires handoff fields"):
        CommentParams(ref="WI-1", body="nothing structured", kind="handoff", reason=WHY)
    with pytest.raises(ValidationError, match="belong on a comment of kind handoff"):
        CommentParams(ref="WI-1", body="a note", handoff=_fields(), reason=WHY)


def test_comment_order_keeps_one_end_of_the_thread(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(instance, title="A long thread", project="vogt")
    for n in range(5):
        comment_work(
            instance, CommentParams(ref=item.ref, body=f"note {n}", reason=WHY)
        )

    newest = get_work(
        instance,
        GetWorkParams(ref=item.ref, comment_limit=2, comment_order="newest"),
    )
    oldest = get_work(
        instance,
        GetWorkParams(ref=item.ref, comment_limit=2, comment_order="oldest"),
    )
    assert [one.body for one in newest.comments] == ["note 4", "note 3"]
    assert [one.body for one in oldest.comments] == ["note 0", "note 1"]


def test_the_brief_leads_with_the_latest_handoff(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(instance, title="Brief me", project="vogt")
    comment_work(
        instance,
        CommentParams(
            ref=item.ref,
            body="first stop",
            kind="handoff",
            handoff=_fields(branch="old-branch", remaining=["superseded"]),
            reason=WHY,
        ),
    )
    comment_work(
        instance, CommentParams(ref=item.ref, body="a later remark", reason=WHY)
    )
    comment_work(
        instance,
        CommentParams(
            ref=item.ref,
            body="second stop, the one that counts",
            kind="handoff",
            handoff=_fields(branch="new-branch", remaining=["the rest"]),
            reason=WHY,
        ),
    )

    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        brief = brief_for_work_item(view, stored, "ses-1").text

    assert "## Hand-off" in brief
    assert brief.index("## Hand-off") < brief.index("## Comments")
    assert "new-branch" in brief
    assert "second stop, the one that counts" in brief
    assert "old-branch" not in brief
    assert "a later remark" in brief


def test_from_handoff_opens_on_the_item(instance: AppContext) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title="Resume me", project="vogt")
    comment_work(
        ctx,
        CommentParams(
            ref=item.ref,
            body="where I stopped",
            kind="handoff",
            handoff=_fields(),
            reason=WHY,
        ),
    )

    started = start_session(ctx, StartSessionParams(from_handoff=item.ref, reason=WHY))
    assert started.session.work_item == item.ref
    assert "## Hand-off" in engine.last_spec["prompt"]
    assert "where I stopped" in engine.last_spec["prompt"]


def test_from_handoff_refuses_a_different_work_item(instance: AppContext) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    first = native_work_item(ctx, title="One", project="vogt")
    second = native_work_item(ctx, title="Two", project="vogt")
    comment_work(
        ctx,
        CommentParams(
            ref=first.ref, body="stopped", kind="handoff", handoff=_fields(), reason=WHY
        ),
    )

    with pytest.raises(InvalidRequest, match="work_item names"):
        start_session(
            ctx,
            StartSessionParams(
                from_handoff=first.ref, work_item=second.ref, reason=WHY
            ),
        )
    assert engine.sent == []


def test_from_handoff_refuses_an_item_with_no_handoff(instance: AppContext) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title="Bare", project="vogt")
    comment_work(ctx, CommentParams(ref=item.ref, body="only a note", reason=WHY))

    with pytest.raises(InvalidRequest, match="no hand-off"):
        start_session(ctx, StartSessionParams(from_handoff=item.ref, reason=WHY))
    assert engine.sent == []


def test_a_handoff_on_an_upstream_item_is_refused(tmp_path: Path) -> None:
    forge = RecordingForge()
    forge.issues.append(
        {
            "number": 1,
            "title": "Upstream",
            "state": "open",
            "labels": [],
            "updated_at": "2026-08-01T00:00:01Z",
            "html_url": f"{REPO}/issues/1",
        }
    )
    ctx = _instance(tmp_path, forge)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    onboard(ctx, OnboardParams(project="demo", reason=WHY))
    forge.requests.clear()

    with pytest.raises(InvalidRequest, match="upstream item"):
        comment_work(
            ctx,
            CommentParams(
                ref="gh:acme/demo#1",
                body="stopping",
                kind="handoff",
                handoff=_fields(),
                reason=WHY,
            ),
        )
    assert forge.mutations == []


def test_a_review_brief_checks_the_diff_against_the_items_own_criteria(
    instance: AppContext,
) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(
        instance,
        title="Brief me",
        project="vogt",
        body="Done means the brief names the diff and the acceptance criteria.",
    )
    comment_work(
        instance,
        CommentParams(ref=item.ref, body="a remark, not the review", reason=WHY),
    )
    comment_work(
        instance,
        CommentParams(
            ref=item.ref,
            body="I finished the composer",
            kind="handoff",
            handoff=_fields(branch="wi-1299-review-brief", base="integrate/vogt-next"),
            reason=WHY,
        ),
    )

    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        worker = brief_for_work_item(view, stored, "ses-1").text
        review = review_brief_for_work_item(view, stored, "ses-1").text

    assert review.startswith("# Review ")
    assert "## Done means" in review
    assert "the brief names the diff" in review
    assert "## The diff" in review
    assert "wi-1299-review-brief" in review
    assert "integrate/vogt-next" in review
    assert "git diff" in review
    # The worker's claim is quoted as a claim. The remark thread and the
    # ranking — both of which read as "carry on" — are not the review.
    assert "I finished the composer" in review
    assert "a remark, not the review" not in review
    assert "## Why this is ranked" not in review
    assert "## Comments" not in review
    assert "## Hand-off" in worker and "## The diff" not in worker


def test_a_review_with_no_handoff_says_there_is_no_diff(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(instance, title="Bare", project="vogt", body="")

    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        review = review_brief_for_work_item(view, stored, "ses-1").text

    assert "no hand-off" in review
    assert "states no acceptance criteria" in review
    assert "git diff" not in review


def test_review_of_opens_a_session_on_the_review_brief(instance: AppContext) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(
        ctx, title="Review me", project="vogt", body="Done means it builds."
    )
    comment_work(
        ctx,
        CommentParams(
            ref=item.ref,
            body="ready",
            kind="handoff",
            handoff=_fields(base="main"),
            reason=WHY,
        ),
    )

    started = start_session(
        ctx,
        StartSessionParams(review_of=item.ref, task="watch the migration", reason=WHY),
    )
    assert started.session.work_item == item.ref
    prompt = engine.last_spec["prompt"]
    assert "# Review " in prompt
    assert "## The diff" in prompt
    assert "## Task" not in prompt
    assert "## Also asked" in prompt
    assert "watch the migration" in prompt


def test_review_of_refuses_a_different_item(instance: AppContext) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    first = native_work_item(ctx, title="One", project="vogt")
    second = native_work_item(ctx, title="Two", project="vogt")

    with pytest.raises(InvalidRequest, match="work_item names"):
        start_session(
            ctx,
            StartSessionParams(review_of=first.ref, work_item=second.ref, reason=WHY),
        )
    assert engine.sent == []
