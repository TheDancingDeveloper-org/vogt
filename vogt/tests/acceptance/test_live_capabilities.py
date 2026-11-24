"""Outcome tests for acceptance checks that gate on live capabilities."""

from __future__ import annotations

from typing import Any

import pytest

from tests.acceptance.checks import sessions, tracker
from tests.acceptance.harness import Context, Options


class FakeSessionTarget:
    """Immediate session answers, so capability tests assert outcomes, not time."""

    secrets: tuple[str, ...] = ()

    def __init__(self, replies: list[str]) -> None:
        self.replies = list(replies)
        self.last = ""

    def tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if name == "session_input":
            self.last = self.replies.pop(0)
            return {"delivery": "sent"}
        if name == "session_last_reply":
            return {"messages": [{"text": self.last}]}
        raise AssertionError(f"unexpected tool call: {name} {arguments}")


class EmptyReplyTarget:
    """A session whose model turn failed before writing an assistant reply."""

    secrets: tuple[str, ...] = ()

    def tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if name == "session_screen":
            return {"lines": ["Codex composer after the failed turn"]}
        if name == "session_log_tail":
            return {
                "text": "Reconnecting... waiting for network\n"
                "Connection failed: error sending request"
            }
        if name == "session_list":
            return {"sessions": []}
        raise AssertionError(f"unexpected tool call: {name} {arguments}")


def _context(*replies: str) -> Context:
    options = Options(with_sessions=True, state={"sessions.codex": "ses_fake"})
    return Context(FakeSessionTarget(list(replies)), options, "acceptance-capability")


@pytest.mark.parametrize(
    ("reply", "verdict"),
    [
        ("WS_CAPABILITY_ABSENT\nWS_SCAN_DONE", "SKIP"),
        (
            "WS_CAPABILITY_PRESENT\nWS_AGENTS_MISSING\nWS_RULES_OK\nWS_SCAN_DONE",
            "FAIL",
        ),
        (
            "WS_CAPABILITY_PRESENT\nWS_AGENTS_OK\nWS_RULES_OK\n"
            "settings.local.json 0\nauth.json 0\n.git-credentials 0\n"
            ".netrc 0\nssh_keys 0\nWS_SCAN_DONE",
            "PASS",
        ),
    ],
)
def test_workspace_capability_uses_the_live_pod_fact(reply: str, verdict: str) -> None:
    assert sessions.workspace_content(_context(reply)).verdict == verdict


@pytest.mark.parametrize(
    ("replies", "verdict"),
    [
        (("GRAFT_CREDENTIAL_ABSENT",), "SKIP"),
        (("GRAFT_CREDENTIAL_BROKEN",), "FAIL"),
        (("GRAFT_CREDENTIAL_PRESENT", "GRAFT_CLONE_FAIL"), "FAIL"),
        (("GRAFT_CREDENTIAL_PRESENT", "GRAFT_CLONE_OK"), "PASS"),
    ],
)
def test_graft_capability_uses_the_live_pod_fact(
    replies: tuple[str, ...], verdict: str
) -> None:
    assert sessions.graft_token(_context(*replies)).verdict == verdict


@pytest.mark.parametrize(
    ("replies", "verdict"),
    [
        (("CADASTRE_CAPABILITY_ABSENT",), "SKIP"),
        (("CADASTRE_CAPABILITY_PRESENT", "CADASTRE_CALL_FAIL denied"), "FAIL"),
        (("CADASTRE_CAPABILITY_PRESENT", "CADASTRE_CALL_OK"), "PASS"),
    ],
)
def test_cadastre_capability_requires_a_live_tool_call(
    replies: tuple[str, ...], verdict: str
) -> None:
    assert sessions.cadastre_mcp(_context(*replies)).verdict == verdict


def test_import_tool_is_detected_and_its_production_guard_runs() -> None:
    check = tracker.import_refuses_prod(_context())
    assert check.verdict == "PASS"
    assert "production target refused" in check.evidence


def test_empty_reply_names_the_provider_failure_not_transcript_writeback() -> None:
    ctx = Context(
        EmptyReplyTarget(),
        Options(with_sessions=True, state={"sessions.codex": "ses_fake"}),
        "acceptance-provider-failure",
    )

    detail = sessions._missing_reply_detail(ctx, "ses_fake")

    assert "provider/network failure" in detail
    assert "connection failed:" in detail


def test_dependent_session_checks_skip_after_the_first_turn_failed() -> None:
    reason = "the first turn produced no assistant reply because the provider failed"
    ctx = Context(
        EmptyReplyTarget(),
        Options(
            with_sessions=True,
            state={
                "sessions.codex": "ses_fake",
                "sessions.codex.first_turn_failure": reason,
            },
        ),
        "acceptance-provider-failure",
    )

    queued = sessions.input_queueing(ctx)
    resumed = sessions.hibernate_and_wake(ctx)

    assert queued.verdict == "SKIP"
    assert resumed.verdict == "SKIP"
    assert "S3 established" in queued.evidence
    assert "S3 established" in resumed.evidence
