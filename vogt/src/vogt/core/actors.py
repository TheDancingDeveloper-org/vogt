"""Who caused an Inbox entry: a human or a bot, inside the org or outside it.

Pure classification over facts a collector already stored. Nothing here
fetches: the forge collectors resolve the triggering author and the owning
org's membership at collect time and write the raw facts onto the
observation, and every read — the Inbox list, its filter, the sidebar badge —
re-derives the answer from those stored facts with this module. That keeps
the read path free of network calls (the 0.5.4 lesson) while letting a
change to the configured bot list apply without waiting for a re-sweep.

The rules (operator decision 2026-10-04):

- **Bot** — the forge says so (`user.type == "Bot"`), or the login ends in
  `[bot]`, or the login is on the configured bot list. A bot is never
  external, whatever its association says.
- **Org member** — the login is on the repository's owning org's membership
  list, or the forge reports an `author_association` of `MEMBER`/`OWNER`
  (which also covers a member whose membership is private and so absent from
  a list the token can read).
- **External** — a human the org list does not contain. Outside collaborators
  are external. When the org list could not be read (a user-owned
  repository, or a token without org visibility), any other reported
  association means external.
- **Unknown** — no author could be resolved, or there is neither a list nor
  an association to judge by. Unknown is reported as such, never guessed.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from typing import Literal

ActorKind = Literal["human", "bot"]
ActorRelation = Literal["org_member", "external", "unknown"]
#: The `inbox.list` actor filter vocabulary.
ActorFilter = Literal["any", "external", "org", "bot"]

#: Logins treated as bots even when the forge does not mark them. Compared
#: case-insensitively against the login with any `[bot]` suffix removed.
DEFAULT_BOT_LOGINS: tuple[str, ...] = (
    "dependabot",
    "renovate",
    "renovate-bot",
    "github-actions",
)

_MEMBER_ASSOCIATIONS = frozenset({"MEMBER", "OWNER"})


@dataclass(frozen=True)
class ActorFacts:
    """What a collector recorded about the author of one occurrence."""

    login: str | None = None
    #: The forge's own account type (`User`, `Bot`, `Organization`), or None.
    user_type: str | None = None
    #: GitHub's `author_association` (`MEMBER`, `OWNER`, `COLLABORATOR`,
    #: `CONTRIBUTOR`, `NONE`, ...), or None when the forge does not report it.
    association: str | None = None
    #: Whether the login is on the owning org's membership list; None when the
    #: list could not be read.
    org_member: bool | None = None


@dataclass(frozen=True)
class ActorClass:
    """The derived answer, as the Inbox reports it."""

    login: str | None
    kind: ActorKind | None
    relation: ActorRelation


#: Every system-originated entry (drift, CI, agent) is attributed to the
#: instance itself: a bot, and internal.
SYSTEM_ACTOR = ActorClass(login=None, kind="bot", relation="org_member")


def normalise_bot_logins(logins: Iterable[str]) -> frozenset[str]:
    """Lower-case, `[bot]`-stripped, blank-free — the comparison form."""
    return frozenset(_base_login(login) for login in logins if login and login.strip())


def is_bot(login: str | None, user_type: str | None, bots: frozenset[str]) -> bool:
    if isinstance(user_type, str) and user_type.lower() == "bot":
        return True
    if not login:
        return False
    if login.lower().endswith("[bot]"):
        return True
    return _base_login(login) in bots


def classify(facts: ActorFacts, *, bots: frozenset[str]) -> ActorClass:
    """Classify one author from stored facts; never fetches."""
    if not facts.login and not facts.user_type:
        return ActorClass(login=None, kind=None, relation="unknown")
    if is_bot(facts.login, facts.user_type, bots):
        return ActorClass(login=facts.login, kind="bot", relation=_relation(facts))
    return ActorClass(login=facts.login, kind="human", relation=_relation(facts))


def facts_from_payload(raw: object) -> ActorFacts | None:
    """Read the `actor` block a collector wrote; None when it wrote none."""
    if not isinstance(raw, Mapping):
        return None

    def text(key: str) -> str | None:
        value = raw.get(key)
        return value if isinstance(value, str) and value else None

    org_member = raw.get("org_member")
    return ActorFacts(
        login=text("login"),
        user_type=text("user_type"),
        association=text("association"),
        org_member=org_member if isinstance(org_member, bool) else None,
    )


def matches(actor: ActorClass, wanted: ActorFilter) -> bool:
    """Whether an entry's actor passes the `inbox.list` actor filter.

    `external` and `org` are about *people*: a bot passes neither, and an
    unknown author passes neither (the caller reports how many unknowns the
    `external` filter hid).
    """
    if wanted == "any":
        return True
    if wanted == "bot":
        return actor.kind == "bot"
    if actor.kind != "human":
        return False
    if wanted == "external":
        return actor.relation == "external"
    return actor.relation == "org_member"


def _relation(facts: ActorFacts) -> ActorRelation:
    association = (facts.association or "").upper()
    if facts.org_member is True or association in _MEMBER_ASSOCIATIONS:
        return "org_member"
    if facts.org_member is False:
        return "external"
    if association:
        return "external"
    return "unknown"


def _base_login(login: str) -> str:
    lowered = login.strip().lower()
    return lowered.removesuffix("[bot]")


__all__ = [
    "DEFAULT_BOT_LOGINS",
    "SYSTEM_ACTOR",
    "ActorClass",
    "ActorFacts",
    "ActorFilter",
    "ActorKind",
    "ActorRelation",
    "classify",
    "facts_from_payload",
    "is_bot",
    "matches",
    "normalise_bot_logins",
]
