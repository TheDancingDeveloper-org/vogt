"""WI-868: `instance.diagnostics` — confirm a deploy without tailnet or Komodo."""

from __future__ import annotations

import json
import logging
from dataclasses import replace
from datetime import timedelta

import pytest

from vogt import __version__
from vogt.adapters.engine import EngineClient
from vogt.adapters.peer import PeerClient
from vogt.application.context import AppContext
from vogt.application.models import DiagnosticsParams
from vogt.application.services import instance_diagnostics
from vogt.observability import (
    PROCESS_STARTED_AT,
    configure_logging,
    logger,
    recent_problems,
    redact,
)
from vogt.registry import default_registry


def _fixed_clock(ctx: AppContext) -> AppContext:
    now = PROCESS_STARTED_AT + timedelta(seconds=90)
    return replace(ctx, clock=lambda: now)


def test_a_healthy_instance_reports_its_identity_and_checks(
    instance: AppContext,
) -> None:
    ctx = _fixed_clock(instance)
    answer = instance_diagnostics(ctx, DiagnosticsParams())

    assert answer.vogt_version == __version__
    assert answer.image_digest is None, "unstated is reported unknown, not guessed"
    assert answer.uptime_seconds == 90
    assert answer.instance_id
    checks = {check.name: check for check in answer.checks}
    assert checks["declared_store"].status == "ok"
    assert checks["observed_store"].status == "ok"
    assert checks["engine"].status == "not_configured"
    # Nothing has been swept: collection is degraded, and says so.
    assert checks["collection"].status == "degraded"
    assert answer.status == "degraded"
    for store in ("declared", "observed"):
        migration = answer.migrations[store]
        assert migration.pending == 0
        assert migration.applied == migration.expected
    assert answer.peer.status == "not_requested"


def test_the_stated_image_digest_is_reported(instance: AppContext) -> None:
    config = instance.config.model_copy(update={"image_digest": "sha256:abc"})
    answer = instance_diagnostics(replace(instance, config=config), DiagnosticsParams())
    assert answer.image_digest == "sha256:abc"


def test_an_unreachable_engine_degrades_rather_than_raises(
    instance: AppContext,
) -> None:
    def down(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del url, headers, body, method
        return 503, b""

    ctx = replace(instance, engine=EngineClient(base_url="http://e", transport=down))
    checks = {c.name: c for c in instance_diagnostics(ctx, DiagnosticsParams()).checks}
    assert checks["engine"].status == "degraded"
    assert "503" in (checks["engine"].detail or "")

    def ok(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        assert url == "http://e/healthz"
        del headers, body, method
        return 200, b'{"ok": true}'

    ctx = replace(instance, engine=EngineClient(base_url="http://e", transport=ok))
    checks = {c.name: c for c in instance_diagnostics(ctx, DiagnosticsParams()).checks}
    assert checks["engine"].status == "ok"


def test_recent_problems_are_redacted_and_bounded(instance: AppContext) -> None:
    configure_logging(level="info")
    try:
        log = logger("diagnostics-test")
        log.warning(
            "push failed for https://bot:s3cret@github.com/x token=vogt_abcdefgh123"
        )
        log.error("Authorization: Bearer abc.def.ghi rejected")
        log.info("an info line is not a problem")

        answer = instance_diagnostics(instance, DiagnosticsParams(log_lines=2))
        assert answer.recent_log.capturing is True
        lines = answer.recent_log.lines
        assert len(lines) == 2
        joined = "\n".join(lines)
        assert "s3cret" not in joined
        assert "vogt_abcdefgh123" not in joined
        assert "abc.def.ghi" not in joined
        assert "push failed" in joined
        assert "an info line" not in joined
        assert (
            instance_diagnostics(
                instance, DiagnosticsParams(log_lines=0)
            ).recent_log.lines
            == []
        )
    finally:
        root = logging.getLogger()
        for handler in list(root.handlers):
            if handler.get_name() in {"vogt", "vogt-recent"}:
                root.removeHandler(handler)


def test_redaction_shapes() -> None:
    assert redact("ghp_" + "a" * 30) == "[redacted]"
    assert redact('{"password": "hunter2"}') == '{"password": "[redacted]"}'
    assert redact("https://u:p@host/x") == "https://[redacted]@host/x"
    assert redact("nothing secret here") == "nothing secret here"
    assert recent_problems(0) == []


def test_peer_not_configured_is_said(instance: AppContext) -> None:
    answer = instance_diagnostics(instance, DiagnosticsParams(peer=True))
    assert answer.peer.status == "not_configured"


def test_peer_answer_is_fetched_with_the_token_and_without_recursion(
    instance: AppContext,
) -> None:
    seen: list[tuple[str, dict[str, str]]] = []

    def peer(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del body, method
        seen.append((url, headers))
        return 200, json.dumps({"vogt_version": "9.9.9", "status": "ok"}).encode()

    ctx = replace(
        instance,
        peer=PeerClient(base_url="https://prod/api/vogt", token="t0k", transport=peer),
    )
    answer = instance_diagnostics(ctx, DiagnosticsParams(peer=True, log_lines=5))
    assert answer.peer.status == "ok"
    assert answer.peer.diagnostics == {"vogt_version": "9.9.9", "status": "ok"}
    url, headers = seen[0]
    assert url == "https://prod/api/vogt/instance/diagnostics?peer=false&log_lines=5"
    assert headers["Authorization"] == "Bearer t0k"


@pytest.mark.parametrize(
    ("status", "answer_body", "expected"),
    [
        (401, b"", "refused"),
        (502, b"", "unreachable"),
        (200, b"<html>", "invalid_response"),
        (200, b"[1]", "invalid_response"),
    ],
)
def test_peer_failures_are_reported_not_raised(
    instance: AppContext, status: int, answer_body: bytes, expected: str
) -> None:
    def peer(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del url, headers, body, method
        return status, answer_body

    ctx = replace(instance, peer=PeerClient(base_url="https://p", transport=peer))
    answer = instance_diagnostics(ctx, DiagnosticsParams(peer=True))
    assert answer.peer.status == expected
    assert answer.peer.diagnostics is None


def test_peer_client_reads_its_token_from_a_file(tmp_path: object) -> None:
    from pathlib import Path

    token = Path(str(tmp_path)) / "peer.token"
    token.write_text("abc\n", encoding="utf-8")
    client = PeerClient.from_config("https://p/api/vogt/", token)
    assert client is not None
    assert client.base_url == "https://p/api/vogt"
    assert client.token == "abc"
    assert PeerClient.from_config(None, token) is None


def test_diagnostics_is_a_read_on_every_surface() -> None:
    registry = default_registry()
    operation = registry.get("instance.diagnostics")
    assert operation.scope == "read"
    assert operation.mutating is False
    assert operation.mcp_tool_name == "instance_diagnostics"
    assert registry.transports_for("instance.diagnostics") == {"cli", "http", "mcp"}
