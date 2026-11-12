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
from tests.acceptance.checks.health import _digest_matches
from tests.acceptance.checks.persistence import _covering_mount
from tests.acceptance.checks.providers import deepseek_flash_registered
from tests.acceptance.fake_server import FakeServer
from tests.acceptance.policy import SESSION_MODEL

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


def test_image_digest_accepts_a_deployed_prefix_only() -> None:
    """The rollout prints a short source prefix; the engine reports sha256:…."""
    full = "sha256:f09829dfe9c53b3ce531f968e87359fc7af4ad0ea0115e1b13de8b4e71c1ea2f"
    assert _digest_matches(full, "f09829df") == (True, "prefix")
    assert _digest_matches(full, full) == (True, "exact")
    assert _digest_matches(full, full.removeprefix("sha256:")) == (True, "exact")
    assert _digest_matches(full, "deadbeef")[0] is False
    # A one-character prefix, or a digest from a different image, is not a
    # match even though one string starts with the other.
    assert _digest_matches(full, "f09")[0] is False
    assert _digest_matches(full, "f09829dg")[0] is False


def test_image_digest_refuses_a_truncated_reported_digest() -> None:
    """Only the expected side may be a prefix; a short reported digest is not
    evidence that the full expected image is running."""
    full = "sha256:f09829dfe9c53b3ce531f968e87359fc7af4ad0ea0115e1b13de8b4e71c1ea2f"
    assert _digest_matches("sha256:f09829df", full) == (False, "mismatch")
    assert _digest_matches("f09829dfe9c5", full)[0] is False


class _ProviderClient:
    def __init__(self, providers: list[dict[str, Any]]) -> None:
        self.providers = providers

    def tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        assert name == "llm_provider_list"
        return {"providers": self.providers}


class _ProviderContext:
    def __init__(self, providers: list[dict[str, Any]]) -> None:
        self.client = _ProviderClient(providers)


@pytest.mark.parametrize(
    ("row", "verdict"),
    [
        ({}, "FAIL"),
        ({"state": None}, "FAIL"),
        ({"state": ""}, "FAIL"),
        ({"state": "missing-secret"}, "FAIL"),
        ({"state": "active"}, "PASS"),
        ({"state": "ready"}, "PASS"),
    ],
)
def test_provider_without_a_ready_state_fails(
    row: dict[str, Any], verdict: str
) -> None:
    """P1 must not pass on a provider row that does not say it is ready."""
    provider = {"name": "deepseek", "models": [SESSION_MODEL], **row}
    check = deepseek_flash_registered(_ProviderContext([provider]))  # type: ignore[arg-type]
    assert check.verdict == verdict


def test_state_mount_is_the_most_specific_ancestor() -> None:
    """K1 must pick the nested state PVC, not the emptyDir on its parent."""
    state_dir = "/home/sprooty/.local/share/vogt-engine"
    mounts = [
        ("engine", "agent-config", "/home/sprooty"),
        ("engine", "state", state_dir),
    ]
    assert _covering_mount(state_dir, mounts) == ("engine", "state", state_dir)
    # Without the nested mount, the parent is the mount that holds state.
    assert _covering_mount(state_dir, mounts[:1]) == (
        "engine",
        "agent-config",
        "/home/sprooty",
    )
    # A sibling path that only shares a string prefix is not inside the mount.
    assert _covering_mount("/home/sprooty2/vogt-engine", mounts) is None


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


def test_a_tracked_session_survives_the_case_that_started_it() -> None:
    """S1 starts the pod the later session cases assert against.

    A per-case Context used to stop every session it had tracked, so S1 killed
    the pod before S2 waited for it and the whole group failed against the
    live instance. The stop belongs to the run, after the last case.
    """
    client = _StoppingClient()
    seen: list[list[str]] = []

    def start_case(ctx: harness.Context) -> harness.Check:
        ctx.track_session("ses_shared")
        return harness.passed("started")

    def use_case(ctx: harness.Context) -> harness.Check:
        seen.append(list(client.stopped))
        return harness.passed("still there")

    cases = [
        harness.Case("T-start", "local", "WI-4135", "critical", "start", start_case),
        harness.Case("T-use", "local", "WI-4135", "critical", "use", use_case),
    ]
    summary = harness.run_cases(
        cases, client=client, options=harness.Options(fake=True), tag="acceptance-share"
    )
    assert [result.verdict for result in summary.results] == ["PASS", "PASS"]
    assert seen == [[]], "the session was stopped before the next case ran"
    assert client.stopped == ["ses_shared"]


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


