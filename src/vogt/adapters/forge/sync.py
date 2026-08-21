"""Incremental all-state forge sync (D3, #173).

The collectors this replaces asked the forge for its *open* issues and PRs and
nothing else, so a closure upstream was never an event Vogt could see — the
subject simply stopped appearing, and its last-seen `state: open` sat in the
projection forever (#169). This reads `state=all` incrementally instead: each
sweep asks "what changed since I last looked", so a close is a fact that
arrives like any other, and the ranked view self-heals.

Two pieces of per-collector bookkeeping make that work, both in the observed
store because neither is a fact a person asserted (D1):

- a **watermark** per (collector, project): the max upstream `updated_at`
  seen, so the next sweep fetches only what moved since (with a small overlap;
  digest dedup absorbs the replays);
- **`subject_seen`**: every subject confirmed to still exist this sweep, so
  trust can be read from "last confirmed" rather than "last changed" (Phase 3).

Both are written *after* the append commits (`after_append`), never before: a
watermark that advanced past observations that failed to persist would skip
them forever.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass, field
from datetime import datetime, timedelta

from vogt.adapters.forge.kinds import (
    COLLECTOR_ISSUES,
    COLLECTOR_PULLS,
    KIND_ISSUE,
    KIND_PULL_REQUEST,
    KIND_SYNC,
)
from vogt.adapters.forge.models import ForgeIssue, ForgePull, RepoRef
from vogt.adapters.forge.provider import ForgeProvider
from vogt.adapters.forge.registry import provider_for, unsupported_reason
from vogt.adapters.github.client import Transport
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.clock import from_iso, to_iso
from vogt.core.entities import Project
from vogt.storage.interface import ObservedStore

#: Re-ask for a small window before the watermark, so a subject updated in the
#: same second as the boundary is not skipped. Digest dedup means the overlap
#: costs nothing but a few unchanged reads.
OVERLAP = timedelta(seconds=60)


@dataclass
class _Pending:
    """A project's watermark advance and confirmed subjects, awaiting commit."""

    watermark: str | None
    subject_keys: list[str] = field(default_factory=list)


class _ForgeSyncCollector:
    """Shared incremental-sync plumbing; subclasses pick issues vs PRs."""

    name = ""
    kind = ""

    def __init__(
        self, store: ObservedStore, *, transport: Transport | None = None
    ) -> None:
        self._store = store
        self._transport = transport
        self._pending: dict[str, _Pending] = {}

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        provider = provider_for(project.repo_url, ctx.config, transport=self._transport)
        ref = None if provider is None else provider.parse(project.repo_url)
        if provider is None or ref is None:
            # Either no forge hosts this, or the one that does is not
            # configured. Both are "not collected", and the receipt says which
            # so a zero here is never read as "there is nothing" (FR-O4/O11).
            yield self._receipt(project, ref=None, supported=False, count=0)
            return

        watermark = self._store.get_watermark(
            collector=self.name, project_id=project.id
        )
        since = _since_of(watermark)
        items = list(self._fetch(provider, ref, since))

        newest = None if watermark is None else from_iso(watermark)
        subject_keys: list[str] = []
        for item in items:
            key = provider.subject_key(ref, item.number)
            subject_keys.append(key)
            moved = _moment(item.updated_at)
            if moved is not None and (newest is None or moved > newest):
                newest = moved
            yield self._finding(provider, ref, project, item)

        self._pending[project.id] = _Pending(
            watermark=None if newest is None else to_iso(newest),
            subject_keys=subject_keys,
        )
        yield self._receipt(
            project,
            ref=ref,
            supported=True,
            count=len(items),
            # A full page means the forge may hold more that moved since the
            # watermark; the next sweep continues from where this one reached.
            truncated=len(items) >= _PAGE,
            watermark=None if newest is None else to_iso(newest),
        )

    def after_append(self, *, at: datetime) -> None:
        """Commit each project's watermark and confirmations post-append (D1)."""
        for project_id, pending in self._pending.items():
            self._store.set_watermark(
                collector=self.name,
                project_id=project_id,
                watermark=pending.watermark,
                at=at,
            )
            self._store.touch_subjects(pending.subject_keys, at=at)
        self._pending.clear()

    def reset_watermark(self, project_id: str, *, at: datetime) -> None:
        """Forget progress for one project, so the next sync backfills it.

        `forge onboard` is exactly "reset the watermark and sync now" (D3):
        the same read path a sweep uses, walked from the start of history.
        """
        self._store.set_watermark(
            collector=self.name, project_id=project_id, watermark=None, at=at
        )

    # -- subclass surface --------------------------------------------------

    def _fetch(
        self, provider: ForgeProvider, ref: RepoRef, since: str | None
    ) -> Iterable[ForgeIssue | ForgePull]:
        raise NotImplementedError  # pragma: no cover - subclasses implement

    def _finding(
        self,
        provider: ForgeProvider,
        ref: RepoRef,
        project: Project,
        item: ForgeIssue | ForgePull,
    ) -> Finding:
        raise NotImplementedError  # pragma: no cover - subclasses implement

    def _receipt(
        self,
        project: Project,
        *,
        ref: RepoRef | None,
        supported: bool,
        count: int,
        truncated: bool = False,
        watermark: str | None = None,
    ) -> Finding:
        reason = None if supported else unsupported_reason(project.repo_url)
        return finding(
            kind=KIND_SYNC,
            subject_key=f"sync:{self.name}/{ref.slug if ref else project.id}",
            project=project,
            payload={
                "collector": self.name,
                "supported": supported,
                "count": count,
                "truncated": truncated,
                "watermark": watermark,
                "repo": None if ref is None else ref.slug,
                "detail": reason,
            },
        )


