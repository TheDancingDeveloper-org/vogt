"""What the audit log can be asked (FR-S6, FR-U5, FR-U19).

Three properties, each of which was a hole the surface had to apologise for:

1. An item's trail contains every write against that item, comments
   included. A comment is audited against the comment, so the naive query
   returned creates, updates and transitions and dropped the conversation.
   A trail that omits a kind of write is the failure this product exists to
   prevent, because a reader believes it.
2. The log can be asked about a period. `since` is inclusive, `until` is
   exclusive; both boundaries are tested here deliberately, because "was
   this write inside the window" is the whole question.
3. The log can be read past its newest page, and paging it neither skips a
   record nor returns one twice.
"""

from __future__ import annotations

import time
from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from typing import Any

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    CreateLabelParams,
    CreateWorkParams,
    ListAuditParams,
    ListEventsParams,
    RegisterProjectParams,
    SuppressParams,
    TransitionWorkParams,
    UpdateWorkParams,
)
from vogt.application.services import (
    comment_work,
    create_label,
    create_work,
    list_audit,
    list_events,
    register_project,
    suppress,
    transition_work,
    update_work,
)
from vogt.core.entities import AuditRecord
from vogt.errors import NotFound

from tests.conftest import mark_linked, native_comment, native_work_item

WHY = "a test said so"


def _project(ctx: AppContext, name: str) -> str:
    return register_project(
        ctx,
        RegisterProjectParams(name=name, root_path=f"/srv/{name}", reason=WHY),
    ).project.slug


def _item(ctx: AppContext, title: str, *, project: str | None = None) -> str:
    # Since #181 `work.create` refuses on an unlinked project, and these
    # tests are about the audit log, not the write-through plane — so a
    # project-bound fixture item lands as the audited native row `work.adopt`
    # still produces, and a project-less one keeps riding the real service.
    if project is not None:
        return native_work_item(ctx, kind="bug", title=title, project=project).ref
    return create_work(
        ctx,
        CreateWorkParams(kind="bug", title=title, reason=WHY),
    ).item.ref


def _item_id(ctx: AppContext, ref: str) -> str:
    with ctx.declared.read() as view:
        item = view.work_item_by_ref(ref)
    assert item is not None
    return item.id


def _trail(ctx: AppContext, **narrowing: Any) -> list[AuditRecord]:
    return list_audit(ctx, ListAuditParams(**narrowing)).records


def _operations(records: list[AuditRecord]) -> list[str]:
    return [record.operation for record in records]


# -- FR-U5: an item's trail includes what was said about it ----------------


def test_a_comment_appears_in_its_work_items_audit_trail(
    instance: AppContext,
) -> None:
    """The gap FR-U5 was open on: a comment is audited against the comment.

    Filtering by the item's id has to follow that link, or the trail reports
    that nothing was said about an item people have been talking about.
    """
    ref = _item(instance, "Talked about")
    comment_work(instance, CommentParams(ref=ref, body="seen it", reason=WHY))

    trail = _trail(instance, entity_id=_item_id(instance, ref))

    assert "work.comment" in _operations(trail)
    assert "work.create" in _operations(trail)


def test_an_items_trail_carries_every_kind_of_write_against_it(
    instance: AppContext,
) -> None:
    ref = _item(instance, "Busy")
    update_work(instance, UpdateWorkParams(ref=ref, priority="p0", reason=WHY))
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    comment_work(instance, CommentParams(ref=ref, body="on it", reason=WHY))

    trail = _trail(instance, entity_id=_item_id(instance, ref))

    assert set(_operations(trail)) == {
        "work.create",
        "work.update",
        "work.transition",
        "work.comment",
    }


def test_one_items_comments_stay_out_of_another_items_trail(
    instance: AppContext,
) -> None:
    """Following the link must not become "return all comments"."""
    mine = _item(instance, "Mine")
    theirs = _item(instance, "Theirs")
    comment_work(instance, CommentParams(ref=theirs, body="not yours", reason=WHY))

    trail = _trail(instance, entity_id=_item_id(instance, mine))

    assert "work.comment" not in _operations(trail)


def test_filtering_by_a_comments_own_id_returns_only_that_comment(
    instance: AppContext,
) -> None:
    """The trail of a work item widens; the trail of anything else does not."""
    ref = _item(instance, "Talked about")
    first = comment_work(
        instance, CommentParams(ref=ref, body="one", reason=WHY)
    ).comment
    comment_work(instance, CommentParams(ref=ref, body="two", reason=WHY))

    trail = _trail(instance, entity_id=first.id)

    assert [record.entity_id for record in trail] == [first.id]


