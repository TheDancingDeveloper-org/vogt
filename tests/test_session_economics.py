"""C16 session economics: the pure transcript reading."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from vogt.core.session_economics import CHARS_PER_TOKEN, meter


def _line(obj: object) -> str:
    return json.dumps(obj)


def test_claude_usage_is_the_latest_turn_and_peak_is_the_largest() -> None:
    lines = [
        _line({"type": "user", "message": {"role": "user", "content": "go"}}),
        _line(
            {
                "type": "assistant",
                "timestamp": "2026-10-11T01:00:00Z",
                "message": {
                    "model": "claude",
                    "usage": {
                        "input_tokens": 100,
                        "cache_read_input_tokens": 50,
                        "cache_creation_input_tokens": 25,
                        "output_tokens": 10,
                    },
                    "content": [
                        {
                            "type": "tool_result",
                            "name": "Bash",
                            "content": "x" * 40,
                        }
                    ],
                },
            }
        ),
        _line(
            {
                "type": "assistant",
                "timestamp": "2026-10-11T01:00:30Z",
                "message": {
                    "usage": {"input_tokens": 80, "output_tokens": 5},
                    "content": [],
                },
            }
        ),
    ]
    result = meter(lines, agent="claude")
    assert result.turns == 2
    # The latest turn, not the sum: 100+50+25 would double-count history.
    assert result.context_tokens == 80
    assert result.peak_context_tokens == 175
    assert result.output_tokens == 15
    assert result.usage_measured is True
    assert result.compactions == 0
    assert result.tool_result_bytes == {"Bash": 40}
    assert result.wall_clock.count == 1
    assert result.wall_clock.max_ms == 30_000
    assert result.wall_clock.mean_ms == 30_000


def test_claude_compaction_marker_counts_wherever_it_sits() -> None:
    lines = [
        _line({"type": "system", "isCompactSummary": True}),
        _line(
            {
                "type": "assistant",
                "message": {"isCompactSummary": True, "usage": {"input_tokens": 1}},
            }
        ),
    ]
    result = meter(lines, agent="claude")
    assert result.compactions == 2
    assert result.turns == 1


def test_klaudia_estimates_and_counts_its_own_compaction_marker() -> None:
    body = "y" * (CHARS_PER_TOKEN * 10)
    lines = [
        _line({"type": "system", "subtype": "compact_boundary"}),
        _line({"type": "assistant", "message": {"content": body}}),
    ]
    result = meter(lines, agent="klaudia")
    assert result.usage_measured is False
    assert result.compactions == 1
    assert result.turns == 1
    assert result.context_tokens == sum(len(line) for line in lines) // CHARS_PER_TOKEN
    assert result.peak_context_tokens == result.context_tokens


def test_a_repeated_read_counts_once_per_path() -> None:
    def read(path: str) -> str:
        return _line(
            {
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "name": "Read",
                            "input": {"file_path": path},
                            "content": "contents",
                        }
                    ]
                },
            }
        )

    result = meter(
        [read("/a.py"), read("/a.py"), read("/b.py"), read("/a.py")],
        agent="klaudia",
    )
    assert result.reads_repeated == 1
    assert result.tool_result_bytes["Read"] == len("contents") * 4


def test_codex_results_under_payload_are_counted() -> None:
    line = _line(
        {
            "type": "tool_result",
            "payload": {
                "content": [
                    {"type": "tool_result", "tool": "shell", "content": "z" * 8}
                ]
            },
        }
    )
    result = meter([line], agent="codex")
    assert result.turns == 0
    assert result.tool_result_bytes == {"shell": 8}
    assert result.context_tokens == len(line) // CHARS_PER_TOKEN


def test_garbage_lines_are_skipped_and_an_empty_transcript_is_zero() -> None:
    result = meter(["not json", "", "[]"], agent="claude")
    assert result.turns == 0
    assert result.context_tokens == 0
    assert result.usage_measured is False
    assert result.wall_clock.count == 0


def test_one_timestamped_turn_has_no_wall_clock() -> None:
    line = _line(
        {"type": "assistant", "timestamp": "2026-10-11T01:00:00Z", "message": {}}
    )
    assert meter([line], agent="klaudia").wall_clock.count == 0


def test_claude_counts_a_turn_once_across_its_content_blocks() -> None:
    """Claude writes one assistant entry per content block of one message."""

    def block(index: int, tokens: int) -> str:
        return _line(
            {
                "type": "assistant",
                "timestamp": f"2026-10-11T01:00:{index:02d}Z",
                "message": {
                    "id": "msg_1",
                    "usage": {"input_tokens": tokens, "output_tokens": 1},
                    "content": [{"type": "text", "text": "part"}],
                },
            }
        )

    other = _line(
        {
            "type": "assistant",
            "timestamp": "2026-10-11T01:01:00Z",
            "message": {
                "id": "msg_2",
                "usage": {"input_tokens": 9, "output_tokens": 1},
            },
        }
    )
    result = meter([block(1, 40), block(2, 40), other], agent="claude")
    assert result.turns == 2
    # One output reading per turn, not one per content-block entry.
    assert result.output_tokens == 2
    assert result.wall_clock.count == 1


def test_klaudia_compaction_is_a_system_subtype_and_resets_the_estimate() -> None:
    archived = "y" * (CHARS_PER_TOKEN * 100)
    live = "z" * (CHARS_PER_TOKEN * 10)
    lines = [
        _line({"type": "assistant", "message": {"content": archived}}),
        _line({"type": "system", "subtype": "compact_boundary"}),
        _line({"type": "assistant", "message": {"content": live}}),
    ]
    result = meter(lines, agent="klaudia")
    assert result.compactions == 1
    assert result.turns == 2
    live_only = sum(len(line) for line in lines[1:]) // CHARS_PER_TOKEN
    assert result.context_tokens == live_only
    assert result.peak_context_tokens > result.context_tokens


def test_claude_tool_results_are_named_from_the_tool_use_id() -> None:
    lines = [
        _line(
            {
                "type": "assistant",
                "message": {
                    "id": "msg_1",
                    "usage": {"input_tokens": 10, "output_tokens": 1},
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "Read",
                            "input": {"file_path": "/a.py"},
                        }
                    ],
                },
            }
        ),
        _line(
            {
                "type": "user",
                "message": {
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "toolu_1",
                            "content": "contents",
                        }
                    ]
                },
            }
        ),
    ]
    result = meter(lines, agent="claude")
    assert result.tool_result_bytes == {"Read": len("contents")}
    assert result.reads_repeated == 0


def test_budget_line_states_what_is_known_and_omits_what_is_not() -> None:
    result = meter(
        [_line({"type": "assistant", "isCompactSummary": True, "message": {}})],
        agent="klaudia",
    )
    assert result.budget_line(120, None) == "1/120 turns, 1 compaction"
    assert "ctx" not in result.budget_line(None, None)
    measured = meter(
        [
            _line(
                {
                    "type": "assistant",
                    "message": {"usage": {"input_tokens": 61}},
                }
            )
        ],
        agent="claude",
    )
    assert measured.budget_line(120, 100) == "1/120 turns, 61% ctx"


def test_claude_output_tokens_count_once_per_turn_across_its_blocks() -> None:
    """Claude repeats one turn's usage on each content-block entry."""
    usage = {
        "input_tokens": 10,
        "cache_read_input_tokens": 0,
        "cache_creation_input_tokens": 0,
        "output_tokens": 7,
    }
    lines = [
        _line(
            {
                "type": "assistant",
                "message": {"id": "msg_1", "usage": usage, "content": [block]},
            }
        )
        for block in (
            {"type": "thinking", "thinking": "hm"},
            {"type": "text", "text": "hello"},
            {"type": "tool_use", "id": "t1", "name": "Read", "input": {}},
        )
    ]
    reading = meter(lines, agent="claude")
    assert reading.turns == 1
    assert reading.output_tokens == 7


