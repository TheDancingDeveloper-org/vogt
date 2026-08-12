"""Who is acting.

FR-S2: the acting principal is derived from *authentication only* and is
never a caller-suppliable field. At M0 the only authentication path is the
local one — the OS user running the process — which is exactly what
`DEPLOYMENT.md` §3 prescribes for the loopback topology. Token-derived
principals arrive at M4 and plug in here; nothing downstream needs to change,
because nothing downstream ever sees where the principal came from.
"""

from __future__ import annotations

import getpass
import os
from dataclasses import dataclass
from typing import Literal

ActorKind = Literal["human", "agent"]

LOCAL_SCHEME = "local"


@dataclass(frozen=True)
class Principal:
    """An authenticated identity, ready to be bound to an Actor row."""

    identity_ref: str
    kind: ActorKind
    display_name: str

    def __post_init__(self) -> None:
        if not self.identity_ref.strip():
            msg = "identity_ref must not be empty"
            raise ValueError(msg)


def _os_user() -> str:
    """Best-effort OS username, without ever raising on odd environments."""
    try:
        return getpass.getuser()
    except (KeyError, OSError):  # pragma: no cover - depends on host env
        return os.environ.get("USER") or os.environ.get("USERNAME") or "unknown"


def local_principal() -> Principal:
    """Derive the `local:<os-user>` principal for this process.

    Local callers are recorded as humans: the agent kind is reserved for
    identities that authenticate with their own credential (M4), so that
    "an agent did this" never becomes a guess made from a process name.
    """
    user = _os_user()
    return Principal(
        identity_ref=f"{LOCAL_SCHEME}:{user}",
        kind="human",
        display_name=user,
    )
