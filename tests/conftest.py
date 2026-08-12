"""Shared fixtures.

Every test runs against its own data directory and its own principal, so no
test depends on the OS user running it or on anything left behind by another.
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
    return VogtConfig(data_dir=data_dir)


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
