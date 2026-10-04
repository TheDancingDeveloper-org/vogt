"""The agent activity index: redaction, the transcript reader, the store, the ops.

Every fake credential below is assembled at runtime from pieces, so this file
never contains a string a secret scanner would (rightly) refuse to let through
— and so the tests prove the redactor recognises the *shape*, not one value.
"""

from __future__ import annotations

import dataclasses
import json
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    AgentActivitySearchParams,
    AgentActivitySummaryParams,
    CoverageParams,
    RegisterProjectParams,
    StartSessionParams,
    SweepParams,
)
from vogt.application.services import (
    coverage,
    register_project,
    search_agent_activity,
    start_session,
    summarize_agent_activity,
    sweep,
)
from vogt.application.services.agent_activity import NOT_CONFIGURED, NOT_YET_INDEXED
from vogt.application.services.collect import collector_registry
from vogt.collectors.agent_activity import AgentActivityCollector
from vogt.config import VogtConfig
from vogt.core.agent_activity import (
    WITHHELD,
    ServiceMatcher,
    dumps_secrets,
    excerpt,
    is_error,
    looks_like_dump,
    redact,
    result_excerpt,
    summarize_input,
)
from vogt.errors import NotFound
from vogt.storage.observed_types import ActivityQuery
from vogt.storage.sqlite.observed import SqliteObservedStore

from tests.conftest import native_work_item
from tests.test_session_outcomes import StandInEngine

WHY = "agent activity test"
T0 = datetime(2026, 10, 4, 6, 0, tzinfo=UTC)

# -- realistic fake secrets, built from parts ------------------------------

GH_CLASSIC = "gh" + "p_" + "Zq3" * 12
GH_FINE = "github" + "_pat_" + "11ABCDEFG0" + "x9Y" * 20
JWT = ".".join(
    [
        "ey" + "JhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
        "ey" + "JzdWIiOiJhZ2VudCIsImV4cCI6MTk5OTk5OTk5OX0",
        "Qm9ndXNTaWduYXR1cmVGb3JUZXN0c09ubHkxMjM0NTY",
    ]
)
PEM = (
    "-----BEGIN " + "OPENSSH PRIVATE KEY-----\n"
    "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAAAMwAAAAtzc2gtZW\n"
    "QyNTUxOQAAACBGZmFrZUtleUZvclRlc3RzT25seU5vdFJlYWwAAAAAAAAAAAAAAAAAAA==\n"
    "-----END " + "OPENSSH PRIVATE KEY-----"
)
AWS_ID = "AK" + "IA" + "QWERTYUIOP234567"
ANTHROPIC = "sk-" + "ant-api03-" + "Fake0Key1For2Tests3Only4" * 2
SLACK = "xo" + "xb-" + "123456789012-1234567890123-" + "AbCdEfGhIjKlMnOpQrStUvWx"
INFISICAL = "st." + "abcd1234-ef56-7890." + "0f" * 16 + "." + "a1" * 16
STRIPE = "sk" + "_live_" + "51Hfake0Key1For2Tests"
PASSWORD = "Tr0ub4dor&3-horse-battery"
HEX_KEY = "9f" * 32
B64_SECRET = "Qm9ndXNTZWNyZXQ" + "Rm9yVGVzdHM0Mz" + "IxMjM0NTY3ODkw"


def _all_secret_values() -> list[str]:
    """Secrets recognisable by shape alone. A bare password is not among them:
    without a name beside it (`password=`, `--password`, a URL's userinfo) it
    is indistinguishable from a word, and the named forms are tested below."""
    return [
        GH_CLASSIC,
        GH_FINE,
        JWT,
        AWS_ID,
        ANTHROPIC,
        SLACK,
        INFISICAL,
        STRIPE,
        HEX_KEY,
        B64_SECRET,
    ]


# -- the redactor ----------------------------------------------------------


