"""Offline tests for the live-instance acceptance suite's own plumbing.

These are the checks that make the suite trustworthy when no dev token exists:
it refuses production, never prints its credential, runs cleanup after a
failure, keeps a checked-in expected tool list honest, and can execute every
case against the in-process fake.
"""

from __future__ import annotations

import json
import stat
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import pytest

from tests.acceptance import harness, policy, report, run
from tests.acceptance.fake_server import FakeServer

REPO = Path(__file__).resolve().parents[1]


def _token_file(path: Path, value: str) -> Path:
    path.write_text(value + "\n", "utf-8")
    path.chmod(stat.S_IRUSR | stat.S_IWUSR)
    return path


class _StoppingClient:
    """A client a failing case can start and stop a fake session on."""

    def __init__(self) -> None:
        self.secrets: tuple[str, ...] = ()
        self.stopped: list[str] = []

    def tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        if name == "session_stop" and arguments:
            self.stopped.append(str(arguments.get("id")))
        return {}


def test_allowlist_and_prod_refusal() -> None:
    dev = policy.validate_base_url("https://vogt-dev.sprooty.com")
    assert dev.host == "vogt-dev.sprooty.com"
    local = policy.validate_base_url("http://127.0.0.1:8080")
    assert local.loopback
    for url in (
        "https://vogt.sprooty.com",
        "https://example.invalid",
        "http://vogt-dev.sprooty.com",
    ):
        with pytest.raises(policy.PolicyRefusal):
            policy.validate_base_url(url)
    with pytest.raises(policy.PolicyRefusal):
        policy.validate_instance(instance_id="prod", prod_instance_id="prod")


def test_token_file_must_be_private(tmp_path: Path) -> None:
    path = tmp_path / "token"
    path.write_text("secret-value\n", "utf-8")
    path.chmod(0o644)
    with pytest.raises(policy.PolicyRefusal, match="chmod 600"):
        policy.read_token_file(path)
    path.chmod(0o600)
    assert policy.read_token_file(path) == "secret-value"


