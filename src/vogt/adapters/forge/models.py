"""Normalized forge objects, shared across every provider.

These are the shapes the rest of Vogt will eventually see instead of a raw
GitHub JSON payload (Phase 4). A provider reads its own API and returns one
of these; a caller never learns which forge answered from the shape it gets
back. Only the *keys* carry a provider's fingerprint (`gh:` for github.com),
and even that is the provider's decision, made in one place (D5).

Deliberately dataclasses rather than the pydantic models used for declared
entities: an observation is not a declared write, it never round-trips
through storage as one of these, and the adapter layer must stay installable
without pulling declared-side machinery in (NFR-PO1).
"""

from __future__ import annotations

from dataclasses import dataclass, field


@dataclass(frozen=True)
class RepoRef:
    """A repository, resolved to the identity its host understands.

    `host` is what makes a future provider's keys unambiguous: two forges can
    each hold an `acme/widgets`, and the host is the only thing that tells
    them apart. For github.com the host is carried but not spelled into the
    subject key, because rekeying every existing observation to gain a prefix
    it never needed is churn with no reader (D5).
    """

    host: str
    owner: str
    repo: str

    @property
    def slug(self) -> str:
        """`owner/repo`, the half of the identity a person recognises."""
        return f"{self.owner}/{self.repo}"


@dataclass(frozen=True)
class ForgeCapabilities:
    """What a provider can and cannot do, declared rather than discovered.

    A caller reads this instead of catching a `NotImplementedError` or, worse,
    silently collecting nothing (FR-O11). `supports_since` is the one Phase 2
    turns on its head: a forge that cannot filter by update time forces a full
    re-read every sweep, which the watermark machinery has to know about
    rather than assume away.
    """

    hosts: tuple[str, ...]
    supports_since: bool
    supports_posture: bool
    supports_notifications: bool
    supports_webhooks: bool


@dataclass(frozen=True)
class ForgeIssue:
    """One issue, normalized. The state is `open` or `closed`, never a code."""

    number: int
    title: str
    state: str
    repo: str
    labels: tuple[str, ...] = ()
    author: str | None = None
    assignees: tuple[str, ...] = ()
    comments: int = 0
    updated_at: str | None = None
    closed_at: str | None = None
    source_url: str | None = None


@dataclass(frozen=True)
class ForgePull:
    """One pull/merge request, normalized across the forges' two names for it."""

    number: int
    title: str
    state: str
    repo: str
    draft: bool = False
    author: str | None = None
    head: str | None = None
    base: str | None = None
    labels: tuple[str, ...] = ()
    updated_at: str | None = None
    closed_at: str | None = None
    source_url: str | None = None


@dataclass(frozen=True)
class ForgeRelease:
    """One release, which is where an observed version comes from (FR-P3)."""

    tag: str
    repo: str
    name: str | None = None
    draft: bool = False
    prerelease: bool = False
    published_at: str | None = None
    source_url: str | None = None


@dataclass(frozen=True)
class ForgeCheck:
    """One check on one revision (FR-O6). Actions is a producer, not the model."""

    revision: str
    check: str
    repo: str
    status: str | None = None
    conclusion: str | None = None
    branch: str | None = None
    event: str | None = None
    run_number: int | None = None
    updated_at: str | None = None
    source_url: str | None = None
    extra: dict[str, object] = field(default_factory=dict)


__all__ = [
    "ForgeCapabilities",
    "ForgeCheck",
    "ForgeIssue",
    "ForgePull",
    "ForgeRelease",
    "RepoRef",
]
