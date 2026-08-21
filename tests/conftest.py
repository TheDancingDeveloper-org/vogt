"""Shared fixtures.

Every test runs against its own data directory and its own principal, so no
test depends on the OS user running it or on anything left behind by another.

That isolation is what makes the suite disk-bound: each test creates a
directory and two SQLite databases with their WAL and shared-memory files,
then throws them away. Measured on this host, the suite spent 275 seconds of
a 291-second CI job waiting on a disk, against under a second of CPU. The
`config` fixture below deals with that, and does not change what is under
test.
"""

from __future__ import annotations

from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.config import VogtConfig
from vogt.core.entities import WorkItem
from vogt.core.principal import Principal

TEST_PRINCIPAL = Principal(
    identity_ref="local:test-user", kind="human", display_name="test-user"
)


def native_work_item(
    ctx: AppContext,
    *,
    title: str,
    body: str = "",
    kind: str = "feature",
    project: str | None = None,
    state: str | None = None,
    priority: str = "p2",
    effort: str | None = None,
    assignee: str | None = None,
    initiative: str | None = None,
    labels: tuple[str, ...] = (),
    reason: str = "native fixture (pre-#183 shape)",
) -> WorkItem:
    """A declared work item in a project, written the audited way.

    Since #181 `work.create` refuses on an unlinked project (decision 10),
    but a *native declared row in a project* remains a legitimate shape the
    read plane must keep serving — `work.adopt` still produces one, and
    #183 owns their migration. Fixtures that need "a work item in this
    project" and are not testing the write-through plane use this instead
    of riding `work.create`; it lands the same audit and event rows the
    service does, so audit- and event-shaped assertions keep meaning what
    they meant.
    """
    from vogt.application.services import _resolve
    from vogt.application.writes import WriteOutcome, audited_write
    from vogt.core.entities import Actor
    from vogt.storage.interface import WriteTxn

    body_text = body

    def write(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkItem]:
        del actor
        workflow = txn.workflow_for(kind)
        now = ctx.clock()
        item = WorkItem(
            id=ctx.id_factory("wrk"),
            ref=txn.next_work_ref(),
            kind=kind,  # type: ignore[arg-type]
            title=title,
            body=body_text,
            state=state or workflow.initial_state,
            priority=priority,  # type: ignore[arg-type]
            effort=effort,  # type: ignore[arg-type]
            project_id=(None if project is None else _resolve.project(txn, project).id),
            initiative_id=(
                None if initiative is None else _resolve.initiative(txn, initiative).id
            ),
            origin="created",
            trust_state="unverified",
            assignee_actor_id=(
                None if assignee is None else _resolve.actor(txn, assignee).id
            ),
            labels=list(labels),
            created_at=now,
            updated_at=now,
        )
        txn.insert_work_item(item)
        stored = txn.work_item_by_id(item.id)
        assert stored is not None  # written in this transaction
        return WriteOutcome(
            result=stored,
            entity_kind="work_item",
            entity_id=item.id,
            payload=stored.model_dump(mode="json"),
            event_kind="work.created",
            summary={"ref": item.ref, "kind": item.kind, "title": item.title},
        )

    return audited_write(ctx, operation="work.create", reason=reason, body=write)


def mark_linked(ctx: AppContext, slug: str) -> None:
    """Persist `link_state='linked'` for a fixture project, the audited way.

    Since #183 an unlinked project's native rows are withdrawn from every
    curated surface, so a test whose subject is *not* linking — Board SQL
    paging, GUI serialisation — needs its fixture project linked for the
    rows to surface at all. `forge.link` validates credentials and migrates
    open items, which those tests are not about; this writes the same
    `link_state` row the real operation writes, with the same audit and
    event shape, and nothing else.
    """
    from vogt.application.services import _resolve
    from vogt.application.writes import WriteOutcome, audited_write
    from vogt.core.entities import Actor, Project
    from vogt.storage.interface import ProjectUpdate, WriteTxn

    def write(txn: WriteTxn, actor: Actor) -> WriteOutcome[Project]:
        del actor
        project = _resolve.project(txn, slug)
        txn.update_project(
            project.id, ProjectUpdate(link_state="linked"), at=ctx.clock()
        )
        updated = txn.project_by_id(project.id)
        assert updated is not None
        return WriteOutcome(
            result=updated,
            entity_kind="project",
            entity_id=project.id,
            payload={"link_state": "linked"},
            event_kind="forge.linked",
            summary={"slug": slug, "to": "linked"},
        )

    audited_write(
        ctx,
        operation="forge.link",
        reason="linked fixture (#183 surface shape)",
        body=write,
    )


