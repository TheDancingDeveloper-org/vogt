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

from vogt.adapters.forge import (
    KIND_ISSUE,
    KIND_LABEL,
    KIND_PULL_REQUEST,
    KIND_RELEASE,
    ForgeProvider,
    provider_for,
    unsupported_reason,
)
from vogt.adapters.forge.collectors import forge_read_collectors
from vogt.adapters.forge.sync import forge_sync_collectors
from vogt.adapters.forge.writeback import permits
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
from vogt.collectors import Sweeper
from vogt.collectors.base import CollectorContext
from vogt.core.entities import Actor, Project, WorkItem, WriteBackRecord

WRITEBACK_SET = "project.writeback"
WRITEBACK_SET_EVENT = "project.writeback_set"
#: One consolidation run, attributed to whoever asked for it (FR-S1).
ONBOARDED_EVENT = "forge.onboarded"

#: The read collectors onboarding runs alongside the issue/PR sync: labels and
#: releases are current-state history. Checks/notifications/posture are live
#: signals, not backfill, so a sweep — not onboarding — is where they belong.
_ONBOARD_READS = frozenset({"forge-labels", "forge-releases"})


def _count_latest(ctx: AppContext, project_id: str, kind: str) -> int:
    return len(ctx.observed.latest(kinds=(kind,), project_id=project_id, limit=100_000))


@dataclass(frozen=True)
class Attempt:
    """One write-back, as it will be recorded."""

    action: str
    work_item: WorkItem | None
    project: Project | None
    body: str | None = None
    labels: tuple[str, ...] = ()
    state: str | None = None


def writer_for(ctx: AppContext) -> ForgeProvider | None:
    """A configured provider, or `None` when no forge is set up.

    `None` is the ordinary case, and it is not a failure: an instance with no
    forge token simply never speaks upstream. Write-back goes through the
    provider's append-only write surface now (#175), not a GitHub-specific
    writer — the policy machinery (`permits`, the ledger) is unchanged. The
    per-action provider is resolved from the project's own `repo_url`; this
    only answers "is any forge configured to write at all".
    """
    from vogt.adapters.forge import github_provider

    return github_provider(ctx.config)


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

    repo_url = None if project is None else project.repo_url
    # The whole write goes through the provider surface now (#175): it owns the
    # key scheme (`number_of`), the append-only verbs, and — being resolved
    # from the project's own `repo_url` — which forge to speak to.
    #
    # Attribution (#179): when the acting actor has linked their own PAT for
    # this host, speak upstream as *them*; otherwise the instance file token
    # (FR-S7) is the fallback, which is what every sweep and unlinked actor
    # uses. A linked actor can write even when the instance has no file token
    # at all, so the identity is chosen before the "no forge configured" gate,
    # not after it. Choosing the identity changes none of the policy machinery
    # below — only whose credential the write lands under.
    provider, writer_identity = _writer_provider(ctx, actor, repo_url)
    if provider is None:
        return record.model_copy(
            update={"detail": "no forge is configured to write to for this project"}
        )
    ref = None if repo_url is None else provider.parse(repo_url)
    number = provider.number_of(subject_key)
    if ref is None and action != "create":
        return record.model_copy(
            update={"detail": "this project has no forge repository to write to"}
        )
    if action == "comment" and ref is not None and number is not None and body:
        result = provider.comment(ref, number, body)
    elif action == "label" and ref is not None and number is not None and labels:
        result = provider.add_labels(ref, number, list(labels))
    elif action in ("close", "reopen") and ref is not None and number is not None:
        result = provider.set_state(
            ref, number, "closed" if action == "close" else "open"
        )
    elif action == "create" and ref is not None and item is not None:
        result = provider.create_issue(
            ref,
            title=item.title,
            body=item.body or f"Filed from Vogt as {item.ref}.",
            labels=list(item.labels),
        )
    else:
        return record.model_copy(update={"detail": f"nothing to send for {action}"})

    return record.model_copy(
        update={
            "outcome": result.outcome,
            "detail": _detail_with_identity(result.detail, writer_identity),
            "source_url": result.source_url,
            "subject_key": result.subject_key or subject_key,
        }
    )