@pytest.mark.parametrize(
    ("label", "text", "secret"),
    [
        (
            "github classic token",
            f"gh auth login --with-token <<< {GH_CLASSIC}",
            GH_CLASSIC,
        ),
        ("github fine-grained token", f"export GH_TOKEN={GH_FINE}", GH_FINE),
        ("jwt in a header", f"curl -H 'X-Session: {JWT}' https://api", JWT),
        ("aws access key id", f"aws configure set aws_access_key_id {AWS_ID}", AWS_ID),
        ("anthropic key", f"ANTHROPIC_API_KEY={ANTHROPIC} claude -p hi", ANTHROPIC),
        ("slack bot token", f'{{"token": "{SLACK}"}}', SLACK),
        (
            "infisical service token",
            f"INFISICAL_TOKEN={INFISICAL} infisical run",
            INFISICAL,
        ),
        ("stripe key", f"stripe --api-key {STRIPE} charges list", STRIPE),
        ("bearer header", f'curl -H "Authorization: Bearer {B64_SECRET}"', B64_SECRET),
        ("password flag", f"mysql --password={PASSWORD} -u root", PASSWORD),
        ("password env", f"DB_PASSWORD='{PASSWORD}' ./migrate", PASSWORD),
        ("json password", f'{{"user": "a", "password": "{PASSWORD}"}}', PASSWORD),
        ("yaml secret", f"client_secret: {PASSWORD}", PASSWORD),
        (
            "url userinfo",
            f"git clone https://bot:{PASSWORD}@git.example.org/a.git",
            PASSWORD,
        ),
        ("long hex key", f"echo {HEX_KEY} > key.bin", HEX_KEY),
        ("random blob", f"value is {B64_SECRET} here", B64_SECRET),
    ],
)
def test_redact_removes_credentials(label: str, text: str, secret: str) -> None:
    redacted = redact(text)
    assert secret not in redacted, label
    assert "REDACTED" in redacted, label


def test_redact_removes_a_private_key_block_whole() -> None:
    text = f"cat ~/.ssh/id_ed25519\n{PEM}\ndone"
    redacted = redact(text)
    assert "PRIVATE KEY" not in redacted
    assert "b3BlbnNzaC1rZXkt" not in redacted
    assert redacted.startswith("cat ~/.ssh/id_ed25519")
    assert redacted.endswith("done")


def test_redact_removes_a_truncated_private_key() -> None:
    """A tool output cut off mid-key still never keeps the key body."""
    cut = PEM[: PEM.index("-----END")]
    redacted = redact("prefix " + cut)
    assert "b3BlbnNzaC1rZXkt" not in redacted
    assert "[REDACTED:pem]" in redacted


def test_redact_keeps_what_makes_a_row_useful() -> None:
    """Commit ids, UUIDs, Vogt ids, paths and ordinary words survive."""
    sha = "4a4cf5a0" * 5
    text = (
        f"git show {sha} -- src/vogt/storage/sqlite/migrations/observed "
        "session 01a08fac-ae53-7f70-952d-855ee60f0a64 ses_01M42R7B5EWE0FVEQCN43ZAJXR"
    )
    assert redact(text) == text


def test_summaries_and_excerpts_never_carry_any_fake_secret() -> None:
    every = " ".join(_all_secret_values()) + "\n" + PEM
    summary = summarize_input("Bash", {"command": f"echo token={every}"})
    kept = excerpt(f"Error: {every}\n" * 3)
    for secret in _all_secret_values():
        assert secret not in summary
        assert secret not in kept
    assert "PRIVATE KEY" not in kept


def test_an_environment_dump_command_withholds_its_output() -> None:
    assert dumps_secrets("docker inspect stack | jq '.config.environment'")
    assert dumps_secrets("printenv | sort")
    assert dumps_secrets("kubectl config view --raw")
    assert dumps_secrets("cat ~/.kube/config")
    assert dumps_secrets("cat deploy/.env")
    assert not dumps_secrets("git status --short")
    assert result_excerpt("anything at all", error=True, withheld=True) == WITHHELD


