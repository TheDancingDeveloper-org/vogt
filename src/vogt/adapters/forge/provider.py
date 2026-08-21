"""The `ForgeProvider` seam — one interface, many forges (D2).

Everything Vogt does *to* or *reads from* a forge goes through this contract.
GitHub is the only implementation in v1; the interface is validated on paper
against GitLab and Gitea/Forgejo (see the module note below) so a second
forge is a new class, not a re-plumbing.

## The shape is a policy, not just a type

Two things about this protocol are load-bearing:

1. **There is no destructive verb.** The write surface is `comment`,
   `create_issue`, `add_labels`, `set_state` — append, append, append, and a
   reversible toggle. FR-B4 ("no deletion, no force, ever") is guaranteed here
   by construction: a provider *cannot* offer a delete because the interface
   it satisfies has no name for one.

2. **Capabilities are declared, never discovered.** A caller reads
   `capabilities` and adjusts — it never probes a forge to find out whether
   `since` filtering works and never silently collects nothing when it does
   not (FR-O11). A forge that cannot answer "what changed since X" forces a
   full re-read, and the watermark machinery in Phase 2 must *know* that
   rather than assume it away.

## Validated on paper against three forges (D2)

- **GitHub** — implemented. `GET /repos/{o}/{r}/issues?state=all&sort=updated`
  carries `since`; PRs come from `/pulls`; releases from `/releases`; checks
  from `/actions/runs`. Subject keys are `gh:{o}/{r}#{n}` (D5).
- **GitLab** — `GET /projects/{id}/issues?updated_after=…` and
  `/merge_requests`; releases at `/releases`; checks are pipelines at
  `/pipelines`. A merge request is a `ForgePull`; the id is URL-encoded
  `owner/repo`. `since` supported; posture differs (no repo-level Dependabot
  toggle) so `supports_posture` would be `False`.
- **Gitea / Forgejo** — GitHub-shaped by design: `/repos/{o}/{r}/issues`
  takes `since`; `/pulls`, `/releases` line up. No Actions-equivalent on older
  installs, so `checks` may be empty and `supports_webhooks` varies by
  version. This is the acceptance test of the interface in Phase 5 (#176).
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING, Protocol, runtime_checkable

from vogt.adapters.forge.models import (
    ForgeCapabilities,
    ForgeCheck,
    ForgeIssue,
    ForgeLabel,
    ForgeNotification,
    ForgePosture,
    ForgePull,
    ForgeRelease,
    ForgeRepo,
    RepoRef,
)

if TYPE_CHECKING:
    from vogt.adapters.forge.writeback import WriteBackResult


@runtime_checkable
class ForgeProvider(Protocol):
    """One forge, behind a stable contract (D2).

    `runtime_checkable` so a test can assert an implementation satisfies the
    surface without importing every method by name — the interface is the
    thing under test in Phase 5, and a structural check is what makes "the
    Forgejo provider passes the same contract" an assertion rather than a
    hope.
    """

    # -- identity ----------------------------------------------------------

    @property
    def capabilities(self) -> ForgeCapabilities:
        """What this forge can do, for a caller to read before it asks."""

    def list_repos(self) -> Iterable[ForgeRepo]:
        """Repositories the credential can see, for the import picker (#180).

        Enumeration, not discovery: it lists what *this credential* is entitled
        to, so a person can pick one to import — it never crawls, and the scope
        rule (FR-G15) is unbroken because the credential is the scope. A provider
        with no usable credential yields nothing; the caller tells that apart
        from "the account has no repositories" through the empty list plus its
        own knowledge of whether a token was configured.
        """

    def parse(self, repo_url: str | None) -> RepoRef | None:
        """Resolve a project's repository URL to this forge's identity.

        `None` is the ordinary answer for a URL this forge does not host — it
        means "not mine", not "malformed".
        """

    def subject_key(self, ref: RepoRef, number: int) -> str:
        """The stable key for issue/PR number `n` in `ref` (D5).

        The provider owns this scheme so a second forge cannot collide with
        github.com's keys, and so the one place that builds them is the one
        place that parses them back.
        """

    def number_of(self, subject_key: str | None) -> int | None:
        """The issue/PR number a subject key names, or `None`."""

    # -- read surface ------------------------------------------------------

    def issues_updated_since(
        self, ref: RepoRef, since: str | None
    ) -> Iterable[ForgeIssue]:
        """Issues touched since `since` (all states), or all when `since` is None.

        `since` is honoured only when `capabilities.supports_since`; a provider
        that cannot filter returns the current page and says so through the
        capability rather than by pretending the filter worked.
        """

    def pulls_updated_since(
        self, ref: RepoRef, since: str | None
    ) -> Iterable[ForgePull]:
        """Pull/merge requests touched since `since` (all states)."""

    def releases(self, ref: RepoRef) -> Iterable[ForgeRelease]:
        """Published releases, newest first."""

    def checks(self, ref: RepoRef) -> Iterable[ForgeCheck]:
        """Recent CI checks, as generic per-revision facts (FR-O6)."""

    def labels(self, ref: RepoRef) -> Iterable[ForgeLabel]:
        """Repository labels."""

    def posture(self, ref: RepoRef) -> ForgePosture:
        """Update-automation posture (FR-D6). Only meaningful when
        `capabilities.supports_posture`; a provider without it should not be
        asked, and the collector gates on the capability."""

    def notifications(self, ref: RepoRef) -> Iterable[ForgeNotification]:
        """Per-repository notifications (FR-O8). Gated on
        `capabilities.supports_notifications`."""

    # -- write surface (append-only by construction, FR-B4) ----------------

    def comment(self, ref: RepoRef, number: int, body: str) -> WriteBackResult:
        """Post a comment on a linked issue or pull request."""

    def create_issue(
        self,
        ref: RepoRef,
        *,
        title: str,
        body: str,
        labels: list[str] | None = None,
    ) -> WriteBackResult:
        """Open a new issue. Never edits or replaces an existing one."""

    def add_labels(
        self, ref: RepoRef, number: int, labels: list[str]
    ) -> WriteBackResult:
        """Add labels. Adds only — never replaces the existing set."""

    def set_state(self, ref: RepoRef, number: int, state: str) -> WriteBackResult:
        """Close or reopen. Both directions are recoverable by the other."""


__all__ = ["ForgeProvider"]
