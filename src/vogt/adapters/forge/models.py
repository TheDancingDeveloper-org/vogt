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
class ForgeRepo:
    """One repository a credential can see, for the import picker (#180).

    Provider-agnostic on purpose: a picker built on this never learns which
    forge answered. `already_registered` is deliberately *not* here — a provider
    reads a forge and knows nothing of what this Vogt instance has registered,
    so that field is computed by the service against the declared project list,
    not by the adapter. `web_url` is what the service matches a registered
    project's `repo_url` against, and what an import is then driven with.
    """

    owner: str
    name: str
    default_branch: str | None
    #: "public" or "private", as the forge reports its visibility.
    visibility: str
    web_url: str

    @property
    def slug(self) -> str:
        """`owner/name`, the half of the identity a person recognises."""
        return f"{self.owner}/{self.name}"


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
    #: The issue body. Carried so an initiative tracking issue (#286) can be
    #: recognised by its managed marker and its checkbox states read back —
    #: `None` is "the forge did not include it", never "empty".
    body: str | None = None
    updated_at: str | None = None
    closed_at: str | None = None
    source_url: str | None = None


@dataclass(frozen=True)
class ForgePull:
    """One pull/merge request, normalized across the forges' two names for it.

    `state` is `open` or `closed`; `merged` is the separate fact a closed PR
    needs, because a merged PR and an abandoned one both read `closed` and only
    one of them shipped. `head_ref` is the branch name (distinct from `head`,
    the tip SHA) — the half the PR↔work-item edge reads (#284). `review_state`,
    `mergeable` and `checks` are the reviewability rollups a forge exposes on a
    single-PR read; `None` is "the forge did not say", never "false".
    """

    number: int
    title: str
    state: str
    repo: str
    draft: bool = False
    merged: bool = False
    author: str | None = None
    head: str | None = None
    head_ref: str | None = None
    base: str | None = None
    body: str | None = None
    labels: tuple[str, ...] = ()
    review_state: str | None = None
    mergeable: str | None = None
    checks: str | None = None
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
class ForgeLabel:
    """One repository label."""

    name: str
    repo: str
    color: str | None = None
    description: str | None = None


@dataclass(frozen=True)
class ForgePosture:
    """Update-automation posture, three independent facts (FR-D6).

    `None` is "could not tell", distinct from `False` ("told, and off"): a
    provider whose forge cannot answer one of these reports the gap honestly
    rather than as an absence."""

    version_updates_config: str | None
    vulnerability_alerts: bool | None
    automated_security_fixes: bool | None
    repo: str = ""

    @property
    def version_updates(self) -> bool:
        return self.version_updates_config is not None


@dataclass(frozen=True)
class ForgeNotification:
    """One notification thread for a registered repository (FR-O8)."""

    thread: str
    repo: str
    reason: str | None = None
    unread: bool = False
    title: str = ""
    subject_type: str | None = None
    updated_at: str | None = None
    last_read_at: str | None = None
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
    "ForgeLabel",
    "ForgeNotification",
    "ForgePosture",
    "ForgePull",
    "ForgeRelease",
    "ForgeRepo",
    "RepoRef",
]
