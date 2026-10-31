"""Hand-off comments (design C11).

A hand-off is a comment with a kind and structured fields, not a new entity.
A fresh session starts from the latest one; an upstream item cannot carry one,
because the forge thread has no kind to keep.
"""

from __future__ import annotations

import dataclasses
import json
from datetime import UTC, datetime
from pathlib import Path
from typing import Literal

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
from vogt.core.entities import (
    AgentPolicy,
    CodingSession,
    Comment,
    HandoffFields,
    Project,
    VerdictFields,
    VerificationFields,
)
from vogt.core.principal import Principal
from vogt.errors import CommentRefused, InvalidRequest

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
    comment_work(
        instance, CommentParams(ref=item.ref, body="after the hand-off", reason=WHY)
    )

    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        brief = brief_for_work_item(view, stored, "ses-1").text

    assert "## Latest hand-off (" in brief
    assert "— a claim, not evidence" in brief
    assert "new-branch" in brief
    assert "second stop, the one that counts" in brief
    assert "old-branch" not in brief
    assert "1 earlier hand-off omitted." in brief
    # The fixture's notes are a person's, so they are decisions, newest first,
    # and a decision is not superseded by a later hand-off (WI-1320).
    assert brief.index("after the hand-off") < brief.index("a later remark")
    assert brief.index("## Decisions") < brief.index("## Latest hand-off")


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
    assert "## Latest hand-off (" in engine.last_spec["prompt"]
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
    # The real base, and the tip resolved against the hand-off head.
    assert "`git diff integrate/vogt-next...<tip>`" in review
    assert "hand-off head abc123 · reviewed tip <tip>" in review
    assert "<base>" not in review and "<head>" not in review
    # The worker's claim is quoted as a claim. The remark thread and the
    # ranking — both of which read as "carry on" — are not the review.
    assert "I finished the composer" in review
    assert "a remark, not the review" not in review
    assert "## Why this is ranked" not in review
    assert "## Comments" not in review
    assert "## Latest hand-off (" in worker and "## The diff" not in worker


def _review_of(
    instance: AppContext,
    *,
    project: Project | None = None,
    **fields: object,
) -> str:
    item = native_work_item(
        instance, title="Review me", project="vogt", body="Done means X."
    )
    comment_work(
        instance,
        CommentParams(
            ref=item.ref,
            body="all green, trust me",
            kind="handoff",
            handoff=_fields(**fields),
            reason=WHY,
        ),
    )
    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        return review_brief_for_work_item(view, stored, "ses-1", project=project).text


def _policied(**policy: object) -> Project:
    return Project(
        id="p1",
        slug="vogt",
        name="Vogt",
        root_path=ROOT,
        created_at=datetime(2026, 10, 13, tzinfo=UTC),
        updated_at=datetime(2026, 10, 13, tzinfo=UTC),
        agent_policy=AgentPolicy.model_validate(policy),
    )


