"""Resolve who caused a notification, at collect time and bounded.

A forge notification thread names no author. The collector follows the
thread's `latest_comment_url` (or, without one, the subject itself) to read
the author, and the owning org's member list to tell a member from an
outsider. Both reads are network calls, so both are cached and both happen
here — at collect time — and never on a read path: the Inbox, its filter
and the sidebar badge classify from the facts stored on the observation
(`vogt.core.actors`).

Bounds:

- **Per thread** — an author is resolved once per `(url, updated_at)` (an
  unresolvable one is retried on a later sweep, within the budget). The
  previous observation of the same thread is the persistent cache (it
  survives a restart); a small in-process map covers the rest.
- **Per sweep** — at most `RESOLVE_BUDGET` author reads per project. The
  remainder stay unknown this sweep and are resolved on the next, so a first
  sweep over a busy repository cannot spend the hourly rate limit.
- **Org lists** — one list read per `(host, owner)` per `ORG_TTL_SECONDS`,
  and a failed read is remembered (briefly) as "unknown" rather than retried
  per thread.
"""

from __future__ import annotations

import time
from collections import OrderedDict
from collections.abc import Callable, Mapping
from dataclasses import dataclass, field

from vogt.adapters.forge.models import ForgeActor, ForgeNotification, RepoRef
from vogt.adapters.forge.provider import ForgeProvider
from vogt.errors import VogtError
from vogt.observability import logger

log = logger("forge.actors")

#: Author reads one project's notification sweep may spend.
RESOLVE_BUDGET = 30
#: How long an org member list is trusted before it is read again.
ORG_TTL_SECONDS = 3600.0
#: How long a failed org list read is remembered as "unknown".
ORG_FAILURE_TTL_SECONDS = 300.0
_AUTHOR_CACHE_SIZE = 4096

#: GitHub reports workflow runs as `CheckSuite` threads with no subject URL;
#: the author of a CI notification is the Actions app.
_CHECK_SUITE_ACTOR = ForgeActor(login="github-actions[bot]", user_type="Bot")


@dataclass
class _OrgEntry:
    members: frozenset[str] | None
    expires: float


@dataclass
class ActorResolver:
    """Process-wide caches for author and org-membership reads."""

    clock: Callable[[], float] = time.monotonic
    _authors: OrderedDict[tuple[str, str], ForgeActor] = field(
        default_factory=OrderedDict
    )
    _orgs: dict[tuple[str, str], _OrgEntry] = field(default_factory=dict)

    def clear(self) -> None:
        self._authors.clear()
        self._orgs.clear()

    def org_members(
        self, provider: ForgeProvider, ref: RepoRef
    ) -> frozenset[str] | None:
        key = (ref.host, ref.owner.lower())
        now = self.clock()
        cached = self._orgs.get(key)
        if cached is not None and cached.expires > now:
            return cached.members
        try:
            members = provider.org_members(ref.owner)
        except VogtError as error:
            log.info(
                "org member list unavailable",
                extra={"vogt": {"owner": ref.owner, "error": str(error)}},
            )
            self._orgs[key] = _OrgEntry(None, now + ORG_FAILURE_TTL_SECONDS)
            return None
        self._orgs[key] = _OrgEntry(members, now + ORG_TTL_SECONDS)
        return members

    def actor_block(
        self,
        provider: ForgeProvider,
        ref: RepoRef,
        note: ForgeNotification,
        *,
        prior: Mapping[str, object] | None,
        budget: list[int],
    ) -> dict[str, object] | None:
        """The `actor` payload block for one thread, or None when unknown.

        `budget` is a one-element counter shared across a project's threads;
        a resolution that needs the network spends one, and an exhausted
        budget (or a forge that refused once this sweep) leaves the rest
        unresolved until the next sweep.
        """
        url = note.latest_comment_api_url or note.subject_api_url
        stamp = note.updated_at or ""
        actor: ForgeActor | None
        if url is None:
            if note.subject_type != "CheckSuite":
                return None
            actor = _CHECK_SUITE_ACTOR
        else:
            actor = _from_prior(prior, url, stamp)
            if actor is None:
                key = (url, stamp)
                if key in self._authors:
                    self._authors.move_to_end(key)
                    actor = self._authors[key]
                elif budget[0] <= 0:
                    return None
                else:
                    budget[0] -= 1
                    try:
                        actor = provider.resolve_actor(url)
                    except VogtError as error:
                        # Rate-limited or refused: stop spending on this
                        # project for this sweep; the threads stay unknown.
                        budget[0] = 0
                        log.info(
                            "notification author unavailable",
                            extra={"vogt": {"repo": ref.slug, "error": str(error)}},
                        )
                        return None
                    # Only an answer is cached. "No author" (a 404, a resource
                    # naming nobody) is retried on a later sweep, within the
                    # same budget, rather than remembered as final.
                    if actor is not None:
                        self._authors[key] = actor
                        while len(self._authors) > _AUTHOR_CACHE_SIZE:
                            self._authors.popitem(last=False)
            if actor is None:
                return None
        org_member: bool | None = None
        if actor.login and (actor.user_type or "").lower() != "bot":
            members = self.org_members(provider, ref)
            if members is not None:
                org_member = actor.login.lower() in members
        return {
            "login": actor.login,
            "user_type": actor.user_type,
            "association": actor.association,
            "org_member": org_member,
            "resolved_from": url,
            "resolved_for": stamp or None,
        }


def _from_prior(
    prior: Mapping[str, object] | None, url: str, stamp: str
) -> ForgeActor | None:
    """Reuse the author the previous observation resolved for the same
    `(url, updated_at)` — the cache that survives a restart."""
    if prior is None:
        return None
    block = prior.get("actor")
    if not isinstance(block, Mapping):
        return None
    if block.get("resolved_from") != url or (block.get("resolved_for") or "") != stamp:
        return None
    login = block.get("login")
    user_type = block.get("user_type")
    association = block.get("association")
    return ForgeActor(
        login=login if isinstance(login, str) else None,
        user_type=user_type if isinstance(user_type, str) else None,
        association=association if isinstance(association, str) else None,
    )


#: The one resolver the collectors share for the life of the process.
RESOLVER = ActorResolver()

__all__ = [
    "ORG_TTL_SECONDS",
    "RESOLVER",
    "RESOLVE_BUDGET",
    "ActorResolver",
]