_FIXTURES = (
    Path(__file__).resolve().parents[1] / "engine/server/tests/fixtures/economics"
)

# The fixtures live with the engine's tests. The core-alone CI job removes
# engine/, so the parity tests skip there, as other engine-dependent tests do.
_needs_fixtures = pytest.mark.skipif(
    not _FIXTURES.is_dir(), reason="needs engine/ (absent in the core-alone job)"
)


@_needs_fixtures
def test_shared_fixtures_match_the_engine_port() -> None:
    """The numbers the Rust port asserts against the same files, literally."""

    def reading(name: str) -> dict[str, object]:
        lines = (_FIXTURES / f"{name}.jsonl").read_text().splitlines()
        result = meter(lines, agent=name)
        return {
            "turns": result.turns,
            "context_tokens": result.context_tokens,
            "peak_context_tokens": result.peak_context_tokens,
            "compactions": result.compactions,
            "tool_result_bytes": dict(result.tool_result_bytes),
            "reads_repeated": result.reads_repeated,
            "wall_clock": {
                "count": result.wall_clock.count,
                "max_ms": result.wall_clock.max_ms,
                "mean_ms": result.wall_clock.mean_ms,
            },
            "output_tokens": result.output_tokens,
            "usage_measured": result.usage_measured,
            "context_window_tokens": result.context_window_tokens,
        }

    assert reading("claude") == {
        "turns": 2,
        "context_tokens": 80,
        "peak_context_tokens": 175,
        "compactions": 1,
        "tool_result_bytes": {"Bash": 40, "Read": 16},
        "reads_repeated": 1,
        "wall_clock": {"count": 1, "max_ms": 30_000, "mean_ms": 30_000},
        "output_tokens": 16,
        "usage_measured": True,
        "context_window_tokens": 0,
    }
    assert reading("klaudia") == {
        "turns": 2,
        "context_tokens": 67,
        "peak_context_tokens": 117,
        "compactions": 1,
        "tool_result_bytes": {"Read": 16, "shell": 6},
        "reads_repeated": 1,
        "wall_clock": {"count": 1, "max_ms": 60_000, "mean_ms": 60_000},
        "output_tokens": 0,
        "usage_measured": False,
        "context_window_tokens": 0,
    }
    assert reading("codex") == {
        "turns": 3,
        "context_tokens": 625,
        "peak_context_tokens": 940,
        "compactions": 1,
        "tool_result_bytes": {"cat": 10, "grep": 9, "shell": 11},
        "reads_repeated": 1,
        "wall_clock": {"count": 2, "max_ms": 45_000, "mean_ms": 22_500},
        "output_tokens": 65,
        "usage_measured": True,
        "context_window_tokens": 272_000,
    }