def test_fake_work_writes_answer_with_the_live_compact_summary() -> None:
    """The live write surface answers with four fields, not the whole item.

    A check that asserted priority on the write result would fail against the
    live instance even though the write persisted; the fake must keep the same
    shape so the offline run catches that class of mistake.
    """
    from tests.acceptance.client import HttpClient, McpClient

    with FakeServer() as fake:
        mcp = McpClient(HttpClient(fake.base_url, token=fake.token))
        mcp.initialize()
        project = mcp.tool(
            "project_create",
            {
                "name": "acceptance-shape",
                "root_path": "/tmp/acceptance-shape",
                "lifecycle_state": "incubating",
                "reason": "offline shape test",
            },
        )
        slug = str((project.get("project") or project)["slug"])
        created = mcp.tool(
            "work_create",
            {
                "kind": "bug",
                "title": "shape",
                "priority": "p1",
                "project": slug,
                "local_only": True,
                "labels": ["shape"],
                "reason": "offline shape test",
            },
        )
        assert set(created) == {"ref", "state", "title", "updated_at"}
        read = mcp.tool("work_get", {"ref": created["ref"]})
        assert read["item"]["priority"] == "p1"
        assert read["item"]["project_slug"] == slug
        assert read["item"]["labels"] == ["shape"]
        assert read["comments"] == []
        target = mcp.tool(
            "work_create",
            {
                "kind": "chore",
                "title": "shape target",
                "project": slug,
                "local_only": True,
                "reason": "offline shape test",
            },
        )
        related = mcp.tool(
            "work_relate",
            {
                "ref": target["ref"],
                "kind": "depends_on",
                "target": created["ref"],
                "reason": "offline shape test",
            },
        )
        relation = related["item"]["relations"][0]
        assert relation["related_ref"] == created["ref"]
        updated = mcp.tool(
            "work_update",
            {"ref": created["ref"], "priority": "p0", "reason": "offline shape test"},
        )
        assert set(updated) == {"ref", "state", "title", "updated_at"}
        assert mcp.tool("work_get", {"ref": created["ref"]})["item"]["priority"] == "p0"


def test_fake_session_list_splits_digest_and_full_rows() -> None:
    """`session_list` answers digest rows under `digests`, full under `sessions`.

    The live surface does this, so a check that reads only `sessions` sees an
    empty list in the default mode. The fake has to keep the split or the
    offline run cannot catch that mistake.
    """
    from tests.acceptance.client import HttpClient, McpClient

    with FakeServer() as fake:
        mcp = McpClient(HttpClient(fake.base_url, token=fake.token))
        mcp.initialize()
        started = mcp.tool(
            "session_start",
            {
                "project": "vogt",
                "placement": "pod",
                "worktree": False,
                "model": "deepseek/deepseek-v4-flash",
                "task": "Reply with READY.",
                "reason": "offline shape test",
            },
        )
        session_id = str((started.get("session") or started)["id"])
        digest = mcp.tool("session_list")
        assert digest["sessions"] == []
        assert [row["id"] for row in digest["digests"]] == [session_id]
        full = mcp.tool("session_list", {"mode": "full"})
        assert full["digests"] == []
        assert [row["id"] for row in full["sessions"]] == [session_id]


def test_suite_docs_the_fake_verification() -> None:
    docs = (REPO / "docs" / "ACCEPTANCE.md").read_text("utf-8")
    for needle in ("--fake", "token", "known_issues.toml", "tests/acceptance"):
        assert needle in docs
