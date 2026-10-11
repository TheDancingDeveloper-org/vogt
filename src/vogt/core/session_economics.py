"""What a session cost, read from the transcript the agent already wrote.

C16 (docs/design/context-management.md). The engine and the core both need
the same answer — turns, how full the context window is, how often it was
compacted, which tools ate the bytes, which files were read twice, how long
turns took — and neither should invent its own reading of a JSONL line. This
module is that reading. It takes text and returns numbers. It opens no file,
asks no clock, and stores nothing.

Three agents, three shapes, one result:

- **Claude Code** records `message.usage` on every assistant entry. The sum
  of its input, cache-read and cache-creation tokens is the live context
  size at that turn; `output_tokens` is what the turn added. Compaction is an
  entry with `isCompactSummary: true`.
- **Klaudia** writes the same envelope but no usage. It marks a compaction
  with a `compact_boundary` line. Context is estimated at 4 chars per token,
  the figure its own compaction doc uses.
- **Codex** writes `turn_context` entries and carries tool results under
  `payload`. Same 4 chars/token estimate; no compaction marker is read.

`context_tokens` is the latest turn's context, not a sum across turns —
that sum would count the same history once per turn. `peak_context_tokens`
is the largest such reading. `turns` counts distinct assistant `message.id`
values, because Claude writes one assistant entry per content block of a
single turn. A user message or a tool result is not a turn. An estimate
counts characters since the latest compaction only: that is the live
history, not the archived one.

Tool-result bytes and repeated reads count what the model was shown, which
is the thing that fills the window. A Read of the same path twice counts
once toward `reads_repeated`. Wall clock is the gap between consecutive
assistant entries that both carry a timestamp; a transcript with one turn
has no gap and reports none.
"""

from __future__ import annotations

import json
from collections.abc import Iterable, Mapping
from dataclasses import dataclass, field
from datetime import timedelta
from itertools import pairwise

#: Klaudia's own figure (docs/compaction.md in msp-klaudia): four characters
#: of transcript per token when the agent records no usage.
CHARS_PER_TOKEN = 4

_MILLISECOND = timedelta(milliseconds=1)

#: Tool names whose input names a file. A repeated value is a repeated read.
_READ_TOOLS = frozenset({"Read", "read_file", "cat"})
_PATH_KEYS = ("file_path", "path", "filePath")


@dataclass(frozen=True)
class TurnTiming:
    """Per-turn wall clock, in milliseconds. Empty when fewer than two turns
    carried a timestamp, because one turn has no gap to measure."""

    count: int = 0
    max_ms: int = 0
    mean_ms: int = 0


@dataclass(frozen=True)
class SessionEconomics:
    """The economics of one session's transcript. Every field is present even
    when the transcript said nothing: zero and an empty map, not absence.
    Absence would read as "not measured"."""

    turns: int = 0
    context_tokens: int = 0
    peak_context_tokens: int = 0
    compactions: int = 0
    tool_result_bytes: Mapping[str, int] = field(default_factory=dict)
    reads_repeated: int = 0
    wall_clock: TurnTiming = field(default_factory=TurnTiming)
    output_tokens: int = 0
    #: True when context came from recorded usage, false when it was estimated.
    usage_measured: bool = False

    def budget_line(self, turn_budget: int | None, window_tokens: int | None) -> str:
        """The one line a digest row carries: `96/120 turns, 61% ctx, 2 compactions`.

        A missing budget or window is left out rather than shown as zero,
        because zero would say a budget exists and is exhausted.
        """
        parts: list[str] = []
        if turn_budget:
            parts.append(f"{self.turns}/{turn_budget} turns")
        else:
            parts.append(f"{self.turns} turns")
        if window_tokens:
            pct = min(100, round(100 * self.context_tokens / window_tokens))
            parts.append(f"{pct}% ctx")
        if self.compactions:
            noun = "compaction" if self.compactions == 1 else "compactions"
            parts.append(f"{self.compactions} {noun}")
        return ", ".join(parts)


