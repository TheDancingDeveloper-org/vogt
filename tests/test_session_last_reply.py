"""A session's own agent replies, read from its transcript (WI-872, WI-876)."""

from __future__ import annotations

import dataclasses
import json
import os
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters import transcripts
from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    ListSessionsParams,
    RegisterProjectParams,
    SessionLastReplyParams,
    StartSessionParams,
)
from vogt.application.services import (
    last_reply,
    list_sessions,
    register_project,
    start_session,
)
from vogt.errors import NotFound

WHY = "last reply test"
ROOT = "/srv/vogt"
ENGINE_ID = "0f8fad5b-d9cb-469f-a165-70867728950e"
SECRET = "ghp_" + "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"


def _claude_line(message_id: str, text: str, **extra: Any) -> str:
    return json.dumps(
        {
            "type": "assistant",
            "timestamp": "2026-10-05T00:01:00Z",
            "cwd": ROOT,
            "message": {
                "id": message_id,
                "role": "assistant",
                "content": [{"type": "text", "text": text}],
            },
            **extra,
        }
    )


def _write_claude(root: Path, conversation: str, lines: list[str]) -> Path:
    directory = root / transcripts.claude_key(ROOT)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{conversation}.jsonl"
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


# -- the adapter -------------------------------------------------------------


def test_claude_replies_are_grouped_by_message_and_redacted(tmp_path: Path) -> None:
    root = tmp_path / "claude"
    _write_claude(
        root,
        ENGINE_ID,
        [
            json.dumps({"type": "user", "message": {"content": "hi"}}),
            _claude_line("m1", "First reply."),
            _claude_line("m2", "Part one,"),
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "id": "m2",
                        "content": [{"type": "tool_use", "name": "Bash"}],
                    },
                }
            ),
            _claude_line("m2", f"part two with {SECRET}."),
            _claude_line("sub", "a subagent's reply", isSidechain=True),
            "not json",
        ],
    )
    found = transcripts.find(
        {"claude": root},
        engine_session_id=ENGINE_ID,
        command="vogt-agent-auth run -- claude --session-id " + ENGINE_ID,
        template=None,
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=False,
    )
    assert found is not None
    assert found.basis == "session-id" and found.agent == "claude"
    replies = transcripts.last_replies(found, 5)
    assert [r.text.split()[0] for r in replies] == ["First", "Part"]
    assert "part two" in replies[-1].text
    assert SECRET not in replies[-1].text, "redacted like the activity index"
    assert replies[-1].at is not None


def test_a_resumed_or_engine_started_conversation_is_found_by_its_id(
    tmp_path: Path,
) -> None:
    root = tmp_path / "claude"
    _write_claude(root, "resumed-1", [_claude_line("m", "resumed reply")])
    by_resume = transcripts.find(
        {"claude": root},
        engine_session_id="11111111-2222-4333-8444-555555555555",
        command="claude --resume resumed-1",
        template=None,
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=False,
    )
    assert by_resume is not None and by_resume.basis == "resume-id"
    _write_claude(root, ENGINE_ID, [_claude_line("m", "pinned")])
    by_engine = transcripts.find(
        {"claude": root},
        engine_session_id=ENGINE_ID,
        command="claude",
        template="claude",
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=False,
    )
    assert by_engine is not None and by_engine.basis == "engine-id"


def test_the_directory_guess_is_only_made_when_allowed(tmp_path: Path) -> None:
    root = tmp_path / "claude"
    _write_claude(root, "some-other-id", [_claude_line("m", "guessed")])
    kwargs: dict[str, Any] = {
        "engine_session_id": ENGINE_ID,
        "command": "bash",
        "template": None,
        "cwd": ROOT,
        "started_at": None,
    }
    assert transcripts.find({"claude": root}, allow_cwd_guess=False, **kwargs) is None
    guessed = transcripts.find({"claude": root}, allow_cwd_guess=True, **kwargs)
    assert guessed is not None and guessed.basis == "cwd"
    assert guessed.conversation_id == "some-other-id"


