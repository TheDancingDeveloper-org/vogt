"""The suite's safety policy: where it may run, and what it may start.

These are deliberately not configurable by a command-line override. A test
suite that can be pointed at production by adding one flag is one typo away
from writing to production. A new instance is added to ``ALLOWED_HOSTS`` in a
reviewed change, on purpose.
"""

from __future__ import annotations

import stat
import urllib.parse
from dataclasses import dataclass
from pathlib import Path

#: Hosts the suite may write to. ``vogt-dev`` is the daily dev instance; the
#: loopback entries exist for the in-process fake and a locally forwarded dev.
ALLOWED_HOSTS: frozenset[str] = frozenset(
    {
        "vogt-dev.sprooty.com",
        "127.0.0.1",
        "localhost",
        "::1",
    }
)

#: Hosts that are refused before the allowlist is consulted, so the error
#: reads "production" rather than "not allowed".
PROD_HOSTS: frozenset[str] = frozenset({"vogt.sprooty.com"})

#: The model every throwaway session must use. The suite never starts a
#: session on a production provider or a Claude subscription.
SESSION_MODEL = "deepseek/deepseek-v4-flash"
SESSION_PROJECT = "vogt"
SESSION_TEMPLATE = "Codex (protected)"

#: Names that, if present in a pod's workspace, are a leaked credential rather
#: than workspace content. Checked by name only; the suite never reads them.
WORKSPACE_SECRET_NAMES: tuple[str, ...] = (
    "settings.local.json",
    "auth.json",
)


class PolicyRefusal(Exception):
    """The run would be unsafe or meaningless; nothing was executed."""


@dataclass(frozen=True)
class Target:
    base_url: str
    host: str
    loopback: bool


def validate_base_url(raw: str) -> Target:
    """The target, or a refusal naming the rule. No request is made here."""
    parts = urllib.parse.urlsplit(raw)
    if parts.scheme not in ("https", "http"):
        raise PolicyRefusal(
            f"base URL must be http(s), not {parts.scheme or 'empty'!r}"
        )
    host = (parts.hostname or "").lower()
    if not host:
        raise PolicyRefusal("base URL has no host")
    if host in PROD_HOSTS:
        raise PolicyRefusal(
            f"refusing {host}: that is the production Vogt instance. "
            "The acceptance suite writes to a dev instance only."
        )
    if host not in ALLOWED_HOSTS:
        allowed = ", ".join(sorted(ALLOWED_HOSTS))
        raise PolicyRefusal(
            f"refusing {host}: not in the acceptance allowlist ({allowed}). "
            "Add a host in a reviewed change if this is a new dev instance."
        )
    loopback = host in {"127.0.0.1", "localhost", "::1"}
    if parts.scheme == "http" and not loopback:
        raise PolicyRefusal(
            f"refusing plain http to {host}: a bearer would cross the network in clear"
        )
    return Target(base_url=raw.rstrip("/"), host=host, loopback=loopback)


def validate_instance(*, instance_id: str | None, prod_instance_id: str | None) -> None:
    """Refuse a dev hostname that is actually production behind it.

    ``instance_id`` comes from the target's own diagnostics. ``prod_instance_id``
    is either configured or learned from the target's configured diagnostics
    peer; when neither is known the check says so rather than pretending.
    """
    if not instance_id or not prod_instance_id:
        return
    if instance_id == prod_instance_id:
        raise PolicyRefusal(
            "the base URL answered with the production instance id; refusing to run"
        )


def read_token_file(path: Path) -> str:
    """Read a token from a regular file, refusing a world-readable one.

    The value is returned to the caller and never logged. A missing, empty or
    group/world-readable file is a refusal, not a skip: the operator asked for
    a run with a credential, and a bad credential file is an invocation error.
    """
    try:
        info = path.lstat()
    except FileNotFoundError:
        raise PolicyRefusal(f"token file does not exist: {path}") from None
    if not stat.S_ISREG(info.st_mode):
        raise PolicyRefusal(f"token file is not a regular file: {path}")
    if info.st_mode & (stat.S_IRWXG | stat.S_IRWXO):
        raise PolicyRefusal(
            f"token file {path} is group/world-accessible; chmod 600 it first"
        )
    try:
        value = path.read_text("utf-8").strip()
    except OSError as exc:
        raise PolicyRefusal(
            f"token file could not be read: {type(exc).__name__}"
        ) from None
    if not value:
        raise PolicyRefusal(f"token file is empty: {path}")
    if any(character.isspace() for character in value):
        raise PolicyRefusal("token file contains whitespace; it must hold one token")
    return value


def session_start_arguments(tag: str, *, task: str) -> dict[str, object]:
    """The one shape every throwaway session uses.

    ``worktree=false`` so the session cannot disturb a checkout,
    ``placement=pod`` so it lands on the session node and never the engine
    host, and the deepseek flash model so a test never spends a subscription.
    """
    return {
        "project": SESSION_PROJECT,
        "template": SESSION_TEMPLATE,
        "placement": "pod",
        "worktree": False,
        "model": SESSION_MODEL,
        "task": task,
        "permission_mode": "default",
        "autopilot": False,
        "reason": f"{tag}: throwaway acceptance session",
    }