def test_output_shaped_like_a_kubeconfig_or_env_dump_is_withheld() -> None:
    kubeconfig = (
        "apiVersion: v1\nkind: Config\nclusters:\n- cluster:\n"
        "    certificate-authority-data: LS0tLS1CRUdJTi\nusers:\n- name: admin\n"
        "  user:\n    client-key-data: LS0tLS1CRUdJTi\n"
    )
    env_dump = f"HOME=/root\nPATH=/usr/bin\nGITHUB_TOKEN={GH_CLASSIC}\nSHELL=/bin/sh\n"
    assert looks_like_dump(kubeconfig)
    assert looks_like_dump(env_dump)
    assert result_excerpt(kubeconfig, error=True, withheld=False) == WITHHELD
    assert result_excerpt(env_dump, error=True, withheld=False) == WITHHELD
    assert not looks_like_dump("error: build failed\nsee log")


def test_only_failures_keep_an_excerpt_and_it_is_short() -> None:
    assert result_excerpt("all good", error=False, withheld=False) is None
    long_output = "Error: start\n" + ("x " * 50_000) + "\nfinal reason: disk full"
    kept = result_excerpt(long_output, error=True, withheld=False)
    assert kept is not None
    assert len(kept) < 500
    assert kept.startswith("Error: start")
    assert kept.endswith("final reason: disk full")


# -- heuristics ------------------------------------------------------------


def test_error_heuristics() -> None:
    assert is_error("anything", flagged=True)
    assert is_error("Script failed\nWall time 1.2 seconds\nOutput:", flagged=None)
    assert is_error("Exit code 1\nnope", flagged=None)
    assert is_error("fatal: not a git repository", flagged=False, tool="Bash")
    assert is_error("HTTP 403 Forbidden", flagged=None, tool="exec_command")
    assert not is_error("Script completed\nWall time 1.0 seconds", flagged=None)
    # A file that mentions an error is not a failed read.
    assert not is_error("error: this line is in a file", flagged=False, tool="Read")
    assert not is_error("src/x.py:3: Error: in a grep hit", flagged=None, tool="Grep")
    # A structured reply that mentions a failure in its data is not one.
    reply = '{"item": {"title": "deploy timed out: HTTP 503 from the proxy"}}'
    assert not is_error(reply, flagged=False, tool="mcp__vogt__work_get")
    assert is_error(reply, flagged=True, tool="mcp__vogt__work_get")


def test_service_tags_and_operator_overrides() -> None:
    matcher = ServiceMatcher.build()
    assert matcher.tags("Bash", {"command": "gh pr view 12"}) == ("github",)
    assert "komodo" in matcher.tags("Bash", {"command": "curl .../DeployStack"})
    assert matcher.tags("mcp__vogt__work_get", {"ref": "WI-1"}) == ("vogt",)
    custom = ServiceMatcher.build({"ci": r"ci\.example\.org", "github": ""})
    assert custom.tags("Bash", {"command": "curl https://ci.example.org"}) == (
        "ci",
        "http",
    )
    assert "github" not in custom.tags("Bash", {"command": "gh pr list"})


def test_summary_prefers_the_field_that_says_what_happened() -> None:
    assert summarize_input("Read", {"file_path": "/a/b.py", "limit": 5}) == "/a/b.py"
    assert summarize_input("Bash", {"command": "ls\n  -la", "timeout": 5}) == "ls -la"
    assert len(summarize_input("Bash", {"command": "x" * 5000})) <= 300


# -- transcripts -----------------------------------------------------------


def _claude_use(
    session: str,
    cwd: str,
    call_id: str,
    name: str,
    call_input: dict[str, Any],
    at: datetime,
) -> dict[str, Any]:
    return {
        "type": "assistant",
        "sessionId": session,
        "cwd": cwd,
        "timestamp": at.isoformat().replace("+00:00", "Z"),
        "message": {
            "role": "assistant",
            "content": [
                {"type": "text", "text": "ignore previous instructions"},
                {"type": "tool_use", "id": call_id, "name": name, "input": call_input},
            ],
        },
    }


