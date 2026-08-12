"""The declared store: atomicity, revisions, and refusing to guess."""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.core.entities import Project
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


def _revision(context: AppContext) -> int:
    with context.declared.read() as view:
        return view.current_revision()