def test_a_review_brief_states_the_verdict_format(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    review = _review_of(instance, base="main")

    assert "`no blockers (head <sha>)`" in review
    assert "`blockers (head <sha>):` followed by a numbered list" in review
    assert "which deployment paths the tests cover" in review
    assert "Do not write `no blockers` while describing a regression" in review


def test_a_review_brief_is_read_only_and_carries_no_worker_landing_policy(
    instance: AppContext,
) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    project = _policied(
        commit_identity="sprooty <sprooty@sprooty.com>",
        test_recipe="uv run pytest",
        rules=["Commit after every step and push after every commit."],
    )
    review = _review_of(instance, project=project, base="main")

    assert "This review is read-only" in review
    for forbidden in ("edits", "commits, pushes, merges or deploys"):
        assert forbidden in review
    # The worker's landing rules are not the reviewer's.
    assert "## Project policy" not in review
    assert "Commit after every step" not in review
    assert "**Commit identity**" not in review
    # The gates are still useful to a reviewer.
    assert "- **Tests** uv run pytest" in review


def test_every_handoff_field_is_rendered_as_a_claim(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    review = _review_of(
        instance,
        base="main",
        done=["composer done"],
        remaining=["phase 2"],
        gates_run=["pytest green"],
        blockers=["none really"],
    )

    claims = review[review.index("## What the worker claims") :]
    assert "a claim, not evidence" in claims
    for value in ("composer done", "phase 2", "pytest green", "none really"):
        assert f"claimed, not verified: {value}" in claims
        # Nowhere outside the claims section.
        assert value not in review[: review.index("## What the worker claims")]
    assert "all green, trust me" in claims


def test_a_review_brief_fills_in_the_real_diff_command(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    pinned = _review_of(instance, branch="wi-9-x", head_sha="deadbeef", base="b0")
    assert "`git diff b0...<tip>`" in pinned
    assert "`git fetch origin wi-9-x`" in pinned
    assert "`git rev-parse origin/wi-9-x`" in pinned
    # A branch that moved past the hand-off is reviewed at its tip, with the
    # hand-off head shown beside it, never the stale head alone (WI-1837).
    assert "review the tip, not `deadbeef`" in pinned
    assert "`git log deadbeef..<tip>`" in pinned
    assert "hand-off head deadbeef · reviewed tip <tip>" in pinned
    assert "review `deadbeef` and say the branch has moved" not in pinned

    unpinned = _review_of(instance, branch="wi-9-y", head_sha=None, base="b0")
    assert "`git diff b0...<tip>`" in unpinned
    assert "`git rev-parse origin/wi-9-y`" in unpinned
    assert "The hand-off names no head sha" in unpinned


def test_a_review_brief_renders_the_deployed_setup(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    split = "AWS runs the sessions service as a separate container."
    with_notes = _review_of(
        instance, project=_policied(deployed_setup=split), base="main"
    )
    assert "## Deployed setup" in with_notes
    assert split in with_notes

    without = _review_of(instance, project=_policied(), base="main")
    assert "## Deployed setup" in without
    assert "records no deployed-setup notes" in without


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
        StartSessionParams(
            review_of=item.ref,
            template="claude",
            task="watch the migration",
            reason=WHY,
        ),
    )
    assert started.session.work_item == item.ref
    prompt = engine.last_spec["prompt"]
    assert "# Review " in prompt
    assert "## The diff" in prompt
    # The launch prompt waits without a Task section, so the review has one;
    # the caller's own task stays under "Also asked" (WI-1854).
    task = prompt.index("## Task")
    assert "Review the branch" in prompt[task:]
    assert "watch the migration" not in prompt[task : prompt.index("## Also asked")]
    assert "watch the migration" in prompt[prompt.index("## Also asked") :]


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


# -- verdicts and verifications (WI-1319) ------------------------------------


def _verdict(**overrides: object) -> VerdictFields:
    base: dict[str, object] = {"head_sha": "abc123", "verdict": "no_blockers"}
    base.update(overrides)
    return VerdictFields.model_validate(base)


def _check() -> VerificationFields:
    return VerificationFields(lane="dev", what="login works", evidence="curl 200")


def _session(
    ctx: AppContext,
    item_id: str,
    *,
    sid: str,
    purpose: str | None = "review",
    started_by: Literal["person", "oversight", "worker"] | None = "oversight",
    engine_id: str | None = None,
) -> AppContext:
    """Record a session row and return a context acting as that session."""
    with ctx.declared.read() as view:
        project = view.project_by_slug("vogt")
    assert project is not None
    with ctx.declared.write() as txn:
        txn.insert_session(
            CodingSession(
                id=sid,
                engine_session_id=f"eng-{sid}" if engine_id is None else engine_id,
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


def _actor_of(ctx: AppContext) -> str:
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(ctx.principal.identity_ref)
    assert actor is not None
    return actor.id


def _item_with_worker(ctx: AppContext, title: str = "Verdicts") -> tuple[str, str]:
    """An item whose latest hand-off names worker session `eng-ses_worker`."""
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(ctx, title=title, project="vogt")
    handed = comment_work(
        ctx,
        CommentParams(
            ref=item.ref,
            body="done",
            kind="handoff",
            handoff=_fields(session="eng-ses_worker"),
            reason=WHY,
        ),
    )
    return item.ref, handed.comment.work_item_id


def _post_verdict(ctx: AppContext, ref: str, **fields: object) -> Comment:
    return comment_work(
        ctx,
        CommentParams(
            ref=ref,
            body="review",
            kind="verdict",
            verdict=_verdict(**fields),
            reason=WHY,
        ),
    ).comment


def test_a_person_posts_a_verdict(instance: AppContext) -> None:
    ref, _ = _item_with_worker(instance)
    posted = _post_verdict(instance, ref)
    assert posted.verdict is not None
    assert posted.verdict.reviewer_started_by == "person"
    read = get_work(instance, GetWorkParams(ref=ref)).comments
    assert any(c.kind == "verdict" and c.verdict == posted.verdict for c in read)


def test_an_oversight_review_session_posts_on_its_own_item(
    instance: AppContext,
) -> None:
    ref, item_id = _item_with_worker(instance)
    reviewer = _session(instance, item_id, sid="ses_rev")
    posted = _post_verdict(
        reviewer,
        ref,
        reviewer_session="forged",
        reviewer_started_by="person",
    )
    assert posted.verdict is not None
    assert posted.verdict.reviewer_session == "eng-ses_rev"
    assert posted.verdict.reviewer_started_by == "oversight"


def test_a_worker_started_review_session_is_refused(instance: AppContext) -> None:
    ref, item_id = _item_with_worker(instance)
    reviewer = _session(instance, item_id, sid="ses_rev", started_by="worker")
    with pytest.raises(CommentRefused, match="started by worker"):
        _post_verdict(reviewer, ref)


def test_a_review_of_another_item_is_refused(instance: AppContext) -> None:
    ref, _ = _item_with_worker(instance)
    other = native_work_item(instance, title="Other", project="vogt")
    with instance.declared.read() as view:
        other_row = view.work_item_by_ref(other.ref)
    assert other_row is not None
    reviewer = _session(instance, other_row.id, sid="ses_rev")
    with pytest.raises(CommentRefused, match="not bound to"):
        _post_verdict(reviewer, ref)


def test_the_worker_session_is_refused_on_both_kinds(instance: AppContext) -> None:
    ref, item_id = _item_with_worker(instance)
    worker = _session(instance, item_id, sid="ses_worker")
    with pytest.raises(CommentRefused, match="the worker"):
        _post_verdict(worker, ref)
    with pytest.raises(CommentRefused, match="other than the worker"):
        comment_work(
            worker,
            CommentParams(
                ref=ref,
                body="checked",
                kind="verification",
                verification=_check(),
                reason=WHY,
            ),
        )


def test_a_non_review_session_is_refused(instance: AppContext) -> None:
    ref, item_id = _item_with_worker(instance)
    plain = _session(instance, item_id, sid="ses_plain", purpose="implement")
    with pytest.raises(CommentRefused, match="not a review"):
        _post_verdict(plain, ref)


def test_a_verification_from_another_session_or_a_person_stores(
    instance: AppContext,
) -> None:
    ref, item_id = _item_with_worker(instance)
    other = _session(instance, item_id, sid="ses_qa", purpose=None, started_by=None)
    for ctx, by in ((other, "eng-ses_qa"), (instance, None)):
        posted = comment_work(
            ctx,
            CommentParams(
                ref=ref,
                body="checked",
                kind="verification",
                verification=_check(),
                reason=WHY,
            ),
        ).comment
        assert posted.verification is not None
        if by is not None:
            assert posted.verification.by == by
        else:
            assert posted.verification.by == instance.principal.identity_ref
    kinds = [c.kind for c in get_work(instance, GetWorkParams(ref=ref)).comments]
    assert kinds.count("verification") == 2


def test_verdict_field_rules() -> None:
    with pytest.raises(ValidationError, match="at least one blocker"):
        VerdictFields(head_sha="a", verdict="blockers")
    with pytest.raises(ValidationError, match="requires verdict fields"):
        CommentParams(ref="WI-1", body="x", kind="verdict", reason=WHY)
    with pytest.raises(ValidationError, match="kind verification"):
        CommentParams(
            ref="WI-1", body="x", kind="note", verification=_check(), reason=WHY
        )


# -- what the worker brief carries from the thread (WI-1320) ----------------


class _Thread:
    """A read view whose thread for every item is the given comments."""

    def __init__(self, view: object, comments: list[Comment]) -> None:
        self._view = view
        self._comments = sorted(comments, key=lambda c: c.created_at, reverse=True)

    def comments_for(self, *_: object, **__: object) -> list[Comment]:
        return self._comments

    def __getattr__(self, name: str) -> object:
        return getattr(self._view, name)


def _at(day: int) -> datetime:
    return datetime(2026, 10, day, tzinfo=UTC)


def _comment(day: int, body: str, *, by: str = "person", **fields: object) -> Comment:
    return Comment.model_validate(
        {
            "id": f"c-{day}-{body}",
            "work_item_id": "wi",
            "actor_id": by,
            "actor_display_name": by,
            "body": body,
            "created_at": _at(day),
            **fields,
        }
    )


def _handoff_c(day: int, branch: str) -> Comment:
    return _comment(
        day, f"stop {branch}", kind="handoff", handoff=_fields(branch=branch)
    )


def _verdict_c(day: int, head: str, *blockers: str) -> Comment:
    verdict = "blockers" if blockers else "no_blockers"
    return _comment(
        day,
        f"review {head}",
        kind="verdict",
        verdict=_verdict(head_sha=head, verdict=verdict, blockers=list(blockers)),
    )


def _render(instance: AppContext, comments: list[Comment]) -> str:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    item = native_work_item(instance, title="Thread", project="vogt", body="the body")
    with instance.declared.read() as view:
        stored = view.work_item_by_ref(item.ref)
        assert stored is not None
        thread = _Thread(view, comments)
        return brief_for_work_item(
            thread,  # type: ignore[arg-type]
            stored,
            "ses-1",
            decision_author=lambda c: c.actor_id in ("person", "oversight"),
        ).text


def test_a_brief_with_no_thread_has_no_thread_headings(instance: AppContext) -> None:
    brief = _render(instance, [])
    assert "the body" in brief
    for heading in ("## Decisions", "## Latest hand-off", "## Latest review"):
        assert heading not in brief


def test_a_brief_drops_worker_notes_and_leaves_no_empty_heading(
    instance: AppContext,
) -> None:
    brief = _render(
        instance, [_comment(n, f"worker note {n}", by="worker") for n in (1, 2)]
    )
    assert "worker note" not in brief
    assert "## Decisions" not in brief and "### Note" not in brief


def test_a_brief_with_only_a_handoff(instance: AppContext) -> None:
    brief = _render(instance, [_handoff_c(3, "only-branch")])
    assert "## Latest hand-off (2026-10-03, person) — a claim, not evidence" in brief
    assert "- **branch** only-branch" in brief
    assert "## Latest review verdict" not in brief
    assert "omitted" not in brief


def test_a_brief_with_only_a_verdict(instance: AppContext) -> None:
    brief = _render(instance, [_verdict_c(4, "f00d", "tests missing", "no docs")])
    assert "## Latest review verdict (2026-10-04, person, head f00d)" in brief
    assert "1. tests missing\n2. no docs" in brief
    assert "## Latest hand-off" not in brief
    assert "predates" not in brief


def test_the_latest_of_each_wins(instance: AppContext) -> None:
    brief = _render(
        instance,
        [
            _handoff_c(1, "h-one"),
            _handoff_c(3, "h-two"),
            _handoff_c(5, "h-three"),
            _verdict_c(2, "v-one", "old blocker"),
            _verdict_c(6, "v-two"),
        ],
    )
    assert brief.count("## Latest hand-off") == 1
    assert brief.count("## Latest review verdict") == 1
    assert "h-three" in brief and "h-one" not in brief and "h-two" not in brief
    assert "head v-two" in brief and "old blocker" not in brief
    assert "No blockers." in brief
    assert "2 earlier hand-offs omitted." in brief
    assert "1 earlier verdict omitted." in brief
    assert "predates" not in brief


def test_a_verdict_older_than_the_latest_handoff_says_so(
    instance: AppContext,
) -> None:
    brief = _render(instance, [_verdict_c(2, "old-head"), _handoff_c(3, "newer")])
    verdict = brief.index(
        "## Latest review verdict (2026-10-02, person, head old-head)"
    )
    assert brief.index("## Latest hand-off") < verdict
    assert "This verdict predates the latest hand-off" in brief[verdict:]


def test_decision_notes_are_newest_first_capped_and_labelled(
    instance: AppContext,
) -> None:
    notes = [_comment(n, f"decision {n}") for n in range(1, 8)]
    notes.append(_comment(9, "x" * 2000, by="oversight"))
    brief = _render(instance, notes)
    assert "### Note (2026-10-09, oversight)" in brief
    assert "x" * 1500 + "…" in brief and "x" * 1501 not in brief
    # Five notes in all: the oversight note and the four newest person notes.
    assert brief.count("### Note (") == 5
    assert brief.index("decision 7") < brief.index("decision 4")
    assert "decision 3" not in brief


def test_the_brief_carries_decisions_not_the_worker_thread(
    instance: AppContext,
) -> None:
    """The Done-means case, end to end through `session.start`."""
    engine = StandInEngine()
    engine.roles["eng-ses_ov"] = "oversight"
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="Decided")
    overseer = _session(ctx, item_id, sid="ses_ov", purpose="oversight")
    worker = _session(ctx, item_id, sid="ses_wk", purpose="implement")
    for n in (1, 2):
        comment_work(
            ctx,
            CommentParams(
                ref=ref,
                body=f"stop {n}",
                kind="handoff",
                handoff=_fields(branch=f"hand-{n}"),
                reason=WHY,
            ),
        )
    _post_verdict(ctx, ref, head_sha="first-head")
    _post_verdict(ctx, ref, head_sha="second-head")
    comment_work(ctx, CommentParams(ref=ref, body="person decides", reason=WHY))
    comment_work(overseer, CommentParams(ref=ref, body="oversight relays", reason=WHY))
    for n in range(4):
        comment_work(worker, CommentParams(ref=ref, body=f"worker {n}", reason=WHY))

    start_session(ctx, StartSessionParams(work_item=ref, reason=WHY))
    brief = engine.last_spec["prompt"]
    assert brief.count("## Latest hand-off") == 1 and "hand-2" in brief
    assert "hand-1" not in brief
    assert brief.count("## Latest review verdict") == 1
    assert "head second-head" in brief and "first-head" not in brief
    assert brief.count("### Note (") == 2
    assert brief.index("oversight relays") < brief.index("person decides")
    assert "worker 0" not in brief and "worker 3" not in brief


class _Refusing(StandInEngine):
    """An engine that refuses the role lookup for the named sessions only."""

    def __init__(self, refused: set[str], status: int, said: str) -> None:
        super().__init__()
        self.refused = refused
        self.status = status
        self.said = said

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if (
            method == "GET"
            and "/api/sessions/" in url
            and url.rsplit("/", 1)[-1] in self.refused
        ):
            self.sent.append({"url": url, "method": method, "body": {}})
            return self.status, json.dumps({"error": self.said}).encode()
        return super().__call__(url, headers, body, method)


def _role_lookups(engine: StandInEngine, engine_id: str) -> int:
    """How many role reads the engine was asked for one session."""
    return len(
        [
            row
            for row in engine.sent
            if row["method"] == "GET" and row["url"].endswith(f"/{engine_id}")
        ]
    )


def _brief_from_wired_start(ctx: AppContext, engine: StandInEngine, ref: str) -> str:
    start_session(ctx, StartSessionParams(work_item=ref, reason=WHY))
    return str(engine.last_spec["prompt"])


@pytest.mark.parametrize(
    ("status", "said"),
    [
        (503, "the engine is not answering"),
        (403, "forbidden: project not open to grants"),
        (400, "no such session"),
    ],
)
def test_a_refused_role_lookup_drops_the_note_and_the_start_goes_ahead(
    instance: AppContext, status: int, said: str
) -> None:
    engine = _Refusing({"eng-ses_ov"}, status, said)
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title=f"Refused {status}")
    overseer = _session(ctx, item_id, sid="ses_ov", purpose="oversight")
    comment_work(overseer, CommentParams(ref=ref, body="oversight relays", reason=WHY))
    comment_work(ctx, CommentParams(ref=ref, body="person decides", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert "person decides" in brief
    assert "oversight relays" not in brief
    assert brief.count("### Note (") == 1


def test_a_session_the_engine_no_longer_holds_is_a_worker(
    instance: AppContext,
) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="Removed")
    gone = _session(ctx, item_id, sid="ses_gone", purpose="oversight")
    comment_work(gone, CommentParams(ref=ref, body="gone relays", reason=WHY))
    comment_work(ctx, CommentParams(ref=ref, body="person decides", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert "gone relays" not in brief and "person decides" in brief
    assert _role_lookups(engine, "eng-ses_gone") == 1


def test_a_session_row_with_no_engine_id_is_a_worker_and_is_not_looked_up(
    instance: AppContext,
) -> None:
    engine = StandInEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="No engine id")
    bare = _session(ctx, item_id, sid="ses_bare", purpose="oversight", engine_id="")
    comment_work(bare, CommentParams(ref=ref, body="bare relays", reason=WHY))
    comment_work(ctx, CommentParams(ref=ref, body="person decides", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert "bare relays" not in brief and "person decides" in brief
    assert not [
        row
        for row in engine.sent
        if row["method"] == "GET" and row["url"].endswith("/api/sessions/")
    ]


def test_each_author_is_asked_once_per_brief(instance: AppContext) -> None:
    engine = StandInEngine()
    engine.roles["eng-ses_ov"] = "oversight"
    engine.roles["eng-ses_ov2"] = "oversight"
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="Per author")
    first = _session(ctx, item_id, sid="ses_ov", purpose="oversight")
    second = _session(ctx, item_id, sid="ses_ov2", purpose="oversight")
    for n in range(3):
        comment_work(first, CommentParams(ref=ref, body=f"first {n}", reason=WHY))
    for n in range(2):
        comment_work(second, CommentParams(ref=ref, body=f"second {n}", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert brief.count("### Note (") == 5
    assert _role_lookups(engine, "eng-ses_ov") == 1
    assert _role_lookups(engine, "eng-ses_ov2") == 1


def test_the_engine_is_not_asked_about_a_note_past_the_cap(
    instance: AppContext,
) -> None:
    engine = StandInEngine()
    engine.roles["eng-ses_ov"] = "oversight"
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="Cap")
    overseer = _session(ctx, item_id, sid="ses_ov", purpose="oversight")
    comment_work(overseer, CommentParams(ref=ref, body="oldest relay", reason=WHY))
    for n in range(5):
        comment_work(ctx, CommentParams(ref=ref, body=f"person {n}", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert brief.count("### Note (") == 5
    assert "oldest relay" not in brief
    assert _role_lookups(engine, "eng-ses_ov") == 0


def test_once_the_engine_is_down_no_later_author_is_asked(
    instance: AppContext,
) -> None:
    engine = _Refusing({"eng-ses_ov", "eng-ses_ov2"}, 503, "the engine is down")
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    ref, item_id = _item_with_worker(ctx, title="Down")
    first = _session(ctx, item_id, sid="ses_ov", purpose="oversight")
    second = _session(ctx, item_id, sid="ses_ov2", purpose="oversight")
    comment_work(first, CommentParams(ref=ref, body="first relay", reason=WHY))
    comment_work(second, CommentParams(ref=ref, body="second relay", reason=WHY))
    comment_work(ctx, CommentParams(ref=ref, body="person decides", reason=WHY))

    brief = _brief_from_wired_start(ctx, engine, ref)
    assert "person decides" in brief
    assert "first relay" not in brief and "second relay" not in brief
    # The newest author is asked once, finds the engine down, and the older
    # one is not asked at all: one unreachable engine, one wait.
    assert _role_lookups(engine, "eng-ses_ov2") == 1
    assert _role_lookups(engine, "eng-ses_ov") == 0


def test_a_rendered_brief_snapshot(instance: AppContext) -> None:
    brief = _render(
        instance,
        [
            _comment(1, "ship it behind a flag"),
            _comment(2, "noise", by="worker"),
            _handoff_c(3, "wi-1-thread"),
            _verdict_c(4, "abc123", "the flag is missing"),
        ],
    )
    start = brief.index("## Description")
    end = brief.index("## Deployment policy")
    assert brief[start:end] == (
        "## Description\n"
        "\n"
        "the body\n"
        "\n"
        "## Decisions\n"
        "\n"
        "### Note (2026-10-01, person)\n"
        "\n"
        "ship it behind a flag\n"
        "\n"
        "## Latest hand-off (2026-10-03, person) — a claim, not evidence\n"
        "\n"
        "- **branch** wi-1-thread\n"
        "- **head** abc123\n"
        "- **done** kind round-trips\n"
        "- **remaining** phase 2\n"
        "\n"
        "stop wi-1-thread\n"
        "\n"
        "## Latest review verdict (2026-10-04, person, head abc123)\n"
        "\n"
        "Blockers:\n"
        "\n"
        "1. the flag is missing\n"
        "\n"
        "review abc123\n"
        "\n"
    )