def test_codex_rollouts_by_id_and_by_directory(tmp_path: Path) -> None:
    root = tmp_path / "codex"
    day = root / "2026" / "10" / "05"
    day.mkdir(parents=True)
    conversation = "0199aaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee"
    rollout = day / f"rollout-2026-10-05T00-00-00-{conversation}.jsonl"
    rollout.write_text(
        "\n".join(
            json.dumps(entry)
            for entry in [
                {"type": "session_meta", "payload": {"id": conversation, "cwd": ROOT}},
                {
                    "type": "response_item",
                    "timestamp": "2026-10-05T00:02:00Z",
                    "payload": {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Codex says hi"}],
                    },
                },
                {
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": "not a reply"}],
                    },
                },
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    by_id = transcripts.find(
        {"codex": root},
        engine_session_id=ENGINE_ID,
        command=f"codex resume {conversation}",
        template=None,
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=False,
    )
    assert by_id is not None and by_id.agent == "codex"
    assert [r.text for r in transcripts.last_replies(by_id, 3)] == ["Codex says hi"]
    by_cwd = transcripts.find(
        {"codex": root},
        engine_session_id=ENGINE_ID,
        command="codex",
        template=None,
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=True,
    )
    assert by_cwd is not None and by_cwd.conversation_id == conversation


def test_the_excerpt_is_cached_by_the_files_size_and_time(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    transcripts.clear_excerpt_cache()
    path = _write_claude(
        tmp_path / "claude", ENGINE_ID, [_claude_line("m", "word " * 200)]
    )
    found = transcripts.Transcript("claude", path, ENGINE_ID, "engine-id")
    first = transcripts.last_reply_excerpt(found)
    assert first is not None and len(first) <= transcripts.EXCERPT_CHARS

    def no_reread(*_: object) -> list[transcripts.Reply]:
        raise AssertionError("an unchanged transcript was read again")

    monkeypatch.setattr(transcripts, "last_replies", no_reread)
    assert transcripts.last_reply_excerpt(found) == first
    monkeypatch.undo()
    # A new reply changes the size and time, so it is read afresh.
    with path.open("a", encoding="utf-8") as handle:
        handle.write(_claude_line("m9", "the newest reply") + "\n")
    stat = path.stat()
    os.utime(path, ns=(stat.st_atime_ns, stat.st_mtime_ns + 1_000_000))
    assert transcripts.last_reply_excerpt(found) == "the newest reply"


def test_named_ids_ignore_anything_that_is_not_an_id() -> None:
    assert transcripts.named_ids("claude --resume abc-1 --model x") == [
        ("abc-1", "resume-id")
    ]
    assert transcripts.named_ids("claude --session-id 'u-1'") == [("u-1", "session-id")]
    assert transcripts.named_ids(None) == []


# -- the operation -------------------------------------------------------------


class Engine:
    def __init__(self) -> None:
        self.command = "vogt-agent-auth run -- claude --session-id " + ENGINE_ID

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        summary = {
            "id": ENGINE_ID,
            "name": "agent",
            "activity": "idle",
            "cwd": ROOT,
            "alive": True,
            "created_at": "2026-10-05T00:00:00Z",
            "command": self.command,
        }
        if method == "POST" and path == "/api/sessions":
            return 200, json.dumps(summary).encode()
        if path == "/api/sessions":
            return 200, json.dumps([summary]).encode()
        if path == f"/api/sessions/{ENGINE_ID}":
            return 200, json.dumps({"summary": summary}).encode()
        return 404, b""


@pytest.fixture
def wired(instance: AppContext, tmp_path: Path) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=Engine()),
        config=instance.config.model_copy(
            update={"session_transcript_roots": {"claude": tmp_path / "claude"}}
        ),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    return ctx


def test_last_reply_reads_the_sessions_own_conversation(
    wired: AppContext, tmp_path: Path
) -> None:
    started = start_session(
        wired, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    _write_claude(
        tmp_path / "claude",
        ENGINE_ID,
        [
            _claude_line("a", "older"),
            _claude_line("b", "The operator list: token, HAR."),
        ],
    )
    result = last_reply(wired, SessionLastReplyParams(id=started.session.id, n=2))
    assert result.engine_session_id == ENGINE_ID
    assert result.agent == "claude" and result.basis == "session-id"
    assert [m.text for m in result.messages] == [
        "older",
        "The operator list: token, HAR.",
    ]
    assert result.detail is None
    # And the list carries the excerpt of the latest one.
    rows = list_sessions(wired, ListSessionsParams()).sessions
    assert rows[0].last_reply_excerpt == "The operator list: token, HAR."


def test_no_transcript_is_an_honest_detail_not_an_error(wired: AppContext) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    result = last_reply(wired, SessionLastReplyParams(id=started.session.id))
    assert result.messages == []
    assert result.detail is not None and "no agent transcript" in result.detail
    rows = list_sessions(wired, ListSessionsParams()).sessions
    assert rows[0].last_reply_excerpt is None


def test_reading_is_off_with_no_roots(instance: AppContext) -> None:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=Engine()),
    )
    result = last_reply(ctx, SessionLastReplyParams(id=ENGINE_ID))
    assert result.detail is not None and "off" in result.detail


def test_an_unknown_session_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        last_reply(wired, SessionLastReplyParams(id="ses_nope"))


# -- what a session is running (WI-919) ---------------------------------------


def test_the_list_says_which_model_the_conversation_actually_ran(
    wired: AppContext, tmp_path: Path
) -> None:
    from vogt.application.models import ListSessionsParams
    from vogt.application.services import list_sessions

    started = start_session(
        wired, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    _write_claude(
        tmp_path / "claude",
        ENGINE_ID,
        [
            _claude_line("a", "first"),
            json.dumps(
                {
                    "type": "assistant",
                    "message": {
                        "id": "b",
                        "model": "claude-opus-5-5",
                        "content": [{"type": "text", "text": "second"}],
                    },
                }
            ),
            json.dumps(
                {
                    "type": "assistant",
                    "message": {"id": "c", "model": "<synthetic>", "content": []},
                }
            ),
        ],
    )
    row = next(
        s
        for s in list_sessions(wired, ListSessionsParams()).sessions
        if s.id == started.session.id
    )
    assert row.model is None, "nothing was asked for"
    assert row.running is not None
    assert row.running.agent == "claude"
    assert row.running.model == "claude-opus-5-5"
    assert row.running.model_basis == "transcript"
    assert row.template == "claude"


def test_codex_records_model_and_effort_per_turn(tmp_path: Path) -> None:
    conversation = "0199aaaa-bbbb-4ccc-8ddd-eeeeeeeeeeef"
    path = tmp_path / f"rollout-{conversation}.jsonl"
    path.write_text(
        "\n".join(
            json.dumps(e)
            for e in [
                {
                    "type": "turn_context",
                    "payload": {"model": "gpt-5.5", "effort": "low"},
                },
                {
                    "type": "turn_context",
                    "payload": {"model": "gpt-5.6", "effort": "high"},
                },
            ]
        )
        + "\n",
        encoding="utf-8",
    )
    found = transcripts.Transcript(
        agent="codex", path=path, conversation_id=conversation, basis="resume-id"
    )
    assert transcripts.runtime(found) == ("gpt-5.6", "high")


def test_without_a_transcript_the_command_line_and_the_ask_answer() -> None:
    from vogt.core.runtime import from_command, resolve

    flags = from_command(
        "vogt-agent-auth run -- codex -m gpt-5.6 -c model_reasoning_effort=xhigh"
    )
    assert (flags.agent, flags.model, flags.effort) == ("codex", "gpt-5.6", "xhigh")
    resolved = resolve(
        command="claude --session-id x --effort high",
        conversation_agent=None,
        transcript_model=None,
        transcript_effort=None,
        asked_model="claude-sonnet-5-5",
        asked_effort=None,
    )
    assert (resolved.model, resolved.model_basis) == ("claude-sonnet-5-5", "asked")
    assert (resolved.effort, resolved.effort_basis) == ("high", "command")
    nothing = resolve(
        command="bash",
        conversation_agent=None,
        transcript_model=None,
        transcript_effort=None,
        asked_model=None,
        asked_effort=None,
    )
    assert (
        nothing.model is None and nothing.model_basis is None and nothing.agent is None
    )


# -- Klaudia, which writes Claude Code's format under its own root (WI-950) ---


def test_a_klaudia_session_is_read_from_its_own_root_in_claude_format(
    tmp_path: Path,
) -> None:
    """Klaudia files `<id>.jsonl` per directory in Claude Code's line format,
    under `~/.klaudia/sessions`; its assistant lines carry no message id or
    model, so each line is one reply and the model comes from the command."""
    claude_root = tmp_path / "claude"
    klaudia_root = tmp_path / "klaudia"
    # A Claude transcript with the same id must not be the answer: the
    # command says which agent the session runs.
    _write_claude(claude_root, ENGINE_ID, [_claude_line("m", "the wrong agent")])
    directory = klaudia_root / transcripts.claude_key(ROOT)
    directory.mkdir(parents=True)
    (directory / f"{ENGINE_ID}.jsonl").write_text(
        "\n".join(
            json.dumps(entry)
            for entry in (
                {
                    "type": "user",
                    "message": {"content": [{"type": "text", "text": "go"}]},
                },
                {
                    "type": "assistant",
                    "timestamp": "2026-10-06T09:52:00Z",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": "Iteration one done."}],
                    },
                },
                {
                    "type": "assistant",
                    "timestamp": "2026-10-06T09:53:00Z",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": f"Pushed with {SECRET}."}],
                    },
                },
            )
        )
        + "\n",
        encoding="utf-8",
    )
    found = transcripts.find(
        {"claude": claude_root, "klaudia": klaudia_root},
        engine_session_id=ENGINE_ID,
        command=(
            f"vogt-agent-auth run -- klaudia --model grok-4.7 --session-id {ENGINE_ID}"
        ),
        template="klaudia",
        cwd=ROOT,
        started_at=None,
        allow_cwd_guess=False,
    )
    assert found is not None
    assert (found.agent, found.basis) == ("klaudia", "session-id")
    replies = transcripts.last_replies(found, 5)
    assert [r.text.split()[0] for r in replies] == ["Iteration", "Pushed"]
    assert SECRET not in replies[-1].text
    assert transcripts.runtime(found) == (None, None), "Klaudia records no model"

    from vogt.core.runtime import from_command

    flags = from_command(f"klaudia --model grok-4.7 --session-id {ENGINE_ID}")
    assert (flags.agent, flags.model) == ("klaudia", "grok-4.7")