def meter(lines: Iterable[str], *, agent: str) -> SessionEconomics:
    """Reduce one transcript's JSONL lines to its economics.

    A line that is not JSON is skipped, as is a line with no `type`. Callers
    pass the agent name (`claude`, `klaudia`, `codex`) so the reader knows
    which shape to expect; an unknown agent is read as Claude's shape, which
    is the one Klaudia also writes.
    """
    measured = agent == "claude"
    turn_ids: set[str] = set()
    unkeyed_turns = 0
    compactions = 0
    context = 0
    peak = 0
    #: Output tokens per turn. Claude repeats one turn's usage on each of its
    #: content-block entries, so the last reading per message id is the turn's.
    output_by_turn: dict[str, int] = {}
    unkeyed_output = 0
    live_chars = 0
    tools: dict[str, int] = {}
    reads: dict[str, int] = {}
    tool_names: dict[str, str] = {}
    stamps: list[str] = []

    for raw in lines:
        text = raw.strip()
        if not text:
            continue
        try:
            entry = json.loads(text)
        except json.JSONDecodeError:
            continue
        if not isinstance(entry, dict):
            continue
        kind = entry.get("type")

        if _is_compaction(entry):
            compactions += 1
            # The compacted history is no longer in the window.
            live_chars = 0
        live_chars += len(text)

        if kind == "assistant":
            message = entry.get("message")
            message_id = message.get("id") if isinstance(message, dict) else None
            if isinstance(message_id, str) and message_id:
                if message_id not in turn_ids:
                    stamp = entry.get("timestamp")
                    if isinstance(stamp, str) and stamp:
                        stamps.append(stamp)
                turn_ids.add(message_id)
            else:
                unkeyed_turns += 1
                stamp = entry.get("timestamp")
                if isinstance(stamp, str) and stamp:
                    stamps.append(stamp)
            _remember_tools(entry, tool_names)
            usage = _usage(entry)
            if usage is not None:
                turn_context = usage[0]
                if isinstance(message_id, str) and message_id:
                    output_by_turn[message_id] = usage[1]
                else:
                    unkeyed_output += usage[1]
                context = turn_context
                peak = max(peak, turn_context)
            elif not measured:
                context = live_chars // CHARS_PER_TOKEN
                peak = max(peak, context)
        elif not measured and kind in ("user", "tool_result", "turn_context"):
            context = live_chars // CHARS_PER_TOKEN
            peak = max(peak, context)

        _count_tools(entry, tools, reads, tool_names)

    if not measured and context == 0 and live_chars:
        context = live_chars // CHARS_PER_TOKEN
        peak = context

    return SessionEconomics(
        turns=len(turn_ids) + unkeyed_turns,
        context_tokens=context,
        peak_context_tokens=peak,
        compactions=compactions,
        tool_result_bytes=dict(sorted(tools.items())),
        reads_repeated=sum(1 for n in reads.values() if n > 1),
        wall_clock=_timing(stamps),
        output_tokens=sum(output_by_turn.values()) + unkeyed_output,
        usage_measured=measured and peak > 0,
    )


def _is_compaction(entry: Mapping[str, object]) -> bool:
    """Claude sets `isCompactSummary`; Klaudia writes a system line whose
    subtype is `compact_boundary`."""
    if entry.get("isCompactSummary") is True:
        return True
    if entry.get("subtype") == "compact_boundary":
        return True
    message = entry.get("message")
    return isinstance(message, dict) and message.get("isCompactSummary") is True


def _usage(entry: Mapping[str, object]) -> tuple[int, int] | None:
    """`(context_tokens, output_tokens)` from an assistant entry, or None.

    Context is the input the model was given this turn: fresh input plus
    whatever was read from or written to the cache. Summing those across
    turns would count history once per turn, so the caller keeps only the
    latest.
    """
    message = entry.get("message")
    if not isinstance(message, dict):
        return None
    usage = message.get("usage")
    if not isinstance(usage, dict):
        return None
    context = sum(
        int(usage.get(key) or 0)
        for key in (
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        )
    )
    output = int(usage.get("output_tokens") or 0)
    return context, output


