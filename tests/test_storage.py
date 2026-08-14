"""The declared store: atomicity, revisions, and refusing to guess."""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.core.entities import Actor, CodingSession, Project, Token, WorkItem
from vogt.errors import AlreadyInitialized, NotInitialized
from vogt.storage.sqlite.declared import SqliteDeclaredStore

from tests.conftest import TEST_PRINCIPAL, StepClock


def _project(context: AppContext, slug: str) -> Project:
    now = context.clock()
    return Project(
        id=context.id_factory("prj"),
        slug=slug,
        name=slug,
        root_path=f"/srv/{slug}",
        created_at=now,
        updated_at=now,
    )


def _work_item(context: AppContext, project: Project, title: str) -> WorkItem:
    now = context.clock()
    with context.declared.write() as txn:
        item = WorkItem(
            id=context.id_factory("wrk"),
            ref=txn.next_work_ref(),
            kind="feature",
            title=title,
            state="open",
            project_id=project.id,
            created_at=now,
            updated_at=now,
        )
        txn.insert_work_item(item)
    return item


def _session(
    context: AppContext,
    *,
    project: Project,
    item: WorkItem | None = None,
    engine_session_id: str,
) -> CodingSession:
    actor_id = _actor_id(context)
    session = CodingSession(
        id=context.id_factory("ses"),
        engine_session_id=engine_session_id,
        project_id=project.id,
        work_item_id=None if item is None else item.id,
        actor_id=actor_id,
        cwd=project.root_path,
        template="claude",
        reason="pairing on it",
        started_at=context.clock(),
    )
    with context.declared.write() as txn:
        txn.insert_session(session)
    return session


def _token(context: AppContext, actor_id: str, name: str) -> Token:
    return Token(
        id=context.id_factory("tok"),
        actor_id=actor_id,
        name=name,
        scopes=["work.write"],
        created_at=context.clock(),
    )


def _actor_id(context: AppContext) -> str:
    with context.declared.read() as view:
        actor = view.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert actor is not None
        return actor.id


def test_reading_an_uninitialised_directory_says_so(data_dir: Path) -> None:
    store = SqliteDeclaredStore(data_dir / "declared.sqlite3")
    with pytest.raises(NotInitialized, match="vogt init"), store.read():
        pass


def test_migrated_but_unbootstrapped_is_still_uninitialised(data_dir: Path) -> None:
    """A file with tables but no instance row is not an instance."""
    store = SqliteDeclaredStore(data_dir / "declared.sqlite3")
    store.migrate()
    with pytest.raises(NotInitialized), store.read():
        pass


def test_bootstrapping_twice_is_refused(data_dir: Path) -> None:
    store = SqliteDeclaredStore(data_dir / "declared.sqlite3", clock=StepClock())
    store.migrate()
    store.bootstrap(TEST_PRINCIPAL)
    with pytest.raises(AlreadyInitialized):
        store.bootstrap(TEST_PRINCIPAL)


def test_bootstrap_audits_at_revision_zero_and_emits_no_event(
    instance: AppContext,
) -> None:
    """`init` creates the instance; it does not change anything inside it.

    So the audit trail explains where the instance came from, while the
    `/events` cursor starts at the first change a client could act on.
    """
    with instance.declared.read() as view:
        assert view.current_revision() == 0
        assert view.counts().audit == 1
        assert view.counts().events == 0
        record = view.list_audit(limit=10)[0]
        assert record.operation == "instance.init"
        assert record.revision == 0


def test_a_failed_write_rolls_back_the_revision_too(instance: AppContext) -> None:
    """NFR-I1: the revision bump is part of the transaction, not beside it."""
    before = _revision(instance)

    with pytest.raises(RuntimeError, match="boom"), instance.declared.write() as txn:
        txn.insert_project(_project(instance, "doomed"))
        msg = "boom"
        raise RuntimeError(msg)

    with instance.declared.read() as view:
        assert view.current_revision() == before
        assert view.project_by_slug("doomed") is None
        assert view.counts().projects == 0


def test_each_write_takes_the_next_revision(instance: AppContext) -> None:
    for index, slug in enumerate(("one", "two", "three"), start=1):
        with instance.declared.write() as txn:
            assert txn.revision == index
            txn.insert_project(_project(instance, slug))
    with instance.declared.read() as view:
        assert view.current_revision() == 3


def test_projects_are_listed_by_slug_with_paging(instance: AppContext) -> None:
    for slug in ("charlie", "alpha", "bravo"):
        with instance.declared.write() as txn:
            txn.insert_project(_project(instance, slug))

    with instance.declared.read() as view:
        assert [p.slug for p in view.list_projects(limit=2, offset=0)] == [
            "alpha",
            "bravo",
        ]
        assert [p.slug for p in view.list_projects(limit=2, offset=2)] == ["charlie"]


def test_stored_projects_round_trip(instance: AppContext) -> None:
    with instance.declared.write() as txn:
        original = _project(instance, "round-trip")
        txn.insert_project(original)
    with instance.declared.read() as view:
        loaded = view.project_by_slug("round-trip")
    assert loaded == original


