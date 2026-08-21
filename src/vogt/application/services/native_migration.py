"""Native-item migration on link/publish (#183, design §4, decision 7).

When `forge.link` or `forge.publish` succeeds on a project that still holds
**open** native work items, each one is published upstream as an issue and
re-keyed: the vogt-only fields (priority, effort, assignee, initiative,
workflow state) fold into a `work_overlay` row under the new subject key, and
the native row is retired with a `superseded_by` marker naming that subject.
Closed and archived native items stay historical, untouched — migrating a
record of finished work would rewrite history to satisfy a model change.

## Retire-by-marker, not deletion

The native row anchors the item's whole local history — comments, relations,
the FR-B2 write-back ledger, audit rows keyed by its id — and deleting it
would orphan or destroy that history. A superseded row is excluded from every
work view (the shared `_work_where` filter), so each issue is counted exactly
once; the `WI-n` ref still resolves for anyone following an old trail. No
`work_links` row is written for the new subject: the #181 dedup reads
`work_links` as "this declared row IS the item", which is exactly what a
retired row is not — writing one would hide the upstream item it just made.

## Partial failure is honest, not atomic

Migration is per-item write-through, in the fail-loud shape of decision 9:
each item's issue is created upstream *then* its local re-keying commits, so
a provider failure mid-run stops the loop with a typed error that names which
items migrated and which are still native. Nothing is silently dropped and
nothing pretends to be all-or-nothing: the migrated items are upstream truth,
the rest remain native rows on a now-linked project (still readable, still
listed), and re-running `forge.link` — idempotent on a linked project —
resumes exactly where the failure stopped.
"""

from __future__ import annotations

from vogt.adapters.forge import ForgeProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.writeback import permits
from vogt.application.context import AppContext
from vogt.application.models import MigratedItem
from vogt.application.services.work import _merged_overlay
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Project, WorkItem, WriteBackRecord
from vogt.errors import UpstreamWriteFailed, UpstreamWriteRefused
from vogt.storage.interface import ReadView, WorkFilter, WorkItemUpdate, WriteTxn

WORK_MIGRATED_EVENT = "work.migrated"

#: Far above any plausible native backlog; the storage default of 100 would
#: silently strand item 101, which is the one thing this module must not do.
_MIGRATION_LIMIT = 10_000


def open_native_items(view: ReadView, project: Project) -> list[WorkItem]:
    """The migration population: this project's open, un-retired native rows.

    Terminal (closed/archived) items are deliberately absent — they stay
    historical — and already-superseded rows are excluded by the shared
    filter, which is what makes a resumed migration pick up only what the
    failed run left behind. Rows whose adoption link already names a forge
    *object* are excluded too: they are the pre-#181 bridge — upstream
    identity exists, write-back already speaks to it, and the #181 dedup
    treats the declared row as the item — so publishing a second issue for
    one would duplicate the very thing it tracks. Marker adoptions
    (`mark:…#L2`) carry no upstream object and migrate like any native row.
    """
    items = view.list_work_items(
        WorkFilter(project_id=project.id, exclude_terminal=True, limit=_MIGRATION_LIMIT)
    )
    return [item for item in items if not _already_upstream_coupled(view, item)]


def _already_upstream_coupled(view: ReadView, item: WorkItem) -> bool:
    """Whether an adoption link already names a forge object for this row.

    A forge object's subject key ends in `#<number>` (D5 — every provider's
    scheme keys objects by number); a source marker's ends in `#L<line>`.
    Distinguishing on the numeric tail is therefore the scheme itself, not a
    guess about one provider's spelling.
    """
    return any(
        subject.rpartition("#")[2].isdigit()
        for subject in view.work_links_for_subjects_by_item(item.id)
    )


def require_migratable(project: Project, pending: list[WorkItem]) -> None:
    """Refuse the whole link/publish before anything is sent (decision 9).

    Migration creates issues upstream, so it is governed by the same FR-B1
    policy gate `work.create` honours. Checking before the project links —
    rather than failing mid-migration — keeps "linked" and "migrated" from
    drifting apart when the policy was never going to allow the second half.
    """
    if pending and not permits(project.write_back, "create"):
        raise UpstreamWriteRefused(
            f"cannot make {project.slug!r} upstream-truth: {len(pending)} open "
            f"native item(s) must migrate upstream, and write-back policy "
            f"{project.write_back!r} does not permit 'create' — set the "
            "policy with `forge writeback` first, so no open item is left "
            "behind silently"
        )