# -- FR-S6: the log can be asked about a period ----------------------------


def _at(ctx: AppContext, operation: str) -> datetime:
    """When the (single) write of this operation was audited."""
    records = _trail(ctx, operation=operation, limit=500)
    assert len(records) == 1, f"expected one {operation}, got {len(records)}"
    return records[0].at


def test_since_includes_a_write_made_at_exactly_that_instant(
    instance: AppContext,
) -> None:
    """`since` is the inclusive boundary. Stated here so it stays stated."""
    ref = _item(instance, "Boundary")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    moment = _at(instance, "work.transition")

    trail = _trail(instance, since=moment)

    assert "work.transition" in _operations(trail)


def test_until_excludes_a_write_made_at_exactly_that_instant(
    instance: AppContext,
) -> None:
    """`until` is the exclusive boundary — the other half of the same rule."""
    ref = _item(instance, "Boundary")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    moment = _at(instance, "work.transition")

    trail = _trail(instance, until=moment)

    assert "work.transition" not in _operations(trail)
    assert "work.create" in _operations(trail), "everything before it survives"


def test_a_window_excludes_what_happened_on_either_side_of_it(
    instance: AppContext,
) -> None:
    ref = _item(instance, "Windowed")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    comment_work(instance, CommentParams(ref=ref, body="after", reason=WHY))
    middle = _at(instance, "work.transition")

    trail = _trail(
        instance, since=middle, until=middle + timedelta(microseconds=1), limit=500
    )

    assert _operations(trail) == ["work.transition"]


def test_consecutive_windows_tile_the_log_without_gap_or_overlap(
    instance: AppContext,
) -> None:
    """Why one boundary is inclusive and the other is not.

    Two inclusive bounds would return a write made exactly at the seam in
    both windows, and a reader adding up a week would count it twice.
    """
    ref = _item(instance, "Tiled")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    comment_work(instance, CommentParams(ref=ref, body="said", reason=WHY))
    seam = _at(instance, "work.transition")

    whole = {record.id for record in _trail(instance, limit=500)}
    before = {record.id for record in _trail(instance, until=seam, limit=500)}
    after = {record.id for record in _trail(instance, since=seam, limit=500)}

    assert before & after == set(), "no record is in both windows"
    assert before | after == whole, "and none falls between them"


