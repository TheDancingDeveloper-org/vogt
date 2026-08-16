"""Write-back as a consequence of a declared write, never a separate act.

FR-B2: every write-back action is audited locally and re-observed on the
next sweep — Vogt observes its own writes like anyone else's. That loop is
what makes a write-back a fact rather than a claim.

The shape matters. There is no "push this item upstream" operation. A
comment authored in Vogt posts upstream *as part of commenting*; closing an
item closes the linked issue *as part of transitioning*. A separate push
command would mean a declared change and its upstream half could drift
apart, which is precisely the thing this product exists to notice.

An upstream failure never fails the local write. The declared change stands,
the ledger records that the remote half did not land, and the next sweep
shows the disagreement as drift.
"""

from __future__ import annotations

from dataclasses import dataclass

from vogt.adapters.github.client import GitHubClient
from vogt.adapters.github.collectors import (
    KIND_ISSUE,
    KIND_PULL_REQUEST,
    KIND_RELEASE,
)
from vogt.adapters.github.consolidate import KIND_LABEL, GitHubConsolidator
from vogt.adapters.github.writeback import ForgeWriter, permits
from vogt.application.context import AppContext
from vogt.application.models import (
    OnboardParams,
    OnboardResult,
    SetWriteBackParams,
    WriteBackActionView,
    WriteBackListParams,
    WriteBackListResult,
)
from vogt.application.services import _resolve
from vogt.application.writes import audited_action
from vogt.collectors.base import CollectorContext
from vogt.core.entities import Actor, Project, WorkItem, WriteBackRecord

WRITEBACK_SET = "project.writeback"
WRITEBACK_SET_EVENT = "project.writeback_set"
#: One consolidation run, attributed to whoever asked for it (FR-S1).
ONBOARDED_EVENT = "forge.onboarded"


@dataclass(frozen=True)
class Attempt:
    """One write-back, as it will be recorded."""

    action: str
    work_item: WorkItem | None
    project: Project | None
    body: str | None = None
    labels: tuple[str, ...] = ()
    state: str | None = None


def writer_for(ctx: AppContext) -> ForgeWriter | None:
    """A writer, or `None` when the adapter is not configured.

    `None` is the ordinary case, and it is not a failure: an instance with no
    GitHub token simply never speaks upstream.
    """
    client = GitHubClient.from_token_file(ctx.config.github_token_file)
    return None if client is None else ForgeWriter(client)


def attempt(
    ctx: AppContext,
    *,
    actor: Actor,
    action: str,
    project: Project | None,
    item: WorkItem | None,
    subject_key: str | None,
    reason: str,
    body: str | None = None,
    labels: tuple[str, ...] = (),
    state: str | None = None,
) -> WriteBackRecord:
    """Perform one write-back if policy allows, and record what happened.

    Called from inside a declared write's service, *after* the local change
    has been decided. Returns a record either way — including `skipped`,
    which is the answer for the overwhelmingly common case of a project with
    `write_back: none`.
    """
    policy = "none" if project is None else project.write_back
    record = WriteBackRecord(
        id=ctx.id_factory("wbk"),
        at=ctx.clock(),
        project_id=None if project is None else project.id,
        work_item_id=None if item is None else item.id,
        actor_id=actor.id,
        action=action,  # type: ignore[arg-type]
        subject_key=subject_key,
        policy=policy,
        outcome="skipped",
        reason=reason,
        detail=None,
    )

    if not permits(policy, action):
        return record.model_copy(
            update={
                "detail": (
                    f"policy is {policy!r}; {action} is not permitted"
                    if policy != "none"
                    else "write-back is off for this project"
                )
            }
        )
    if subject_key is None and action != "create":
        return record.model_copy(
            update={"detail": "this item is not linked to a forge object"}
        )

    writer = writer_for(ctx)
    if writer is None:
        return record.model_copy(
            update={"detail": "the GitHub adapter is not configured"}
        )

    repo_url = None if project is None else project.repo_url
    number = _number_of(subject_key)
    if action == "comment" and number is not None and body is not None:
        result = writer.comment(repo_url=repo_url, number=number, body=body)
    elif action == "label" and number is not None and labels:
        result = writer.add_labels(
            repo_url=repo_url, number=number, labels=list(labels)
        )
    elif action in ("close", "reopen") and number is not None:
        result = writer.set_state(
            repo_url=repo_url,
            number=number,
            state="closed" if action == "close" else "open",
        )
    elif action == "create" and item is not None:
        result = writer.create_issue(
            repo_url=repo_url,
            title=item.title,
            body=item.body or f"Filed from Vogt as {item.ref}.",
            labels=list(item.labels),
        )
    else:
        return record.model_copy(update={"detail": f"nothing to send for {action}"})

    return record.model_copy(
        update={
            "outcome": result.outcome,
            "detail": result.detail,
            "source_url": result.source_url,
            "subject_key": result.subject_key or subject_key,
        }
    )