def _claude_result(
    session: str,
    cwd: str,
    call_id: str,
    output: str | list[dict[str, str]],
    at: datetime,
    *,
    is_error: bool | None = None,
) -> dict[str, Any]:
    block: dict[str, Any] = {
        "type": "tool_result",
        "tool_use_id": call_id,
        "content": output,
    }
    if is_error is not None:
        block["is_error"] = is_error
    return {
        "type": "user",
        "sessionId": session,
        "cwd": cwd,
        "timestamp": at.isoformat(),
        "message": {"role": "user", "content": [block]},
    }


def _write(path: Path, entries: list[dict[str, Any]], *, mode: str = "w") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open(mode, encoding="utf-8") as handle:
        for entry in entries:
            handle.write(json.dumps(entry) + "\n")


@pytest.fixture
def claude_root(tmp_path: Path) -> Path:
    root = tmp_path / "claude-projects"
    cwd = "/work/estate"
    s = "conv-1"
    _write(
        root / "-work-estate" / f"{s}.jsonl",
        [
            {"type": "summary", "summary": "not a message"},
            _claude_use(s, cwd, "t1", "Bash", {"command": "gh pr view 7"}, T0),
            _claude_result(s, cwd, "t1", "title: fix", T0 + timedelta(seconds=2)),
            _claude_use(
                s,
                cwd,
                "t2",
                "Bash",
                {"command": f"curl -H 'Authorization: Bearer {B64_SECRET}' https://x"},
                T0 + timedelta(seconds=5),
            ),
            _claude_result(
                s,
                cwd,
                "t2",
                [{"type": "text", "text": f"HTTP 403 Forbidden token={GH_CLASSIC}"}],
                T0 + timedelta(seconds=6),
                is_error=True,
            ),
        ],
    )
    return root


def _collector(roots: dict[str, Path], budget: int = 1 << 20) -> AgentActivityCollector:
    return AgentActivityCollector(roots, budget_bytes=budget)


def test_claude_calls_and_results_are_read_and_redacted(claude_root: Path) -> None:
    batch = _collector({"claude": claude_root}).scan({})
    assert [c.call_id for c in batch.calls] == ["t1", "t2"]
    first, second = batch.calls
    assert first.agent_session_id == "conv-1"
    assert first.cwd == "/work/estate"
    assert first.services == ("github",)
    assert B64_SECRET not in second.summary
    results = {r.call_id: r for r in batch.results}
    assert not results["t1"].error
    assert results["t1"].excerpt is None
    assert results["t2"].error
    assert results["t2"].excerpt is not None
    assert GH_CLASSIC not in results["t2"].excerpt
    assert "403" in results["t2"].excerpt


def test_reading_is_incremental_and_waits_for_whole_lines(
    claude_root: Path, tmp_path: Path
) -> None:
    store = SqliteObservedStore(tmp_path / "observed.sqlite3")
    store.migrate()
    collector = _collector({"claude": claude_root})

    def run() -> tuple[int, int]:
        sweep_ = store.begin_sweep(collector="agent-activity", scope=[], at=T0)
        stats = store.index_activity(
            sweep_.id, collector.scan(store.activity_cursors()), at=T0
        )
        return stats.calls, stats.results

    assert run() == (2, 2)
    assert run() == (0, 0)  # nothing new: nothing re-read

    path = claude_root / "-work-estate" / "conv-1.jsonl"
    late = T0 + timedelta(minutes=1)
    _write(
        path,
        [_claude_use("conv-1", "/w", "t3", "Read", {"file_path": "/w/a"}, late)],
        mode="a",
    )
    # Half a line, still being written by the agent.
    with path.open("a", encoding="utf-8") as handle:
        handle.write('{"type": "user", "sessionId": "conv-1", "mess')
    assert run() == (1, 0)
    cursor = store.activity_cursors()[str(path)]
    assert cursor.offset < path.stat().st_size

    # The line completes, carrying t3's result: the call is completed in place.
    with path.open("a", encoding="utf-8") as handle:
        handle.write('age": {"content": [{"type": "tool_result", "tool_use_id": "t3", ')
        handle.write(f'"content": "ok"}}]}}, "timestamp": "{late.isoformat()}"}}\n')
    assert run() == (0, 1)
    rows = store.search_activity(ActivityQuery(tool="Read"), limit=10, offset=0)
    assert rows[0].finished_at is not None


