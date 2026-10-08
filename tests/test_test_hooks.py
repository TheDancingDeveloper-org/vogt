"""The out-of-process deterministic hooks and the registry manifest.

The hooks exist so a core running as its own process can be golden-recorded:
two fresh instances given the same environment and the same script must
produce the same identifiers and the same timestamps. They are inert unless
the environment asks for them, and a listener bound off loopback refuses to
start while either is set.
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest

from vogt.application.context import build_context
from vogt.application.models import (
    CreateActorParams,
    InitParams,
    RegistryDumpParams,
    ServeParams,
)
from vogt.application.services import create_actor, init_instance, registry_dump, serve
from vogt.application.test_hooks import (
    CLOCK_ENV,
    IDS_ENV,
    clock_from_env,
    ids_from_env,
    is_loopback,
    refuse_hooks_off_loopback,
)
from vogt.config import VogtConfig
from vogt.core.clock import utc_now
from vogt.core.ids import new_id
from vogt.errors import InvalidRequest

WHY = "hook test"
START = "2026-01-02T03:04:05+00:00"


@pytest.fixture(autouse=True)
def _clear_hooks(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(CLOCK_ENV, raising=False)
    monkeypatch.delenv(IDS_ENV, raising=False)
    import vogt.application.context as context_module

    context_module._hooks_announced = False


def test_hooks_are_off_unless_the_environment_asks(config: VogtConfig) -> None:
    ctx = build_context(config=config)
    assert ctx.clock is utc_now
    assert ctx.id_factory is new_id


def test_two_fresh_instances_agree_on_the_same_script(
    tmp_path_factory: pytest.TempPathFactory, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv(CLOCK_ENV, START)
    monkeypatch.setenv(IDS_ENV, "sequential")

    def run(name: str) -> tuple[str, str, str]:
        ctx = build_context(config=_config(tmp_path_factory.mktemp(name)))
        init_instance(ctx, InitParams())
        before = ctx.clock()
        actor = create_actor(
            ctx,
            CreateActorParams(
                identity_ref="agent:hooks",
                kind="agent",
                display_name="Hooks",
                reason=WHY,
            ),
        )
        return actor.actor.id, before.isoformat(), ctx.clock().isoformat()

    first_run = run("one")
    assert first_run == run("two")
    actor_id, before, after = first_run
    # Init itself reads the clock, so the first stamp a caller sees is past the
    # start instant. What must hold is that every fresh instance walks the same
    # instants, in order, from that start.
    assert actor_id.startswith("act_")
    assert START < before < after


def test_a_caller_supplied_clock_is_not_overridden(
    config: VogtConfig, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The suite passes its own clock; the environment must not redirect it."""
    monkeypatch.setenv(CLOCK_ENV, START)
    monkeypatch.setenv(IDS_ENV, "sequential")
    fixed = datetime(2024, 1, 1, tzinfo=UTC)
    ctx = build_context(
        config=config, clock=lambda: fixed, id_factory=lambda prefix: f"{prefix}_x"
    )
    assert ctx.clock() == fixed
    assert ctx.id_factory("act") == "act_x"


def test_a_bad_hook_value_refuses_to_start(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(CLOCK_ENV, "yesterday")
    with pytest.raises(InvalidRequest):
        clock_from_env()
    monkeypatch.delenv(CLOCK_ENV)
    monkeypatch.setenv(IDS_ENV, "random")
    with pytest.raises(InvalidRequest):
        ids_from_env()


@pytest.mark.parametrize(
    ("host", "loopback"),
    [
        ("127.0.0.1", True),
        ("::1", True),
        ("localhost", True),
        ("0.0.0.0", False),
        ("10.0.0.8", False),
    ],
)
def test_loopback_is_the_address_not_a_resolution(host: str, loopback: bool) -> None:
    assert is_loopback(host) is loopback


def test_serve_refuses_the_hooks_off_loopback(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv(CLOCK_ENV, START)
    with pytest.raises(InvalidRequest, match="loopback"):
        refuse_hooks_off_loopback("0.0.0.0")
    refuse_hooks_off_loopback("127.0.0.1")

    with pytest.raises(InvalidRequest, match="loopback"):
        serve(build_context(), ServeParams(host="0.0.0.0", port=1))


def test_startup_warns_once_when_a_hook_is_active(
    config: VogtConfig,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    import logging

    monkeypatch.setenv(IDS_ENV, "sequential")
    with caplog.at_level(logging.WARNING, logger="vogt.context"):
        build_context(config=config)
        build_context(config=config)
    warnings = [
        record
        for record in caplog.records
        if "deterministic test hooks" in record.message
    ]
    assert len(warnings) == 1
    assert IDS_ENV in warnings[0].message


def test_the_manifest_describes_every_operation_and_its_exclusions() -> None:
    from vogt.registry import HTTP_ONLY, LOCAL_ONLY, default_registry

    manifest = registry_dump(build_context(), RegistryDumpParams())
    by_name = {operation.name: operation for operation in manifest.operations}
    registry = default_registry()

    assert set(by_name) == set(registry.names)
    dumped = by_name["registry.dump"]
    assert dumped.transports == ["cli", "http", "mcp"]
    assert dumped.exclusion is None
    assert dumped.http_method == "GET"
    assert dumped.http_path == "/registry"
    assert dumped.mcp_tool == "registry_dump"
    assert dumped.cli_path == ["registry", "dump"]
    assert dumped.mutating is False
    assert "properties" in dumped.params_schema

    for name, reason in LOCAL_ONLY.items():
        assert by_name[name].exclusion == "local_only"
        assert by_name[name].exclusion_reason == reason
        assert by_name[name].transports == ["cli"]
    for name, reason in HTTP_ONLY.items():
        assert by_name[name].exclusion == "http_only"
        assert by_name[name].exclusion_reason == reason

    writes = [operation for operation in manifest.operations if operation.mutating]
    assert writes and all(operation.reason_required for operation in writes)


def _config(data_dir: Path) -> VogtConfig:
    return VogtConfig(
        data_dir=data_dir,
        sqlite_synchronous="off",
        session_transcript_roots={},
    )