@_needs_fixtures
def test_shared_unicode_fixture_counts_characters_as_python_does() -> None:
    """Non-ASCII text, a tool result with no content (the json.dumps length)
    and a half-millisecond mean: the cases a byte count, compact JSON or
    half-away-from-zero rounding in the engine port would get wrong."""
    lines = (_FIXTURES / "klaudia_unicode.jsonl").read_text().splitlines()
    result = meter(lines, agent="klaudia")
    assert result.turns == 3
    assert result.context_tokens == 127
    assert dict(result.tool_result_bytes) == {"Grep": 90, "Read": 10}
    assert result.wall_clock.count == 2
    assert result.wall_clock.max_ms == 3
    assert result.wall_clock.mean_ms == 2


@_needs_fixtures
def test_shared_gap_fixture_counts_milliseconds_exactly() -> None:
    """A 1.001 s gap is 1001 ms (float seconds * 1000 said 1000), and a DEL
    in a tool input is a six-character escape in json.dumps."""
    lines = (_FIXTURES / "claude_gap.jsonl").read_text().splitlines()
    result = meter(lines, agent="claude")
    assert result.wall_clock.max_ms == 1001
    assert result.wall_clock.mean_ms == 1001
    assert dict(result.tool_result_bytes) == {"Grep": 71}


def test_codex_without_token_count_is_estimated() -> None:
    """No token_count yet: the 4 chars/token estimate, not measured."""
    line = json.dumps({"type": "turn_context", "payload": {"content": "x" * 40}})
    result = meter([line], agent="codex")
    assert result.usage_measured is False
    assert result.context_tokens > 0
    assert result.context_window_tokens == 0


def _compact() -> str:
    return _line(
        {"type": "user", "isCompactSummary": True, "message": {"content": "s"}}
    )


def _calls(n: int, tag: str = "m") -> str:
    blocks = [{"type": "tool_use", "id": f"{tag}{i}", "name": "Bash"} for i in range(n)]
    return _line({"type": "assistant", "message": {"id": tag, "content": blocks}})


def test_compactions_with_at_most_one_call_between_are_a_streak() -> None:
    lines = [_calls(1), _compact(), _calls(1, "b"), _compact(), _compact()]
    result = meter(lines, agent="claude")
    assert result.compactions == 3
    assert result.compaction_streak == 3


def test_two_tool_calls_after_a_compaction_end_the_streak() -> None:
    lines = [_compact(), _compact(), _calls(2), _compact()]
    assert meter(lines, agent="claude").compaction_streak == 1
    # Calls split across entries count together.
    split = [_compact(), _compact(), _calls(1), _calls(1, "b")]
    assert meter(split, agent="claude").compaction_streak == 0


def test_an_assistant_turn_ends_the_streak() -> None:
    reply = _line(
        {
            "type": "assistant",
            "message": {"id": "r", "content": [{"type": "text", "text": "hi"}]},
        }
    )
    assert meter([_compact(), _compact(), reply], agent="claude").compaction_streak == 0
    assert meter([_compact(), _compact()], agent="claude").compaction_streak == 2


def test_codex_response_item_calls_end_the_streak() -> None:
    def call() -> str:
        return _line({"type": "response_item", "payload": {"type": "function_call"}})

    lines = [_line({"type": "compacted"}), call(), _line({"type": "compacted"})]
    assert meter(lines, agent="codex").compaction_streak == 2
    lines = [_line({"type": "compacted"}), call(), call()]
    assert meter(lines, agent="codex").compaction_streak == 0


@_needs_fixtures
def test_shared_loop_fixture_matches_the_engine_port() -> None:
    lines = (_FIXTURES / "claude_loop.jsonl").read_text().splitlines()
    result = meter(lines, agent="claude")
    assert (result.compactions, result.compaction_streak) == (3, 3)