def test_a_replaced_file_is_read_again_from_the_start(
    claude_root: Path, tmp_path: Path
) -> None:
    store = SqliteObservedStore(tmp_path / "observed.sqlite3")
    store.migrate()
    collector = _collector({"claude": claude_root})
    sweep_ = store.begin_sweep(collector="agent-activity", scope=[], at=T0)
    store.index_activity(sweep_.id, collector.scan({}), at=T0)
    path = claude_root / "-work-estate" / "conv-1.jsonl"
    _write(path, [_claude_use("conv-2", "/w", "n1", "Bash", {"command": "ls"}, T0)])
    batch = collector.scan(store.activity_cursors())
    assert [c.call_id for c in batch.calls] == ["n1"]
    assert batch.calls[0].agent_session_id == "conv-2"


def test_a_sweep_reads_no_more_than_its_budget(tmp_path: Path) -> None:
    root = tmp_path / "claude"
    for n in range(5):
        _write(
            root / "p" / f"c{n}.jsonl",
            [
                _claude_use(f"c{n}", "/w", f"x{i}", "Bash", {"command": "ls " * 50}, T0)
                for i in range(20)
            ],
        )
    total = sum(p.stat().st_size for p in root.rglob("*.jsonl"))
    collector = _collector({"claude": root}, budget=total // 3)
    batch = collector.scan({})
    assert batch.bytes_read <= total // 3 + 2_000  # at most one line past budget
    assert batch.backlog_bytes == total - batch.bytes_read
    cursors = {c.path: c for c in batch.cursors}
    second = collector.scan(cursors)
    assert second.bytes_read > 0
    assert {(c.source_path, c.call_id) for c in batch.calls}.isdisjoint(
        {(c.source_path, c.call_id) for c in second.calls}
    )


def test_codex_rollouts_are_read(tmp_path: Path) -> None:
    root = tmp_path / "codex"
    session = "01a08fac-ae53-7f70-952d-855ee60f0a64"
    at = T0.isoformat()
    _write(
        root / "2026" / "10" / "04" / f"rollout-2026-10-04T06-00-00-{session}.jsonl",
        [
            {
                "timestamp": at,
                "type": "session_meta",
                "payload": {"id": session, "cwd": "/work/estate"},
            },
            {
                "timestamp": at,
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call",
                    "call_id": "c1",
                    "name": "exec",
                    "input": (
                        'const r = await tools.exec_command({cmd:"docker compose ps"});'
                    ),
                },
            },
            {
                "timestamp": at,
                "type": "response_item",
                "payload": {
                    "type": "custom_tool_call_output",
                    "call_id": "c1",
                    "output": [{"type": "input_text", "text": "Script failed\nboom"}],
                },
            },
            {
                "timestamp": at,
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "call_id": "c2",
                    "name": "spawn_agent",
                    "arguments": json.dumps({"task_name": "review", "message": "go"}),
                },
            },
            {
                "timestamp": at,
                "type": "response_item",
                "payload": {
                    "type": "function_call_output",
                    "call_id": "c2",
                    "output": "started",
                },
            },
        ],
    )
    batch = _collector({"codex": root}).scan({})
    tools = [(c.tool, c.summary) for c in batch.calls]
    assert tools[0] == ("exec_command", "docker compose ps")
    assert tools[1][0] == "spawn_agent"
    assert all(c.agent_session_id == session for c in batch.calls)
    assert all(c.cwd == "/work/estate" for c in batch.calls)
    assert batch.calls[0].services == ("docker",)
    errors = {r.call_id: r.error for r in batch.results}
    assert errors == {"c1": True, "c2": False}


