"""Write-back policy and result, provider-agnostic (FR-B1–B5, #175).

The shape is the policy: the actions a level permits are `create`, `comment`,
`label`, `close`/`reopen` — append, append, append, and a reversible toggle.
There is no destructive verb here or on the `ForgeProvider` write surface, so
FR-B4 ("no deletion, no force, ever") holds by construction rather than by
vigilance. These types live in the forge layer, not in any one provider, so a
second forge writes back through the same policy and reports the same result.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

WriteBackPolicy = Literal["none", "comment_only", "full"]
WriteBackAction = Literal["create", "comment", "label", "close", "reopen"]

#: Which actions each policy level permits. `none` is deliberately an empty
#: set rather than a missing key: "this project has no policy" and "this
#: project's policy allows nothing" should not be different code paths.
PERMITTED: dict[str, frozenset[str]] = {
    "none": frozenset(),
    "comment_only": frozenset({"comment"}),
    "full": frozenset({"create", "comment", "label", "close", "reopen"}),
}


def permits(policy: str, action: str) -> bool:
    return action in PERMITTED.get(policy, frozenset())


@dataclass(frozen=True)
class WriteBackResult:
    """What happened upstream, for the ledger."""

    outcome: Literal["succeeded", "failed", "skipped"]
    detail: str | None = None
    source_url: str | None = None
    subject_key: str | None = None


__all__ = [
    "PERMITTED",
    "WriteBackAction",
    "WriteBackPolicy",
    "WriteBackResult",
    "permits",
]