def native_comment(
    ctx: AppContext,
    *,
    ref: str,
    body: str,
    reason: str = "native comment fixture (pre-#183 shape)",
) -> object:
    """A local comment on a native declared item, written the audited way.

    `work.comment` refuses on an unlinked project since #181 (decision 10);
    audit- and trail-shaped tests that need "a comment on this project's
    item" use this, which lands the same comment row, audit row and event
    the native service path does — minus the write-back attempt, which those
    tests were never about.
    """
    from vogt.application.services import _resolve
    from vogt.application.writes import WriteOutcome, audited_write
    from vogt.core.entities import Actor, Comment
    from vogt.storage.interface import WriteTxn

    body_text = body

    def write(txn: WriteTxn, actor: Actor) -> WriteOutcome[Comment]:
        item = _resolve.work_item(txn, ref)
        comment = Comment(
            id=ctx.id_factory("cmt"),
            work_item_id=item.id,
            actor_id=actor.id,
            actor_display_name=actor.display_name,
            body=body_text,
            created_at=ctx.clock(),
        )
        txn.insert_comment(comment)
        return WriteOutcome(
            result=comment,
            entity_kind="comment",
            entity_id=comment.id,
            payload=comment.model_dump(mode="json"),
            event_kind="work.commented",
            summary={"ref": item.ref, "comment_id": comment.id},
        )

    return audited_write(ctx, operation="work.comment", reason=reason, body=write)


class StepClock:
    """A clock that advances one second per read, so ordering is testable."""

    def __init__(self, start: datetime | None = None) -> None:
        self._now = start or datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)

    def __call__(self) -> datetime:
        current = self._now
        self._now += timedelta(seconds=1)
        return current


class SequentialIds:
    """Deterministic ids, so failures name the entity that broke."""

    def __init__(self) -> None:
        self._counts: dict[str, int] = {}

    def __call__(self, prefix: str) -> str:
        self._counts[prefix] = self._counts.get(prefix, 0) + 1
        return f"{prefix}_{self._counts[prefix]:04d}"


@pytest.fixture
def data_dir(tmp_path: Path) -> Path:
    return tmp_path / "instance"


@pytest.fixture
def config(data_dir: Path) -> VogtConfig:
    """Test configuration — durability off, because nothing here outlives the run.

    Without this the suite is entirely fsync-bound. Vogt opens a connection
    per transaction and closes it, and closing the last connection to a WAL
    database forces a checkpoint, which fsyncs. On a contended ext4 disk that
    measured 176ms *per transaction* — and `synchronous=normal` does not help,
    because it skips the per-commit fsync but still syncs at checkpoints.

    The effect on the suite was 50.65s for twenty tests, of which 0.8s was CPU
    and the rest was waiting. With `off`, the same twenty run in well under a
    second. Nothing is lost: every test writes to a temporary directory that
    is deleted afterwards, so durability across a power cut is not a property
    any of them has ever needed.

    This is a test-environment setting, not a test-only code path — the same
    knob is available in production (`VOGT_SQLITE_SYNCHRONOUS`), and the
    storage layer behaves identically either way.
    """
    return VogtConfig(data_dir=data_dir, sqlite_synchronous="off")


@pytest.fixture
def context(config: VogtConfig) -> AppContext:
    return build_context(
        config=config,
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )


@pytest.fixture
def instance(context: AppContext) -> AppContext:
    """An initialised instance, ready for writes."""
    init_instance(context, InitParams())
    return context


@pytest.fixture(scope="session", autouse=True)
def clean_env() -> Iterator[None]:
    """Remove any VOGT_* configuration leaking in from the developer's shell.

    This is suite-wide rather than opt-in because most tests construct
    ``VogtConfig`` directly, and pydantic-settings still lets environment
    values override fields those tests did not name. A developer running the
    suite inside Vogt's own engine therefore used to acquire the live GitHub
    adapter, engine state directory, and public identity in otherwise-local
    tests. Tests of environment precedence set their values after this
    fixture has cleaned the inherited process environment.
    """
    import os

    inherited = {
        key: value for key, value in os.environ.items() if key.startswith("VOGT_")
    }
    for key in inherited:
        os.environ.pop(key, None)
    try:
        yield
    finally:
        # The suite owns the process for its duration, but restoring the
        # caller's environment keeps embedded pytest runs unsurprising.
        for key in list(os.environ):
            if key.startswith("VOGT_"):
                os.environ.pop(key, None)
        os.environ.update(inherited)