def test_audit_can_be_filtered(instance: AppContext) -> None:
    with instance.declared.write() as txn:
        actor = txn.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert actor is not None
        txn.append_audit(
            actor=actor,
            operation="test.op",
            entity_kind="thing",
            entity_id="thing_1",
            reason="because",
            payload_digest="sha256:0",
            at=instance.clock(),
        )
    with instance.declared.read() as view:
        assert len(view.list_audit(limit=10, operation="test.op")) == 1
        assert len(view.list_audit(limit=10, operation="absent.op")) == 0
        assert len(view.list_audit(limit=10, entity_id="thing_1")) == 1
        assert len(view.list_audit(limit=10, actor_id=actor.id)) == 2


def test_a_session_round_trips_by_either_id(instance: AppContext) -> None:
    """Vogt's id and the engine's both have to find the link (FR-E4).

    The engine names a terminal by its own id in everything it sends, so a
    link findable only by Vogt's id would be unreachable from that side.
    """
    project = _project(instance, "engine-work")
    with instance.declared.write() as txn:
        txn.insert_project(project)
    item = _work_item(instance, project, "make it work")
    session = _session(instance, project=project, item=item, engine_session_id="eng-1")

    with instance.declared.read() as view:
        assert view.session_by_id(session.id) == session
        assert view.session_by_engine_id("eng-1") == session
        assert view.session_by_id("ses_absent") is None
        assert view.session_by_engine_id("eng-absent") is None


def test_sessions_are_listed_by_project_and_by_work_item(
    instance: AppContext,
) -> None:
    project = _project(instance, "listed")
    other = _project(instance, "elsewhere")
    with instance.declared.write() as txn:
        txn.insert_project(project)
        txn.insert_project(other)
    item = _work_item(instance, project, "the item")
    on_item = _session(
        instance, project=project, item=item, engine_session_id="eng-item"
    )
    on_project = _session(instance, project=project, engine_session_id="eng-proj")
    elsewhere = _session(instance, project=other, engine_session_id="eng-other")

    with instance.declared.read() as view:
        # Newest first, and unfiltered means every project.
        assert [s.id for s in view.list_sessions(limit=10, offset=0)] == [
            elsewhere.id,
            on_project.id,
            on_item.id,
        ]
        assert [
            s.id for s in view.list_sessions(project_id=project.id, limit=10, offset=0)
        ] == [on_project.id, on_item.id]
        assert [
            s.id for s in view.list_sessions(work_item_id=item.id, limit=10, offset=0)
        ] == [on_item.id]
        assert view.list_sessions(limit=1, offset=1)[0].id == on_project.id


def test_stopping_a_session_takes_it_out_of_the_live_list(
    instance: AppContext,
) -> None:
    """`include_stopped=False` is the default because "what is running for
    this item" is the question nearly every caller has."""
    project = _project(instance, "stopping")
    with instance.declared.write() as txn:
        txn.insert_project(project)
    session = _session(instance, project=project, engine_session_id="eng-stop")
    stopped_at = instance.clock()

    with instance.declared.write() as txn:
        txn.mark_session_stopped(session.id, at=stopped_at)

    with instance.declared.read() as view:
        assert view.list_sessions(limit=10, offset=0) == []
        listed = view.list_sessions(include_stopped=True, limit=10, offset=0)
        assert [s.id for s in listed] == [session.id]
        assert listed[0].stopped_at == stopped_at


def test_stopping_a_stopped_session_keeps_the_first_time(
    instance: AppContext,
) -> None:
    """A second `session.stop`, or an end the engine reports twice, is not a
    reason to rewrite when the session ended."""
    project = _project(instance, "twice")
    with instance.declared.write() as txn:
        txn.insert_project(project)
    session = _session(instance, project=project, engine_session_id="eng-twice")
    first = instance.clock()
    later = instance.clock()

    with instance.declared.write() as txn:
        txn.mark_session_stopped(session.id, at=first)
    with instance.declared.write() as txn:
        txn.mark_session_stopped(session.id, at=later)

    with instance.declared.read() as view:
        stored = view.session_by_id(session.id)
    assert stored is not None
    assert stored.stopped_at == first


def test_tokens_can_be_found_by_the_actor_they_belong_to(
    instance: AppContext,
) -> None:
    """Stopping a session revokes the token it ran with (FR-S10).

    That lookup starts from the actor, so it must not be a page of the whole
    token table filtered in Python — a live credential past the limit would
    be left behind silently.
    """
    mine = _actor_id(instance)
    theirs = Actor(
        id=instance.id_factory("act"),
        kind="agent",
        display_name="somebody else",
        identity_ref="agent:other",
        created_at=instance.clock(),
    )
    with instance.declared.write() as txn:
        txn.insert_actor(theirs)
        txn.insert_token(_token(instance, mine, "live"), token_hash="hash-live")
        txn.insert_token(_token(instance, mine, "dead"), token_hash="hash-dead")
        txn.insert_token(_token(instance, theirs.id, "other"), token_hash="hash-other")
    with instance.declared.read() as view:
        dead = next(t for t in view.tokens_for_actor(mine) if t.name == "dead")
    with instance.declared.write() as txn:
        assert txn.revoke_token(dead.id, reason="rotated", at=instance.clock())

    with instance.declared.read() as view:
        assert [t.name for t in view.tokens_for_actor(mine)] == ["live"]
        assert [t.name for t in view.tokens_for_actor(mine, include_revoked=True)] == [
            "dead",
            "live",
        ]
        assert [t.name for t in view.tokens_for_actor(theirs.id)] == ["other"]
        assert view.tokens_for_actor("act_absent") == []


def _revision(context: AppContext) -> int:
    with context.declared.read() as view:
        return view.current_revision()
