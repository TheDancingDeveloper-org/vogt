"""Out-of-process deterministic hooks for golden recording.

`tests/conftest.py` already has a step clock and a sequential id factory,
but only inside the suite. A core running as its own process cannot be
handed those objects, so the same behaviour is selected by environment:

- `VOGT_TEST_CLOCK_START=<RFC3339>` makes `AppContext.clock` a step clock
  that starts at that instant and advances one second per read.
- `VOGT_TEST_IDS=sequential` makes `AppContext.id_factory` count per prefix.

Either is honoured only when it is set. A listener bound somewhere other
than loopback refuses to start with either set, because a deterministic
clock and predictable identifiers are a test aid, not a deployment mode.
"""

from __future__ import annotations

import ipaddress
import os
from datetime import datetime, timedelta

from vogt.core.clock import Clock, from_iso
from vogt.core.ids import IdFactory
from vogt.errors import InvalidRequest

CLOCK_ENV = "VOGT_TEST_CLOCK_START"
IDS_ENV = "VOGT_TEST_IDS"
SEQUENTIAL = "sequential"


class StepClock:
    """A clock that advances one second per read, so ordering is testable."""

    def __init__(self, start: datetime) -> None:
        self._now = start

    def __call__(self) -> datetime:
        current = self._now
        self._now += timedelta(seconds=1)
        return current


class SequentialIds:
    """Deterministic ids, so two fresh instances agree on the same script."""

    def __init__(self) -> None:
        self._counts: dict[str, int] = {}

    def __call__(self, prefix: str) -> str:
        self._counts[prefix] = self._counts.get(prefix, 0) + 1
        return f"{prefix}_{self._counts[prefix]:04d}"


def clock_from_env(environ: os._Environ[str] | None = None) -> Clock | None:
    """The step clock the environment asks for, or `None` when unset.

    An empty value is unset. A value that is not an RFC3339 timestamp is a
    misconfiguration and refuses to start rather than silently falling back
    to wall-clock time, which would record goldens nobody could reproduce.
    """
    env = os.environ if environ is None else environ
    raw = env.get(CLOCK_ENV)
    if raw is None or not raw.strip():
        return None
    try:
        start = from_iso(raw.strip())
    except ValueError as exc:
        msg = f"{CLOCK_ENV} must be an RFC3339 timestamp, not {raw!r}"
        raise InvalidRequest(msg) from exc
    return StepClock(start)


def ids_from_env(environ: os._Environ[str] | None = None) -> IdFactory | None:
    """The sequential id factory, or `None` when unset or set to anything else."""
    env = os.environ if environ is None else environ
    raw = env.get(IDS_ENV)
    if raw is None or not raw.strip():
        return None
    if raw.strip() != SEQUENTIAL:
        msg = f"{IDS_ENV} must be {SEQUENTIAL!r} when set, not {raw!r}"
        raise InvalidRequest(msg)
    return SequentialIds()


def hooks_active(environ: os._Environ[str] | None = None) -> tuple[str, ...]:
    """The names of the hooks that are set, for the one startup warning."""
    env = os.environ if environ is None else environ
    active: list[str] = []
    if (env.get(CLOCK_ENV) or "").strip():
        active.append(CLOCK_ENV)
    if (env.get(IDS_ENV) or "").strip():
        active.append(IDS_ENV)
    return tuple(active)


def is_loopback(host: str) -> bool:
    """Whether a listen address can only be reached from this machine.

    A name is loopback only when it is the literal `localhost`: resolving it
    would make the answer depend on DNS, and a test aid must not.
    """
    candidate = host.strip().strip("[]")
    if candidate.lower() == "localhost":
        return True
    try:
        address = ipaddress.ip_address(candidate)
    except ValueError:
        return False
    return address.is_loopback


def refuse_hooks_off_loopback(
    host: str, environ: os._Environ[str] | None = None
) -> None:
    """Refuse to listen off loopback while a deterministic hook is set."""
    active = hooks_active(environ)
    if active and not is_loopback(host):
        names = ", ".join(active)
        msg = (
            f"refusing to serve on {host}: {names} selects a deterministic "
            "test clock or id factory, which is only honoured on a loopback bind"
        )
        raise InvalidRequest(msg)