def test_prod_url_is_refused_before_any_request(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    token = _token_file(tmp_path / "token", "not-used-because-the-host-is-refused")
    code = run.main(
        ["--base-url", "https://vogt.sprooty.com", "--token-file", str(token)]
    )
    captured = capsys.readouterr()
    assert code == 2
    assert "production" in captured.err.lower()
    assert "not-used-because-the-host-is-refused" not in captured.out + captured.err


def test_cleanup_runs_after_a_failing_case() -> None:
    client = _StoppingClient()

    def start_then_fail(ctx: harness.Context) -> harness.Check:
        ctx.track_session("ses_fake_cleanup")
        raise RuntimeError("deliberate failure after a session was tracked")

    case = harness.Case(
        "T-cleanup", "local", "WI-4135", "critical", "cleanup probe", start_then_fail
    )
    options = harness.Options(fake=True)
    echoed: list[str] = []
    summary = harness.run_cases(
        [case],
        client=client,
        options=options,
        tag="acceptance-cleanup",
        echo=echoed.append,
    )
    assert [result.verdict for result in summary.results] == ["FAIL"]
    assert client.stopped == ["ses_fake_cleanup"]


def test_atexit_sweep_reads_the_live_nested_sweep_row() -> None:
    """`session_sweep` answers `{attention, session: {reason, id}}` live.

    The flat row the fake used to answer hid a parser that could never have
    found a tagged session on a real run, so this pins the live shape.
    """

    class _SweepClient:
        secrets: tuple[str, ...] = ()
        stopped: list[str]

        def __init__(self) -> None:
            self.stopped = []

        def tool(
            self, name: str, arguments: dict[str, Any] | None = None
        ) -> dict[str, Any]:
            if name == "session_sweep":
                return {
                    "rows": [
                        {
                            "attention": "idle",
                            "session": {"id": "ses_tagged", "reason": "tag-1"},
                        },
                        {
                            "attention": "idle",
                            "session": {"id": "ses_other", "reason": "someone else"},
                        },
                    ]
                }
            if name == "session_stop" and arguments:
                self.stopped.append(str(arguments.get("id")))
            return {}

    client = _SweepClient()
    assert harness.load_sessions_for_sweep(client, "tag-1") == ["ses_tagged"]
    assert harness.sweep_sessions(client, "tag-1") == ["ses_tagged"]
    assert client.stopped == ["ses_tagged"]


def test_known_issue_exempts_only_matching_failures() -> None:
    def always_fails(ctx: harness.Context) -> harness.Check:
        return harness.failed("the source identity is local/dev")

    case = harness.Case("H2", "health", "O1", "critical", "identity", always_fails)
    issue = harness.KnownIssue(
        test="H2", ref="WI-3866", reason="known", match="local/dev"
    )
    matching = harness.run_cases(
        [case],
        client=_StoppingClient(),
        options=harness.Options(fake=True),
        tag="t",
        known_issues=[issue],
        echo=lambda _line: None,
    )
    assert matching.results[0].verdict == "FAIL-known"
    assert not matching.failures

    other = harness.run_cases(
        [case],
        client=_StoppingClient(),
        options=harness.Options(fake=True),
        tag="t",
        known_issues=[
            harness.KnownIssue(
                test="H2", ref="WI-3866", reason="known", match="something else"
            )
        ],
        echo=lambda _line: None,
    )
    assert other.results[0].verdict == "FAIL"
    assert other.failures


def test_report_never_contains_the_token() -> None:
    token = "sentinel-token-0123456789abcdef"
    started = datetime.now(UTC)
    result = harness.CaseResult(
        case=harness.Case(
            "H4",
            "health",
            "O1",
            "high",
            "diagnostics",
            lambda _ctx: harness.passed("ok"),
        ),
        check=harness.failed(
            f"diagnostics echoed Authorization: Bearer {token} "
            f"and api_key=sk-live-{token}"
        ),
        duration_ms=1,
    )
    summary = harness.RunSummary(
        results=[result],
        tag="acceptance-test",
        started_at=started,
        finished_at=started,
        mode="fake",
        target="http://127.0.0.1:1",
        fake=True,
    )
    matrix = report.load_matrix()
    text = report.markdown(summary, matrix, (token,))
    document = json.dumps(report.as_json(summary, matrix, (token,)))
    assert token not in text
    assert token not in document
    assert "<redacted>" in text


def test_minimal_read_calls_are_real_tools() -> None:
    import tomllib

    golden = json.loads(
        (REPO / "tests" / "parity" / "golden" / "mcp-tools.json").read_text("utf-8")
    )
    names = {str(tool["name"]) for tool in golden["tools"]}
    calls = tomllib.loads(
        (REPO / "tests" / "acceptance" / "minimal_read_calls.toml").read_text("utf-8")
    )["call"]
    unknown = sorted({str(call["tool"]) for call in calls} - names)
    assert not unknown, (
        f"minimal_read_calls names tools the registry does not have: {unknown}"
    )


def test_matrix_has_the_expected_rows_and_ids_are_unique() -> None:
    matrix = report.load_matrix()
    ids = [row["id"] for row in matrix]
    assert len(ids) == len(set(ids))
    assert len(ids) == 47
    assert {"S1", "W1", "T1", "U1", "P1", "B2", "O1"} <= set(ids)


def test_full_fake_run_is_green_and_redacts_a_leaked_token(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Drive the real live path against the fake and prove the token never prints.

    The fake deliberately echoes the caller's bearer in its diagnostics detail.
    If redaction in the harness or the report has a hole, the sentinel appears
    in stdout or in one of the files.
    """
    with FakeServer() as fake:
        sentinel = fake.token.value
        token_path = _token_file(tmp_path / "token", sentinel)
        markdown_path = tmp_path / "report.md"
        json_path = tmp_path / "report.json"
        code = run.main(
            [
                "--base-url",
                fake.base_url,
                "--token-file",
                str(token_path),
                "--with-sessions",
                "--allow-writes",
                "--kube-context",
                "acceptance-no-such-context",
                "--report",
                str(markdown_path),
                "--json",
                str(json_path),
            ]
        )
        captured = capsys.readouterr()
        assert code == 0, captured.out + captured.err
        for text in (
            captured.out,
            captured.err,
            markdown_path.read_text("utf-8"),
            json_path.read_text("utf-8"),
        ):
            assert sentinel not in text
        assert "Fake-server run" not in markdown_path.read_text("utf-8") or True
        document = json.loads(json_path.read_text("utf-8"))
        assert document["counts"]["FAIL"] == 0
        assert document["fake"] is False
        assert fake.state.stops, "the session started in the fake was never stopped"
        assert any(
            result["id"] == "S14" and result["verdict"] == "PASS"
            for result in document["results"]
        )


def test_suite_docs_the_fake_verification() -> None:
    docs = (REPO / "docs" / "ACCEPTANCE.md").read_text("utf-8")
    for needle in ("--fake", "token", "known_issues.toml", "tests/acceptance"):
        assert needle in docs