@pytest.fixture
def off_utc(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Run the test somewhere other than UTC, and put the zone back after.

    A bug that reads a naive timestamp in the server's local zone is
    invisible on a host that is already in UTC — which most CI runners are.
    """
    monkeypatch.setenv("TZ", "America/New_York")
    time.tzset()
    yield
    monkeypatch.undo()
    time.tzset()


@pytest.mark.skipif(not hasattr(time, "tzset"), reason="needs a settable TZ")
@pytest.mark.usefixtures("off_utc")
def test_a_time_bound_without_a_zone_is_read_as_utc(instance: AppContext) -> None:
    """Not as the server's local zone, which would move an audit boundary.

    Stored timestamps are UTC. A naive bound has to be read the same way, or
    the same query answers differently on two hosts.
    """
    ref = _item(instance, "Zoned")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    moment = _at(instance, "work.transition")

    aware = _trail(instance, since=moment, limit=500)
    naive = _trail(instance, since=moment.replace(tzinfo=None), limit=500)

    assert [record.id for record in naive] == [record.id for record in aware]


# -- FR-U19: reading past the newest page ----------------------------------


def _noisy(ctx: AppContext, writes: int) -> None:
    for index in range(writes):
        _item(ctx, f"Item {index}")


def test_an_offset_reaches_a_record_the_first_page_could_not_show(
    instance: AppContext,
) -> None:
    """Without this, everything older than one window was unreachable."""
    _noisy(instance, 6)
    total = list_audit(instance, ListAuditParams(limit=500)).total

    first_page = {record.id for record in _trail(instance, limit=2)}
    last_page = _trail(instance, limit=2, offset=total - 1)

    assert len(last_page) == 1, "the last page is short, and that is the end"
    assert last_page[0].operation == "instance.init", "the oldest write there is"
    assert last_page[0].id not in first_page


def test_paging_the_whole_log_neither_skips_a_record_nor_repeats_one(
    instance: AppContext,
) -> None:
    """The page boundary is where an unstable sort order shows up."""
    _noisy(instance, 7)
    every = [record.id for record in _trail(instance, limit=500)]

    paged: list[str] = []
    for offset in range(0, len(every), 3):
        paged += [record.id for record in _trail(instance, limit=3, offset=offset)]

    assert paged == every
    assert len(set(paged)) == len(paged)


def test_the_total_counts_matches_rather_than_the_page(
    instance: AppContext,
) -> None:
    _noisy(instance, 4)
    result = list_audit(instance, ListAuditParams(limit=2))

    assert len(result.records) == 2
    assert result.total == len(_trail(instance, limit=500))


def test_the_total_agrees_with_the_records_under_every_narrowing(
    instance: AppContext,
) -> None:
    """A total counted from a different set is a page indicator that lies."""
    slug = _project(instance, "Counted")
    ref = _item(instance, "Counted item", project=slug)
    native_comment(instance, ref=ref, body="counted")
    moment = _at(instance, "work.comment")

    narrowings: tuple[dict[str, Any], ...] = (
        {},
        {"operation": "work.comment"},
        {"entity_id": _item_id(instance, ref)},
        {"project": slug},
        {"since": moment},
        {"until": moment},
        {"project": slug, "since": moment},
    )
    for narrowing in narrowings:
        result = list_audit(instance, ListAuditParams(limit=500, **narrowing))
        assert result.total == len(result.records), narrowing


# -- FR-U19: the project filter, answered in the query ---------------------


def test_a_projects_trail_holds_its_items_and_their_comments(
    instance: AppContext,
) -> None:
    slug = _project(instance, "Ours")
    ref = _item(instance, "Ours", project=slug)
    native_comment(instance, ref=ref, body="ours")

    trail = _trail(instance, project=slug, limit=500)

    assert set(_operations(trail)) == {
        "project.register",
        "work.create",
        "work.comment",
    }


def test_another_projects_writes_are_not_in_this_projects_trail(
    instance: AppContext,
) -> None:
    ours = _project(instance, "Ours")
    theirs = _project(instance, "Theirs")
    _item(instance, "Theirs", project=theirs)

    trail = _trail(instance, project=ours, limit=500)

    assert _operations(trail) == ["project.register"]


def test_a_projects_trail_reaches_a_kind_that_is_not_a_work_item(
    instance: AppContext,
) -> None:
    """A suppression carries its own project, and the filter follows that too.

    Here because the mapping from a project to an entity kind is a table:
    covering only work items would leave every other row in it unproven.
    """
    slug = _project(instance, "Scoped")
    suppress(
        instance,
        SuppressParams(subject="mark:scoped/notes.md#L1", project=slug, reason=WHY),
    )

    trail = _trail(instance, project=slug, limit=500)

    assert "suppress" in _operations(trail)


def test_a_project_filter_leaves_out_writes_that_belong_to_no_project(
    instance: AppContext,
) -> None:
    """`init` and an unfiled work item are instance-wide, not this project's.

    Excluded rather than lost: they are still in the unfiltered log, which is
    what a filter is supposed to mean.
    """
    slug = _project(instance, "Filed")
    _item(instance, "Unfiled")

    filed = _operations(_trail(instance, project=slug, limit=500))
    everything = _operations(_trail(instance, limit=500))

    assert "instance.init" not in filed
    assert filed.count("work.create") == 0
    assert "instance.init" in everything
    assert everything.count("work.create") == 1


def test_an_unknown_project_is_named_rather_than_answered_with_nothing(
    instance: AppContext,
) -> None:
    """An empty page would read as "this project has done nothing"."""
    with pytest.raises(NotFound, match="no-such-project"):
        _trail(instance, project="no-such-project")


# -- the store's own contract ----------------------------------------------


def test_the_stored_trail_is_ordered_newest_write_first(
    instance: AppContext,
) -> None:
    _noisy(instance, 3)
    with instance.declared.read() as view:
        records = view.list_audit(limit=50)

    revisions = [record.revision for record in records]
    assert revisions == sorted(revisions, reverse=True)
    assert records[-1].operation == "instance.init"


def test_a_time_bound_survives_a_timestamp_with_microseconds(
    instance: AppContext,
) -> None:
    """Stored times are ISO text, so the comparison is textual.

    A whole second renders as `...:00+00:00` and a fraction of one as
    `...:00.5+00:00`; the bound has to order them the way the clock does.
    """
    with instance.declared.write() as txn:
        actor = txn.actor_by_identity("local:test-user")
        assert actor is not None
        for micros in (0, 500_000):
            txn.append_audit(
                actor=actor,
                operation="test.tick",
                entity_kind="thing",
                entity_id=f"thing_{micros}",
                reason="because",
                payload_digest="sha256:0",
                at=datetime(2030, 1, 1, 0, 0, 0, micros, tzinfo=UTC),
            )

    with instance.declared.read() as view:
        after = view.list_audit(
            limit=50,
            operation="test.tick",
            since=datetime(2030, 1, 1, 0, 0, 0, 1, tzinfo=UTC),
        )

    assert [record.entity_id for record in after] == ["thing_500000"]


# ── The other half of an item's history (FR-U5, FR-N1) ────────────────────
#
# The audit says a transition happened, who made it and why, and keeps a
# digest rather than the payload — so it cannot say which state the item came
# from. The event can, and these assert that it does.


def test_an_items_events_carry_the_states_it_moved_between(
    instance: AppContext,
) -> None:
    ref = _item(instance, "Sweep drops a page")
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="review", reason=WHY)
    )

    events = list_events(
        instance, ListEventsParams(entity_id=_item_id(instance, ref))
    ).events
    moves = [e for e in events if e.kind == "work.transitioned"]
    assert [(e.summary["from"], e.summary["to"]) for e in moves] == [
        ("open", "in_progress"),
        ("in_progress", "review"),
    ], "the feed says what the audit's digest cannot: which state it came from"
    assert all(e.audit_id for e in moves), (
        "and each names the audit row that says why, so the two halves join"
    )


def test_a_transition_event_carries_project_kind_and_labels(
    instance: AppContext,
) -> None:
    # #290: the engine's agent-task trigger filters a transition by project,
    # kind, and label with no view of this registry, so the transitioned event
    # carries them beside the states it moved between. Additive — `ref`, `from`,
    # and `to` are unchanged.
    slug = _project(instance, "triggers")
    mark_linked(instance, slug)
    for label in ("urgent", "backend"):
        create_label(instance, CreateLabelParams(name=label, reason=WHY))
    ref = native_work_item(
        instance,
        kind="bug",
        title="Trigger fodder",
        project=slug,
        labels=("urgent", "backend"),
    ).ref
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="in_progress", reason=WHY)
    )

    events = list_events(
        instance, ListEventsParams(entity_id=_item_id(instance, ref))
    ).events
    moved = next(e for e in events if e.kind == "work.transitioned")
    assert moved.summary["ref"] == ref
    assert moved.summary["to"] == "in_progress"
    assert moved.summary["kind"] == "bug"
    assert moved.summary["project"] == slug
    labels = moved.summary["labels"]
    assert isinstance(labels, list)
    assert sorted(labels) == ["backend", "urgent"]


def test_an_items_history_excludes_every_other_items(instance: AppContext) -> None:
    mine = _item(instance, "Mine")
    theirs = _item(instance, "Theirs")
    transition_work(
        instance, TransitionWorkParams(ref=theirs, to_state="in_progress", reason=WHY)
    )

    events = list_events(
        instance, ListEventsParams(entity_id=_item_id(instance, mine))
    ).events
    assert [e.kind for e in events] == ["work.created"]


def test_the_cursor_still_walks_a_narrowed_feed(instance: AppContext) -> None:
    """The filter is applied in SQL, not to a page of the whole feed.

    A page filtered after the read would return whatever slice of the feed
    happened to contain some of this item's events, and a caller paging it
    would decide the history ended at the first quiet stretch — which, on a
    busy estate, is immediately.
    """
    ref = _item(instance, "Mine")
    noise = _item(instance, "Noise")
    for state in ("in_progress", "review"):
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state=state, reason=WHY)
        )
        # Interleaved, so the item's events are sparse in the feed.
        transition_work(
            instance, TransitionWorkParams(ref=noise, to_state=state, reason=WHY)
        )

    mine = _item_id(instance, ref)
    first = list_events(instance, ListEventsParams(entity_id=mine, limit=2))
    assert len(first.events) == 2
    rest = list_events(
        instance, ListEventsParams(entity_id=mine, after=first.next_cursor, limit=2)
    )
    seqs = [e.seq for e in first.events + rest.events]
    assert seqs == sorted(seqs) and len(set(seqs)) == len(seqs), (
        "pages of one entity's history neither repeat nor go backwards"
    )
    assert all(e.entity_id == mine for e in rest.events)