def _remember_tools(entry: Mapping[str, object], tool_names: dict[str, str]) -> None:
    """Map a `tool_use` block's id to its tool name.

    A later `tool_result` carries only `tool_use_id`, so the name has to
    come from the call that produced it.
    """
    for block in _blocks(entry):
        if not isinstance(block, dict) or block.get("type") != "tool_use":
            continue
        tool_id = block.get("id")
        name = block.get("name")
        if isinstance(tool_id, str) and tool_id and isinstance(name, str) and name:
            tool_names[tool_id] = name


def _count_tools(
    entry: Mapping[str, object],
    tools: dict[str, int],
    reads: dict[str, int],
    tool_names: Mapping[str, str],
) -> None:
    """Add one entry's tool results to the byte map and the read counts.

    Claude and Klaudia nest results in `message.content`; Codex nests them
    in `payload`. A result names its tool on the block or, in the real
    Claude shape, only by `tool_use_id` pointing at an earlier `tool_use`.
    A result that names nothing is ignored rather than lumped under a
    made-up name.
    """
    for block in _blocks(entry):
        if not isinstance(block, dict):
            continue
        name = block.get("name") or block.get("tool_name") or block.get("tool")
        tool_id = block.get("tool_use_id")
        if not isinstance(name, str) or not name:
            name = tool_names.get(tool_id) if isinstance(tool_id, str) else None
        if not isinstance(name, str) or not name:
            continue
        size = _block_bytes(block)
        if block.get("type") in ("tool_result", "tool-result") or "content" in block:
            tools[name] = tools.get(name, 0) + size
        if name in _READ_TOOLS:
            path = _read_path(block.get("input"))
            if path:
                reads[path] = reads.get(path, 0) + 1


def _blocks(entry: Mapping[str, object]) -> list[object]:
    found: list[object] = []
    for container_key in ("message", "payload"):
        container = entry.get(container_key)
        if isinstance(container, dict):
            content = container.get("content")
            if isinstance(content, list):
                found.extend(content)
    content = entry.get("content")
    if isinstance(content, list):
        found.extend(content)
    return found


def _block_bytes(block: Mapping[str, object]) -> int:
    content = block.get("content")
    if isinstance(content, str):
        return len(content)
    if isinstance(content, list):
        total = 0
        for part in content:
            if isinstance(part, str):
                total += len(part)
            elif isinstance(part, dict) and isinstance(part.get("text"), str):
                total += len(part["text"])
        return total
    return len(json.dumps(block, default=str))


def _read_path(tool_input: object) -> str | None:
    if isinstance(tool_input, str) and tool_input:
        return tool_input
    if isinstance(tool_input, dict):
        for key in _PATH_KEYS:
            value = tool_input.get(key)
            if isinstance(value, str) and value:
                return value
    return None


def _timing(stamps: list[str]) -> TurnTiming:
    """Gaps between consecutive assistant timestamps, in milliseconds.

    Stamps that do not parse are dropped. Out-of-order stamps give a zero
    gap rather than a negative one: a transcript rewritten by a compaction
    can repeat an earlier time, and a negative turn length is not a length.
    """
    from datetime import datetime

    times: list[datetime] = []
    for stamp in stamps:
        try:
            times.append(datetime.fromisoformat(stamp.replace("Z", "+00:00")))
        except ValueError:
            continue
    if len(times) < 2:
        return TurnTiming()
    # Integer division of timedeltas is exact; total_seconds() * 1000 is a
    # float and comes out a millisecond low for some gaps (1.001 s -> 1000).
    gaps = [max(0, (b - a) // _MILLISECOND) for a, b in pairwise(times)]
    return TurnTiming(
        count=len(gaps), max_ms=max(gaps), mean_ms=round(sum(gaps) / len(gaps))
    )