def test_a_dump_result_read_in_a_later_sweep_is_still_withheld(tmp_path: Path) -> None:
    """The call is in one batch, its result in the next: the store remembers."""
    root = tmp_path / "claude"
    path = root / "p" / "c.jsonl"
    command = "docker inspect vogt | jq '.[0].Config.Env' # .config.environment"
    _write(path, [_claude_use("c", "/w", "d1", "Bash", {"command": command}, T0)])
    store = SqliteObservedStore(tmp_path / "observed.sqlite3")
    store.migrate()
    collector = _collector({"claude": root})
    for _ in range(2):
        sweep_ = store.begin_sweep(collector="agent-activity", scope=[], at=T0)
        store.index_activity(sweep_.id, collector.scan(store.activity_cursors()), at=T0)
        _write(
            path,
            [
                _claude_result(
                    "c", "/w", "d1", f"Error: x\nAPI_KEY={ANTHROPIC}", T0, is_error=True
                )
            ],
            mode="a",
        )
    row = store.search_activity(ActivityQuery(), limit=5, offset=0)[0]
    assert row.error
    assert row.excerpt == WITHHELD


def test_a_batch_indexed_twice_stores_each_call_once(
    claude_root: Path, tmp_path: Path
) -> None:
    store = SqliteObservedStore(tmp_path / "observed.sqlite3")
    store.migrate()
    batch = _collector({"claude": claude_root}).scan({})
    sweep_ = store.begin_sweep(collector="agent-activity", scope=[], at=T0)
    assert store.index_activity(sweep_.id, batch, at=T0).calls == 2
    assert store.index_activity(sweep_.id, batch, at=T0).calls == 0
    assert len(store.search_activity(ActivityQuery(), limit=10, offset=0)) == 2


def test_store_filters_narrow(claude_root: Path, tmp_path: Path) -> None:
    store = SqliteObservedStore(tmp_path / "observed.sqlite3")
    store.migrate()
    other = claude_root / "-work-estate_two" / "conv-9.jsonl"
    _write(
        other,
        [
            _claude_use(
                "conv-9", "/work/estate_two/sub", "z", "Bash", {"command": "ls"}, T0
            )
        ],
    )
    sweep_ = store.begin_sweep(collector="agent-activity", scope=[], at=T0)
    store.index_activity(sweep_.id, _collector({"claude": claude_root}).scan({}), at=T0)

    def ids(**kwargs: Any) -> list[str]:
        rows = store.search_activity(ActivityQuery(**kwargs), limit=50, offset=0)
        return sorted(r.tool + ":" + r.agent_session_id for r in rows)

    assert len(ids()) == 3
    assert len(ids(errors_only=True)) == 1
    assert len(ids(service="github")) == 1
    assert ids(service="git") == []  # an exact tag, not a prefix
    assert len(ids(q="pr view")) == 1
    assert len(ids(q="403")) == 1  # the excerpt is searched too
    assert ids(q="100%") == []  # LIKE metacharacters are literal
    assert ids(agent_session_ids=("conv-9",)) == ["Bash:conv-9"]
    assert ids(cwd_roots=("/work/estate_two",)) == ["Bash:conv-9"]
    # `_` in a root is a character, not a wildcard, and a sibling is not inside.
    assert len(ids(cwd_roots=("/work/estate",))) == 2
    assert ids(since=T0 + timedelta(seconds=4)) == ["Bash:conv-1"]

    summary = store.summarize_activity(
        ActivityQuery(agent_session_ids=("conv-1",)), limit=10, offset=0
    )
    assert len(summary) == 1
    assert summary[0].calls == 2
    assert summary[0].errors == 1
    assert summary[0].wait_ms == 3000
    assert summary[0].tools == {"Bash": 2}
    assert summary[0].services == {"github": 1, "http": 1}


# -- the operations --------------------------------------------------------


@pytest.fixture
def configured(
    instance: AppContext, claude_root: Path, tmp_path: Path
) -> tuple[AppContext, StandInEngine]:
    engine = StandInEngine()
    estate = tmp_path / "estate"
    estate.mkdir()
    config: VogtConfig = instance.config.model_copy(
        update={"agent_activity_roots": {"claude": claude_root}}
    )
    ctx = dataclasses.replace(
        instance,
        config=config,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Estate", root_path=str(estate), reason=WHY)
    )
    native_work_item(ctx, kind="bug", title="Something", project="estate")
    return ctx, engine