_PAGE = 100


class ForgeIssuesCollector(_ForgeSyncCollector):
    """Issues, open and closed, synced incrementally (replaces `gh-issues`)."""

    name = COLLECTOR_ISSUES
    kind = KIND_ISSUE

    def _fetch(
        self, provider: ForgeProvider, ref: RepoRef, since: str | None
    ) -> Iterable[ForgeIssue | ForgePull]:
        return provider.issues_updated_since(ref, since)

    def _finding(
        self,
        provider: ForgeProvider,
        ref: RepoRef,
        project: Project,
        item: ForgeIssue | ForgePull,
    ) -> Finding:
        assert isinstance(item, ForgeIssue)
        return finding(
            kind=KIND_ISSUE,
            subject_key=provider.subject_key(ref, item.number),
            project=project,
            source_url=item.source_url,
            # Open issues are backlog; closed history is context, not backlog —
            # promoting it would drop years of finished work into the ranked
            # view the day a repository first syncs.
            promoted=item.state == "open",
            payload={
                "number": item.number,
                "title": item.title,
                "state": item.state,
                "labels": list(item.labels),
                "author": item.author,
                "assignees": list(item.assignees),
                "comments": item.comments,
                "updated_at": item.updated_at,
                "closed_at": item.closed_at,
                "repo": ref.slug,
            },
        )


class ForgePullsCollector(_ForgeSyncCollector):
    """Pull requests, open and closed, synced incrementally (replaces `gh-prs`)."""

    name = COLLECTOR_PULLS
    kind = KIND_PULL_REQUEST

    def _fetch(
        self, provider: ForgeProvider, ref: RepoRef, since: str | None
    ) -> Iterable[ForgeIssue | ForgePull]:
        return provider.pulls_updated_since(ref, since)

    def _finding(
        self,
        provider: ForgeProvider,
        ref: RepoRef,
        project: Project,
        item: ForgeIssue | ForgePull,
    ) -> Finding:
        assert isinstance(item, ForgePull)
        return finding(
            kind=KIND_PULL_REQUEST,
            subject_key=provider.subject_key(ref, item.number),
            project=project,
            source_url=item.source_url,
            payload={
                "number": item.number,
                "title": item.title,
                "state": item.state,
                "draft": item.draft,
                "author": item.author,
                "head": item.head,
                "base": item.base,
                "updated_at": item.updated_at,
                "closed_at": item.closed_at,
                "repo": ref.slug,
            },
        )


def forge_sync_collectors(
    store: ObservedStore, *, transport: Transport | None = None
) -> list[_ForgeSyncCollector]:
    """The incremental sync collectors — always the pair; the provider is
    resolved per project, so registration is not per host (D4)."""
    return [
        ForgeIssuesCollector(store, transport=transport),
        ForgePullsCollector(store, transport=transport),
    ]


def _since_of(watermark: str | None) -> str | None:
    """The `since` to ask the forge for: the watermark, less the overlap."""
    if watermark is None:
        return None
    return _github_iso(from_iso(watermark) - OVERLAP)


def _moment(updated_at: str | None) -> datetime | None:
    if not updated_at:
        return None
    try:
        return from_iso(updated_at)
    except ValueError:  # pragma: no cover - a malformed timestamp is not a crash
        return None


def _github_iso(moment: datetime) -> str:
    """`Z`-suffixed ISO, the form the forge `since` parameters expect."""
    return moment.strftime("%Y-%m-%dT%H:%M:%SZ")


__all__ = [
    "KIND_ISSUE",
    "KIND_PULL_REQUEST",
    "KIND_SYNC",
    "ForgeIssuesCollector",
    "ForgePullsCollector",
    "forge_sync_collectors",
]
