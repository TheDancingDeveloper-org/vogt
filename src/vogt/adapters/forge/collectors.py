"""The forge read collectors, behind the provider (D2, #175).

Checks, releases, labels, posture and notifications — the read surface that
was GitHub-specific scraping until Phase 4. Each resolves its provider per
project and calls the `ForgeProvider` contract, so nothing here knows which
forge answered. Capability-gated: a provider that declares it cannot offer
posture or per-repo notifications yields a `not_supported` receipt rather than
an empty success (FR-O11), the same way the sync collectors do.

These carry no watermark — they are current-state reads, not incremental
history — so unlike the issue/PR sync they need no `subject_seen` bookkeeping.
"""

from __future__ import annotations

from collections.abc import Iterable

from vogt.adapters.forge.kinds import (
    COLLECTOR_CHECKS,
    COLLECTOR_LABELS,
    COLLECTOR_NOTIFICATIONS,
    COLLECTOR_POSTURE,
    COLLECTOR_RELEASES,
    KIND_CHECK,
    KIND_LABEL,
    KIND_NOTIFICATION,
    KIND_POSTURE,
    KIND_RELEASE,
    KIND_SYNC,
)
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.provider import ForgeProvider
from vogt.adapters.forge.registry import provider_for, unsupported_reason
from vogt.adapters.github.client import Transport
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.entities import Project


class _ForgeReadCollector:
    """Shared plumbing: resolve the provider, gate on capability, read."""

    name = ""
    #: The capability that must be declared for this read to be attempted, or
    #: None when every provider that exists can answer it.
    capability: str | None = None
    missing_detail = ""

    def __init__(self, *, transport: Transport | None = None) -> None:
        self._transport = transport

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        provider = provider_for(project.repo_url, ctx.config, transport=self._transport)
        ref = None if provider is None else provider.parse(project.repo_url)
        if provider is None or ref is None:
            yield self._receipt(
                project,
                supported=False,
                detail=unsupported_reason(project.repo_url, ctx.config),
            )
            return
        if self.capability is not None and not getattr(
            provider.capabilities, self.capability
        ):
            yield self._receipt(project, supported=False, detail=self.missing_detail)
            return
        count = 0
        for entry in self._read(provider, ref, project):
            count += 1
            yield entry
        yield self._receipt(project, supported=True, count=count, ref=ref)

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        raise NotImplementedError  # pragma: no cover - subclasses implement

    def _receipt(
        self,
        project: Project,
        *,
        supported: bool,
        count: int = 0,
        detail: str | None = None,
        ref: RepoRef | None = None,
    ) -> Finding:
        return finding(
            kind=KIND_SYNC,
            subject_key=(f"sync:{self.name}/{ref.slug if ref else project.id}"),
            project=project,
            payload={
                "collector": self.name,
                "supported": supported,
                "count": count,
                "repo": None if ref is None else ref.slug,
                "detail": detail,
            },
        )


class ForgeChecksCollector(_ForgeReadCollector):
    """CI checks, per revision (FR-O6). Replaces `gh-actions`."""

    name = COLLECTOR_CHECKS

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        for check in provider.checks(ref):
            yield finding(
                kind=KIND_CHECK,
                subject_key=f"ci:{ref.slug}@{check.revision}:{check.check}",
                project=project,
                source_url=check.source_url,
                payload={
                    "revision": check.revision,
                    "check": check.check,
                    "status": check.status,
                    "conclusion": check.conclusion,
                    "branch": check.branch,
                    "event": check.event,
                    "run_number": check.run_number,
                    "updated_at": check.updated_at,
                    "repo": ref.slug,
                },
            )


class ForgeReleasesCollector(_ForgeReadCollector):
    """Releases, where an observed version comes from (FR-P3). Replaces
    `gh-releases`."""

    name = COLLECTOR_RELEASES

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        for release in provider.releases(ref):
            yield finding(
                kind=KIND_RELEASE,
                subject_key=f"release:{ref.slug}@{release.tag}",
                project=project,
                source_url=release.source_url,
                payload={
                    "tag": release.tag,
                    "name": release.name,
                    "draft": release.draft,
                    "prerelease": release.prerelease,
                    "published_at": release.published_at,
                    "repo": ref.slug,
                    "source": "forge release",
                },
            )


class ForgeLabelsCollector(_ForgeReadCollector):
    """Repository labels, now on the read surface (v1 ceiling, #171)."""

    name = COLLECTOR_LABELS

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        for label in provider.labels(ref):
            yield finding(
                kind=KIND_LABEL,
                # Unchanged from the retired consolidator so observations share
                # a subject with any already stored (D5's reasoning, for labels).
                subject_key=f"ghlabel:{ref.slug}/{label.name}",
                project=project,
                payload={
                    "name": label.name,
                    "color": label.color,
                    "description": label.description,
                    "repo": ref.slug,
                },
            )


class ForgePostureCollector(_ForgeReadCollector):
    """Update-automation posture, three facts (FR-D6). Replaces `gh-posture`."""

    name = COLLECTOR_POSTURE
    capability = "supports_posture"
    missing_detail = (
        "this forge does not expose repository update-automation posture, so "
        "it is not collected here (FR-O11) — not that automation is off"
    )

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        posture = provider.posture(ref)
        yield finding(
            kind=KIND_POSTURE,
            subject_key=f"posture:{ref.slug}",
            project=project,
            payload={
                "version_updates_config": posture.version_updates_config,
                "version_updates": posture.version_updates,
                "vulnerability_alerts": posture.vulnerability_alerts,
                "automated_security_fixes": posture.automated_security_fixes,
                "repo": ref.slug,
            },
        )


class ForgeNotificationsCollector(_ForgeReadCollector):
    """Per-repository notifications (FR-O8). Replaces `gh-notifications`."""

    name = COLLECTOR_NOTIFICATIONS
    capability = "supports_notifications"
    missing_detail = (
        "this forge does not expose per-repository notifications, so they are "
        "not collected here (FR-O11)"
    )

    def _read(
        self, provider: ForgeProvider, ref: RepoRef, project: Project
    ) -> Iterable[Finding]:
        for note in provider.notifications(ref):
            yield finding(
                kind=KIND_NOTIFICATION,
                # Unchanged from the retired `gh-notifications` collector.
                subject_key=f"gh:{ref.slug}!{note.thread}",
                project=project,
                source_url=note.source_url,
                promoted=False,  # never promoted (FR-O8)
                payload={
                    "thread": note.thread,
                    "reason": note.reason,
                    "unread": note.unread,
                    "title": note.title,
                    "subject_type": note.subject_type,
                    "updated_at": note.updated_at,
                    "last_read_at": note.last_read_at,
                    "repo": ref.slug,
                    "source": "forge notification",
                },
            )


def forge_read_collectors(
    *, transport: Transport | None = None
) -> list[_ForgeReadCollector]:
    """The provider-backed read collectors (checks, releases, labels, posture,
    notifications). Registered whenever a forge is configured; the provider is
    resolved per project (D4)."""
    return [
        ForgeChecksCollector(transport=transport),
        ForgeReleasesCollector(transport=transport),
        ForgeLabelsCollector(transport=transport),
        ForgePostureCollector(transport=transport),
        ForgeNotificationsCollector(transport=transport),
    ]


__all__ = [
    "ForgeChecksCollector",
    "ForgeLabelsCollector",
    "ForgeNotificationsCollector",
    "ForgePostureCollector",
    "ForgeReleasesCollector",
    "forge_read_collectors",
]