def migrate_open_native_items(
    ctx: AppContext,
    *,
    project: Project,
    items: list[WorkItem],
    provider: ForgeProvider,
    identity: str,
    repo: RepoRef,
    reason: str,
    operation: str,
) -> list[MigratedItem]:
    """Publish each open native item upstream and re-key it (#183).

    Called after the project is linked, with the population enumerated by
    `open_native_items` *before* the link — the caller passes it so the
    policy gate and the migration agree on what "pending" meant. Per item:
    one `create_issue` (labels ride along — shared vocabulary, exactly as
    `work.create`'s write-through sends them), then one audited declared
    write folding the vogt-only fields into the overlay and retiring the
    native row. The mirror learns about the new issues on the next sweep or
    consolidation, like any other write-through.
    """
    migrated: list[MigratedItem] = []
    for item in items:
        sent = provider.create_issue(
            repo,
            title=item.title,
            body=_migrated_body(item),
            labels=list(item.labels) or None,
        )
        subject = sent.subject_key
        if sent.outcome != "succeeded" or not subject:
            done = ", ".join(entry.ref for entry in migrated) or "none"
            remaining = ", ".join(entry.ref for entry in items[len(migrated) :])
            raise UpstreamWriteFailed(
                f"native-item migration stopped at {item.ref}: "
                f"{sent.detail or 'no detail from the provider'}. "
                f"Migrated upstream: {done}. Still native: {remaining} — "
                f"{project.slug!r} is linked, those items remain readable, "
                "and re-running `forge link` resumes the migration"
            )
        _commit_migration(
            ctx,
            project=project,
            item=item,
            subject=subject,
            identity=identity,
            source_url=sent.source_url,
            reason=reason,
            operation=operation,
        )
        migrated.append(
            MigratedItem(
                ref=item.ref,
                subject_key=subject,
                title=item.title,
                source_url=sent.source_url,
            )
        )
    return migrated


def _migrated_body(item: WorkItem) -> str:
    """The issue body: the native prose, with its provenance named."""
    footer = f"— migrated from Vogt as {item.ref}"
    return f"{item.body}\n\n{footer}" if item.body else footer


def _commit_migration(
    ctx: AppContext,
    *,
    project: Project,
    item: WorkItem,
    subject: str,
    identity: str,
    source_url: str | None,
    reason: str,
    operation: str,
) -> None:
    """One item's local half: overlay in, native row retired, ledger written."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[MigratedItem]:
        now = ctx.clock()
        txn.upsert_work_overlay(
            _merged_overlay(
                txn.work_overlay(subject),
                subject_key=subject,
                project_id=project.id,
                at=now,
                workflow_state=item.state,
                priority=item.priority,
                effort=item.effort,
                assignee_actor_id=item.assignee_actor_id,
                initiative_id=item.initiative_id,
            )
        )
        txn.update_work_item(item.id, WorkItemUpdate(superseded_by=subject), at=now)
        txn.insert_writeback(
            WriteBackRecord(
                id=ctx.id_factory("wbk"),
                at=now,
                project_id=project.id,
                work_item_id=item.id,
                actor_id=actor.id,
                action="create",
                subject_key=subject,
                policy=project.write_back,
                outcome="succeeded",
                reason=reason,
                detail=f"as {identity}; migrated from {item.ref}",
                source_url=source_url,
            )
        )
        result = MigratedItem(
            ref=item.ref,
            subject_key=subject,
            title=item.title,
            source_url=source_url,
        )
        return WriteOutcome(
            result=result,
            entity_kind="work_item",
            entity_id=item.id,
            payload=result.model_dump(mode="json"),
            event_kind=WORK_MIGRATED_EVENT,
            summary={"from": item.ref, "to": subject, "title": item.title},
        )

    audited_write(ctx, operation=operation, reason=reason, body=body)


__all__ = [
    "WORK_MIGRATED_EVENT",
    "migrate_open_native_items",
    "open_native_items",
    "require_migratable",
]
