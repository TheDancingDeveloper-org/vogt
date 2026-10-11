"""C16 session economics: the pure transcript reading."""

from __future__ import annotations

import json

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
        _line({"type": "compact_boundary"}),
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
