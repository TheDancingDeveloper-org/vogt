"""Shared fixtures.

Every test runs against its own data directory and its own principal, so no
test depends on the OS user running it or on anything left behind by another.

That isolation is what makes the suite disk-bound: each test creates a
directory and two SQLite databases with their WAL and shared-memory files,
then throws them away. Measured on this host, the suite spent 275 seconds of
a 291-second CI job waiting on a disk, against under a second of CPU. The
`config` fixture below deals with that, and does not change what is under
test.
"""

from __future__ import annotations

from collections.abc import Iterator
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.config import VogtConfig
from vogt.core.principal import Principal

TEST_PRINCIPAL = Principal(
    identity_ref="local:test-user", kind="human", display_name="test-user"
)


class StepClock:
    """A clock that advances one second per read, so ordering is testable."""

    def __init__(self, start: datetime | None = None) -> None:
        self._now = start or datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)

    def __call__(self) -> datetime:
        current = self._now
        self._now += timedelta(seconds=1)
        return current


class SequentialIds:
    """Deterministic ids, so failures name the entity that broke."""

    def __init__(self) -> None:
        self._counts: dict[str, int] = {}

    def __call__(self, prefix: str) -> str:
        self._counts[prefix] = self._counts.get(prefix, 0) + 1
        return f"{prefix}_{self._counts[prefix]:04d}"


@pytest.fixture
def data_dir(tmp_path: Path) -> Path:
    return tmp_path / "instance"


@pytest.fixture
def config(data_dir: Path) -> VogtConfig:
    """Test configuration — durability off, because nothing here outlives the run.

    Without this the suite is entirely fsync-bound. Vogt opens a connection
    per transaction and closes it, and closing the last connection to a WAL
    database forces a checkpoint, which fsyncs. On a contended ext4 disk that
    measured 176ms *per transaction* — and `synchronous=normal` does not help,
    because it skips the per-commit fsync but still syncs at checkpoints.

    The effect on the suite was 50.65s for twenty tests, of which 0.8s was CPU
    and the rest was waiting. With `off`, the same twenty run in well under a
    second. Nothing is lost: every test writes to a temporary directory that
    is deleted afterwards, so durability across a power cut is not a property
    any of them has ever needed.

    This is a test-environment setting, not a test-only code path — the same
    knob is available in production (`VOGT_SQLITE_SYNCHRONOUS`), and the
    storage layer behaves identically either way.
    """
    return VogtConfig(data_dir=data_dir, sqlite_synchronous="off")


@pytest.fixture
def context(config: VogtConfig) -> AppContext:
    return build_context(
        config=config,
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )


@pytest.fixture
def instance(context: AppContext) -> AppContext:
    """An initialised instance, ready for writes."""
    init_instance(context, InitParams())
    return context


@pytest.fixture
def clean_env(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Remove any VOGT_* configuration leaking in from the developer's shell."""
    import os

    for key in list(os.environ):
        if key.startswith("VOGT_"):
            monkeypatch.delenv(key, raising=False)
    yield