def test_session_transcript_roots_know_klaudia_and_nothing_else_new() -> None:
    from pydantic import ValidationError

    from vogt.config import VogtConfig

    assert "klaudia" in VogtConfig().session_transcript_roots
    with pytest.raises(ValidationError):
        VogtConfig(session_transcript_roots={"gemini": Path("/x")})
    # The activity index reads only the two formats its collector parses.
    with pytest.raises(ValidationError):
        VogtConfig(agent_activity_roots={"klaudia": Path("/x")})


# -- opencode, read through the engine (WI-931) --------------------------------


class OpencodeEngine(Engine):
    """An engine whose session runs opencode: no transcript file anywhere, the
    replies come from the engine's `/replies` route."""

    def __init__(self) -> None:
        super().__init__()
        self.command = "vogt-agent-auth run -- opencode"
        self.replies_asked: list[str] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        if path == f"/api/sessions/{ENGINE_ID}/replies":
            self.replies_asked.append(url.split("?", 1)[1])
            return 200, json.dumps(
                {
                    "agent": "opencode",
                    "conversation_id": "ses_abc123",
                    "replies": [
                        {"text": "first answer", "at": "2026-10-05T01:00:00Z"},
                        {
                            "text": "done; token=ghp_" + "a" * 36,
                            "at": "2026-10-05T01:01:00Z",
                        },
                    ],
                }
            ).encode()
        status, payload = super().__call__(url, headers, body, method)
        if status == 200 and path.startswith("/api/sessions"):
            data = json.loads(payload)
            for row in data if isinstance(data, list) else [data.get("summary", data)]:
                row["conversation"] = {"agent": "opencode", "id": "ses_abc123"}
            payload = json.dumps(data).encode()
        return status, payload


def test_an_opencode_sessions_replies_come_from_the_engine_redacted(
    instance: AppContext,
) -> None:
    engine = OpencodeEngine()
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
        # No transcript roots at all: opencode does not need them.
        config=instance.config.model_copy(update={"session_transcript_roots": {}}),
    )
    result = last_reply(ctx, SessionLastReplyParams(id=ENGINE_ID, n=2))
    assert result.agent == "opencode"
    assert result.conversation_id == "ses_abc123"
    assert [m.text.split(";")[0] for m in result.messages] == ["first answer", "done"]
    assert "ghp_" + "a" * 36 not in result.messages[-1].text, (
        "redacted like a transcript"
    )
    assert engine.replies_asked == ["n=2"]

    rows = list_sessions(ctx, ListSessionsParams()).sessions
    excerpt = next(
        r for r in rows if r.engine_session_id == ENGINE_ID
    ).last_reply_excerpt
    assert excerpt is not None and excerpt.startswith("done;")
    assert "ghp_" + "a" * 36 not in excerpt