def _number_of(subject_key: str | None) -> int | None:
    """`gh:owner/repo#123` -> 123."""
    if not subject_key or "#" not in subject_key:
        return None
    _, _, tail = subject_key.partition("#")
    return int(tail) if tail.isdigit() else None


# -- operations ------------------------------------------------------------


def set_write_back(ctx: AppContext, params: SetWriteBackParams) -> object:
    """Set a project's write-back policy (FR-B1).

    Defaults to `none` and is set one project at a time on purpose: a tool
    holding a token for somebody's issue tracker should speak only where it
    has been told to.
    """
    from vogt.application.models import ProjectResult
    from vogt.application.writes import WriteOutcome, audited_write
    from vogt.storage.interface import ProjectUpdate, WriteTxn

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        project = _resolve.project(txn, params.project)
        txn.update_project(
            project.id, ProjectUpdate(write_back=params.policy), at=ctx.clock()
        )
        updated = txn.project_by_slug(params.project)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=ProjectResult(project=updated),
            entity_kind="project",
            entity_id=project.id,
            payload={"write_back": params.policy},
            event_kind=WRITEBACK_SET_EVENT,
            summary={
                "slug": project.slug,
                "from": project.write_back,
                "to": params.policy,
            },
        )

    return audited_write(ctx, operation=WRITEBACK_SET, reason=params.reason, body=body)


def list_write_backs(
    ctx: AppContext, params: WriteBackListParams
) -> WriteBackListResult:
    """The ledger of what Vogt has said upstream, and what landed."""
    with ctx.declared.read() as view:
        records = view.list_writeback_actions(
            outcome=params.outcome, limit=params.limit
        )
    return WriteBackListResult(
        actions=[
            WriteBackActionView(
                id=record.id,
                at=record.at,
                action=record.action,
                subject_key=record.subject_key,
                policy=record.policy,
                outcome=record.outcome,
                reason=record.reason,
                detail=record.detail,
                source_url=record.source_url,
            )
            for record in records
        ]
    )


def onboard(ctx: AppContext, params: OnboardParams) -> OnboardResult:
    """Consolidate a repository's existing state, read-only (FR-B3).

    Zero GitHub mutations. Existing objects are incumbent and preserved;
    what this does is make them *visible*, so `adopt` can attach declared
    work to what is already there and drift can keep the pair honest.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)

    client = GitHubClient.from_token_file(ctx.config.github_token_file)
    if client is None:
        unconfigured = OnboardResult(
            project=project.slug,
            repo=project.repo_url,
            issues=0,
            pull_requests=0,
            labels=0,
            releases=0,
            new=0,
            unchanged=0,
            detail=(
                "the GitHub adapter is not configured, so nothing was read — "
                "which is 'not collected', not 'there is nothing'"
            ),
        )
        _record_onboard(ctx, project, params.reason, unconfigured)
        return unconfigured

    consolidator = GitHubConsolidator(client, max_pages=params.max_pages)
    collector_ctx = CollectorContext(config=ctx.config, clock=ctx.clock)
    findings = list(consolidator.collect(collector_ctx, project))

    now = ctx.clock()
    sweep = ctx.observed.begin_sweep(
        collector=consolidator.name, scope=[project.id], at=now
    )
    stats = ctx.observed.append(sweep.id, findings, at=now)
    ctx.observed.finish_sweep(
        sweep.id,
        outcome="ok",
        stats={"projects": 1, "new": stats.new, "unchanged": stats.unchanged},
        at=ctx.clock(),
    )
    ctx.observed.rebuild_latest()

    by_kind: dict[str, int] = {}
    for entry in findings:
        by_kind[entry.kind] = by_kind.get(entry.kind, 0) + 1

    result = OnboardResult(
        project=project.slug,
        repo=project.repo_url,
        issues=by_kind.get(KIND_ISSUE, 0),
        pull_requests=by_kind.get(KIND_PULL_REQUEST, 0),
        labels=by_kind.get(KIND_LABEL, 0),
        releases=by_kind.get(KIND_RELEASE, 0),
        new=stats.new,
        unchanged=stats.unchanged,
        mutations=0,
    )
    # The largest read of an import, and for a long time the only step of one
    # left no trace: a consolidation that ran and one that was never run were
    # indistinguishable in `audit list`, which is how two of a five-project
    # batch were missed for an hour. Every return path records, including the
    # ones that read nothing — "the adapter is not configured" is still an
    # answer somebody asked for, and why they asked is the useful half.
    _record_onboard(ctx, project, params.reason, result)
    return result


def _record_onboard(
    ctx: AppContext, project: Project, reason: str, result: OnboardResult
) -> None:
    """Attribute one consolidation run to the principal who asked for it."""
    audited_action(
        ctx,
        operation="forge.onboard",
        reason=reason,
        entity_kind="project",
        entity_id=project.id,
        outcome=result.model_dump(mode="json"),
        event_kind=ONBOARDED_EVENT,
    )