def test_unconfigured_activity_says_so(instance: AppContext) -> None:
    assert "agent-activity" not in collector_registry(instance).names
    result = search_agent_activity(instance, AgentActivitySearchParams())
    assert result.events == []
    assert result.detail == NOT_CONFIGURED
    summary = summarize_agent_activity(instance, AgentActivitySummaryParams())
    assert summary.detail == NOT_CONFIGURED


def test_configured_but_unswept_says_so(
    configured: tuple[AppContext, StandInEngine],
) -> None:
    ctx, _ = configured
    assert search_agent_activity(ctx, AgentActivitySearchParams()).detail == (
        NOT_YET_INDEXED
    )


def test_a_sweep_indexes_and_the_ops_link_project_and_vogt_session(
    configured: tuple[AppContext, StandInEngine], claude_root: Path, tmp_path: Path
) -> None:
    ctx, _engine = configured
    started = start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    assert started.session.engine_session_id == "eng-1"
    estate = str(tmp_path / "estate")
    # A Claude session Vogt started: its conversation id is the engine's id.
    _write(
        claude_root / "-estate" / "eng-1.jsonl",
        [
            _claude_use(
                "eng-1",
                f"{estate}/.claude/worktrees/a",
                "v1",
                "Bash",
                {"command": "git push"},
                T0,
            ),
            _claude_result(
                "eng-1",
                estate,
                "v1",
                "rejected",
                T0 + timedelta(seconds=1),
                is_error=True,
            ),
        ],
    )

    result = sweep(ctx, SweepParams(reason=WHY))
    report = next(r for r in result.reports if r.collector == "agent-activity")
    assert report.outcome == "ok"
    assert report.new == 3

    found = search_agent_activity(ctx, AgentActivitySearchParams(project="estate"))
    assert [e.agent_session_id for e in found.events] == ["eng-1"]
    event = found.events[0]
    assert event.project == "estate"
    assert event.vogt_session_id == started.session.id
    assert event.error
    assert event.duration_ms == 1000
    assert found.indexed_at is not None
    assert found.detail is None

    by_session = summarize_agent_activity(
        ctx, AgentActivitySummaryParams(session=started.session.id)
    )
    assert [s.agent_session_id for s in by_session.sessions] == ["eng-1"]
    only = by_session.sessions[0]
    assert only.calls == 1
    assert only.error_rate == 1.0
    assert only.services == {"git": 1}
    assert only.vogt_session_id == started.session.id

    unlinked = search_agent_activity(
        ctx, AgentActivitySearchParams(session="conv-1", errors_only=True)
    )
    assert [e.vogt_session_id for e in unlinked.events] == [None]

    entries = {e.collector: e for e in coverage(ctx, CoverageParams()).collectors}
    assert "agent-activity" in entries


def test_a_project_scoped_sweep_does_not_read_transcripts(
    configured: tuple[AppContext, StandInEngine],
) -> None:
    ctx, _ = configured
    result = sweep(ctx, SweepParams(project="estate", reason=WHY))
    assert "agent-activity" not in {r.collector for r in result.reports}
    named = sweep(
        ctx, SweepParams(project="estate", collectors=["agent-activity"], reason=WHY)
    )
    assert [r.collector for r in named.reports] == ["agent-activity"]


def test_an_unknown_session_is_a_typed_error(
    configured: tuple[AppContext, StandInEngine],
) -> None:
    ctx, _ = configured
    sweep(ctx, SweepParams(collectors=["agent-activity"], reason=WHY))
    with pytest.raises(NotFound):
        search_agent_activity(ctx, AgentActivitySearchParams(session="ses_nope"))


def test_config_rejects_unknown_formats_and_bad_patterns(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="claude"):
        VogtConfig(data_dir=tmp_path, agent_activity_roots={"cursor": tmp_path})
    with pytest.raises(ValueError, match="regex"):
        VogtConfig(data_dir=tmp_path, agent_activity_services={"x": "("})
