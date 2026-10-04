"""The conflict policy an applying `import` follows, as one pure decision.

An import merges an export of instance A into a live instance B. For every
entity both sides hold, the question is which version wins, and the answer
needs a *baseline*: the moment the two last agreed. `clone` records one (the
backup's as-of time), so an instance that was cloned from the other — or
cloned the other — has a common past to measure change against. Without a
baseline nothing can tell "changed here" from "changed there", so every
difference is a conflict rather than a guess.

| Same content | Changed since base   | Decision                         |
|--------------|----------------------|----------------------------------|
| yes          | —                    | `unchanged`                      |
| no           | incoming only        | `take_incoming`                  |
| no           | target only          | `keep_target`                    |
| no           | both, or neither     | `conflict`                       |
| no           | no baseline          | `conflict`                       |

"Neither changed, yet they differ" means the baseline is wrong for this pair,
so it is a conflict too: the policy never resolves a difference it cannot
explain. A conflict keeps the target and records the incoming version beside
it (or, under `--strict`, fails the whole import).
"""

from __future__ import annotations

from datetime import datetime
from typing import Literal

MergeDecision = Literal["unchanged", "take_incoming", "keep_target", "conflict"]


def decide(
    *,
    equal: bool,
    base: datetime | None,
    target_updated_at: datetime,
    incoming_updated_at: datetime,
) -> MergeDecision:
    """Which version of one entity an import keeps. See the module table."""
    if equal:
        return "unchanged"
    if base is None:
        return "conflict"
    target_changed = target_updated_at > base
    incoming_changed = incoming_updated_at > base
    if incoming_changed and not target_changed:
        return "take_incoming"
    if target_changed and not incoming_changed:
        return "keep_target"
    return "conflict"