#: What the ledger records when no actor PAT applied — a sweep, an unlinked
#: actor, or an instance with linking switched off.
_FILE_TOKEN_IDENTITY = "instance file token"


def _writer_provider(
    ctx: AppContext, actor: Actor, repo_url: str | None
) -> tuple[ForgeProvider | None, str]:
    """Pick the provider a write lands under, and name whose identity it is.

    Prefers the acting actor's linked PAT for the target host — which works
    even when the instance has no file token — and falls back to the file-token
    provider (`None` when neither is configured). The actor lookup is skipped
    entirely when linking is not configured, so an instance with no key pays
    nothing for the feature.
    """
    # A token-less parse to learn the host, so the actor's PAT can be found even
    # with no file token to build a provider from. GitHub-only in v1, which is
    # the same ceiling the whole write path already holds.
    from vogt.adapters.forge import github_identity
    from vogt.adapters.forge.accounts import account_linking_enabled, load_cipher
    from vogt.adapters.forge.github import GitHubProvider
    from vogt.adapters.github.client import GitHubClient

    gh_ref = None if repo_url is None else github_identity().parse(repo_url)
    if gh_ref is not None and account_linking_enabled(ctx.config):
        with ctx.declared.read() as view:
            secret = view.forge_account_secret(actor_id=actor.id, host=gh_ref.host)
            account = view.forge_account(actor_id=actor.id, host=gh_ref.host)
        if secret is not None and account is not None:
            pat = load_cipher(ctx.config).decrypt(secret)
            client = GitHubClient(token=pat, transport=ctx.forge_transport)
            return GitHubProvider(client), account.login

    file_provider = provider_for(repo_url, ctx.config, transport=ctx.forge_transport)
    return file_provider, _FILE_TOKEN_IDENTITY


def _detail_with_identity(detail: str | None, writer_identity: str) -> str:
    """Fold the writing identity into the ledger row's detail.

    Recorded on every non-skipped attempt so `forge actions` shows whether a
    write went up as an actor or as the file token (#179). Skipped attempts
    return earlier and keep their policy explanation untouched.
    """
    prefix = f"as {writer_identity}"
    return prefix if detail is None else f"{prefix}; {detail}"


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

    # Asked before the adapter is even built: "this host is not supported" and
    # "the adapter is not configured" are different answers, and both are
    # different from "there is nothing there".
    unsupported = unsupported_reason(project.repo_url)
    if unsupported is not None:
        refused = OnboardResult(
            project=project.slug,
            repo=project.repo_url,
            issues=0,
            pull_requests=0,
            labels=0,
            releases=0,
            new=0,
            unchanged=0,
            supported=False,
            detail=unsupported,
        )
        _record_onboard(ctx, project, params.reason, refused)
        return refused

    provider = provider_for(project.repo_url, ctx.config)
    if provider is None:
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
                "the forge adapter is not configured, so nothing was read — "
                "which is 'not collected', not 'there is nothing'"
            ),
        )
        _record_onboard(ctx, project, params.reason, unconfigured)
        return unconfigured

    # Onboarding is "reset the watermark and sync now" (#173): the same
    # read path a sweep walks, from the start of history, plus the current-state
    # read collectors for labels and releases. Zero mutations by construction —
    # every collector here only ever reads (FR-B3).
    now = ctx.clock()
    sync = forge_sync_collectors(ctx.observed)
    for collector in sync:
        collector.reset_watermark(project.id, at=now)
    reads = [c for c in forge_read_collectors() if c.name in _ONBOARD_READS]
    sweeper = Sweeper(
        ctx.observed, CollectorContext(config=ctx.config, clock=ctx.clock)
    )
    reports = sweeper.run([*sync, *reads], [project])
    ctx.observed.rebuild_latest()

    new = sum(r.new for r in reports)
    unchanged = sum(r.unchanged for r in reports)
    result = OnboardResult(
        project=project.slug,
        repo=project.repo_url,
        issues=_count_latest(ctx, project.id, KIND_ISSUE),
        pull_requests=_count_latest(ctx, project.id, KIND_PULL_REQUEST),
        labels=_count_latest(ctx, project.id, KIND_LABEL),
        releases=_count_latest(ctx, project.id, KIND_RELEASE),
        new=new,
        unchanged=unchanged,
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
