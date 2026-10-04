"""`agent-activity` — what agents did, read from their own transcripts.

Claude Code and Codex each keep a JSONL log per conversation: one JSON object
per line, tool calls in assistant entries and their results in the entries
that follow. This collector reads those logs incrementally and reduces each
call to an index row — when, which conversation, which directory, which tool,
a one-line redacted summary, service tags, and whether it failed. The rules
for that reduction (redaction, excerpts, heuristics) are `core/agent_activity`.

Three things set it apart from the project collectors, and are why it has its
own read path rather than returning findings.

**Its scope is the configured transcript roots, not the project list.**
Transcripts are not repositories and are not discovered: an operator names
each root (`agent_activity_roots`), and with none named the collector is not
registered at all, so coverage says "never run" rather than "found nothing".
A project is connected to activity at read time, by its root containing the
call's working directory.

**It is incremental by byte offset.** Each file's cursor records how far it
has been read, always at a line boundary; a line still being written is left
for the next sweep. The sweeper stores the calls and moves the cursors in one
transaction, so nothing is indexed twice and nothing is skipped on a crash.

**Every sweep is bounded.** At most `agent_activity_max_bytes_per_sweep` is
read per sweep, newest files first, so a first sweep over months of
transcripts catches up over several sweeps instead of stalling the schedule;
the backlog still waiting is reported in the sweep's stats.

Transcript content is untrusted data. Nothing in it is interpreted beyond
the JSON structure the two agents document by example.
"""

from __future__ import annotations

import json
import os
import re
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import BinaryIO

from vogt.collectors.base import CollectorContext, Finding
from vogt.core.agent_activity import (
    ServiceMatcher,
    dumps_secrets,
    is_error,
    result_excerpt,
    summarize_input,
)
from vogt.core.clock import from_iso
from vogt.core.entities import Project
from vogt.storage.observed_types import (
    ActivityBatch,
    ActivityCall,
    ActivityResult,
    TranscriptCursor,
)

COLLECTOR_NAME = "agent-activity"

#: The transcript formats this collector reads, by the key an operator uses
#: in `agent_activity_roots`.
AGENTS = ("claude", "codex")

#: A single line longer than this is skipped rather than parsed: it is a
#: multi-megabyte tool output, and the call it answers is still indexed.
MAX_LINE_BYTES = 16 * 1024 * 1024

#: Files considered per root per sweep. A guard against a root pointed at the
#: wrong directory, not a tuning knob.
MAX_FILES_PER_ROOT = 50_000

_UUID_TAIL = re.compile(
    r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$", re.I
)
#: Codex's `exec` tool takes a script; the shell command inside it is the
#: part worth summarising.
_CODEX_CMD = re.compile(
    r"""\bcmd\s*:\s*(?:"((?:[^"\\]|\\.)*)"|'((?:[^'\\]|\\.)*)'|`([^`]*)`)"""
)


@dataclass
class _Pending:
    """A call seen in this batch, held until its result arrives."""

    tool: str
    withheld: bool


@dataclass
class _FileState:
    """What one file read accumulates."""

    path: str
    agent: str
    agent_session_id: str | None
    cwd: str | None
    calls: list[ActivityCall] = field(default_factory=list)
    results: list[ActivityResult] = field(default_factory=list)
    pending: dict[str, _Pending] = field(default_factory=dict)


class AgentActivityCollector:
    """Indexes agent transcripts under the configured roots."""

    #: Not run per project: the sweeper calls `scan` once per sweep.
    project_scoped = False

    def __init__(
        self,
        roots: Mapping[str, Path],
        *,
        budget_bytes: int,
        services: Mapping[str, str] | None = None,
    ) -> None:
        self._roots = {agent: Path(path).expanduser() for agent, path in roots.items()}
        self._budget = budget_bytes
        self._services = ServiceMatcher.build(services)

    @property
    def name(self) -> str:
        return COLLECTOR_NAME

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        """Nothing per project; see `scan`."""
        del ctx, project
        return ()

    # -- the sweep ---------------------------------------------------------

    def scan(self, cursors: Mapping[str, TranscriptCursor]) -> ActivityBatch:
        """Read up to the byte budget of unread transcript, newest files first."""
        skipped: dict[str, str] = {}
        candidates: list[tuple[float, str, Path, int]] = []
        for agent, root in sorted(self._roots.items()):
            if agent not in AGENTS:
                skipped[str(root)] = f"unknown transcript format {agent!r}"
                continue
            if not root.is_dir():
                skipped[str(root)] = "not a directory"
                continue
            for path in _transcripts(root):
                try:
                    stat = path.stat()
                except OSError:
                    continue
                cursor = cursors.get(str(path))
                if cursor is not None and cursor.offset == stat.st_size:
                    continue
                candidates.append((stat.st_mtime, agent, path, stat.st_size))

        candidates.sort(key=lambda item: (-item[0], str(item[2])))
        remaining = self._budget
        batch_calls: list[ActivityCall] = []
        batch_results: list[ActivityResult] = []
        new_cursors: list[TranscriptCursor] = []
        backlog = 0
        files = 0
        for _mtime, agent, path, size in candidates:
            cursor = cursors.get(str(path))
            offset = 0 if cursor is None or cursor.offset > size else cursor.offset
            if remaining <= 0:
                backlog += size - offset
                continue
            state = _FileState(
                path=str(path),
                agent=agent,
                # A file that shrank was replaced: start it over, and forget
                # what the old one said about itself.
                agent_session_id=(
                    None
                    if cursor is None or cursor.offset > size
                    else cursor.agent_session_id
                ),
                cwd=None if cursor is None or cursor.offset > size else cursor.cwd,
            )
            try:
                end = self._read(state, path, offset, remaining)
            except OSError as exc:
                skipped[str(path)] = f"{type(exc).__name__}: {exc}"
                continue
            files += 1
            consumed = end - offset
            remaining -= consumed
            backlog += size - end
            batch_calls.extend(state.calls)
            batch_results.extend(state.results)
            if end != offset or cursor is None or cursor.offset > size:
                new_cursors.append(
                    TranscriptCursor(
                        path=str(path),
                        agent=agent,
                        offset=end,
                        size=size,
                        agent_session_id=state.agent_session_id,
                        cwd=state.cwd,
                    )
                )
        return ActivityBatch(
            calls=batch_calls,
            results=batch_results,
            cursors=new_cursors,
            files=files,
            bytes_read=self._budget - remaining,
            backlog_bytes=max(backlog, 0),
            skipped=skipped,
        )

    def _read(self, state: _FileState, path: Path, offset: int, budget: int) -> int:
        """Parse complete lines from `offset`; return the offset reached."""
        position = offset
        with path.open("rb") as handle:
            handle.seek(offset)
            while position - offset < budget:
                line = handle.readline(MAX_LINE_BYTES + 1)
                if not line:
                    break
                if not line.endswith(b"\n"):
                    if len(line) <= MAX_LINE_BYTES:
                        # Still being written: leave it for the next sweep.
                        break
                    # Too long to parse: skip to the end of it.
                    skipped = _skip_line(handle)
                    if skipped is None:
                        break
                    position += len(line) + skipped
                    continue
                position += len(line)
                self._parse_line(state, line)
        return position

    def _parse_line(self, state: _FileState, raw: bytes) -> None:
        try:
            entry = json.loads(raw)
        except ValueError:
            return
        if not isinstance(entry, dict):
            return
        if state.agent == "claude":
            self._claude(state, entry)
        else:
            self._codex(state, entry)

    # -- Claude Code -------------------------------------------------------

    def _claude(self, state: _FileState, entry: dict[str, object]) -> None:
        session = entry.get("sessionId")
        if isinstance(session, str) and session:
            state.agent_session_id = session
        elif state.agent_session_id is None:
            state.agent_session_id = Path(state.path).stem
        cwd = entry.get("cwd")
        if isinstance(cwd, str) and cwd:
            state.cwd = cwd
        message = entry.get("message")
        if not isinstance(message, dict):
            return
        content = message.get("content")
        if not isinstance(content, list):
            return
        at = _timestamp(entry.get("timestamp"))
        for block in content:
            if not isinstance(block, dict):
                continue
            kind = block.get("type")
            if kind == "tool_use" and at is not None:
                call_id = block.get("id")
                tool = block.get("name")
                if isinstance(call_id, str) and isinstance(tool, str):
                    self._call(state, call_id, tool, block.get("input"), at)
            elif kind == "tool_result":
                call_id = block.get("tool_use_id")
                if isinstance(call_id, str):
                    flagged = block.get("is_error")
                    self._result(
                        state,
                        call_id,
                        _text_of(block.get("content")),
                        flagged=flagged if isinstance(flagged, bool) else None,
                        at=at,
                    )

    # -- Codex -------------------------------------------------------------

    def _codex(self, state: _FileState, entry: dict[str, object]) -> None:
        payload = entry.get("payload")
        if not isinstance(payload, dict):
            return
        kind = entry.get("type")
        if kind == "session_meta":
            session = payload.get("id") or payload.get("session_id")
            if isinstance(session, str) and session:
                state.agent_session_id = session
            cwd = payload.get("cwd")
            if isinstance(cwd, str) and cwd:
                state.cwd = cwd
            return
        if kind == "turn_context":
            cwd = payload.get("cwd")
            if isinstance(cwd, str) and cwd:
                state.cwd = cwd
            return
        if kind != "response_item":
            return
        if state.agent_session_id is None:
            match = _UUID_TAIL.search(Path(state.path).stem)
            state.agent_session_id = match.group(1) if match else Path(state.path).stem
        at = _timestamp(entry.get("timestamp"))
        item = payload.get("type")
        call_id = payload.get("call_id")
        if not isinstance(item, str) or not isinstance(call_id, str):
            return
        if item.endswith("_call_output"):
            self._result(
                state, call_id, _text_of(payload.get("output")), flagged=None, at=at
            )
            return
        if at is None:
            return
        if item == "function_call":
            name = payload.get("name")
            arguments = payload.get("arguments")
            call_input: object = arguments
            if isinstance(arguments, str):
                try:
                    call_input = json.loads(arguments)
                except ValueError:
                    call_input = arguments
            if isinstance(name, str):
                self._call(state, call_id, name, call_input, at)
        elif item == "custom_tool_call":
            name = payload.get("name")
            script = payload.get("input")
            if isinstance(name, str):
                if isinstance(script, str):
                    commands = [
                        next(group for group in match.groups() if group is not None)
                        for match in _CODEX_CMD.finditer(script)
                    ]
                    if commands:
                        self._call(
                            state,
                            call_id,
                            f"{name}_command",
                            {"command": " ; ".join(commands)},
                            at,
                            raw=script,
                        )
                        return
                self._call(state, call_id, name, script, at)
        elif item == "local_shell_call":
            action = payload.get("action")
            command = action.get("command") if isinstance(action, dict) else None
            self._call(state, call_id, "shell", {"command": command}, at)

    # -- shared ------------------------------------------------------------

    def _call(
        self,
        state: _FileState,
        call_id: str,
        tool: str,
        call_input: object,
        at: datetime,
        *,
        raw: object = None,
    ) -> None:
        source = call_input if raw is None else raw
        flat = (
            source
            if isinstance(source, str)
            else json.dumps(source, sort_keys=True, default=str)
        )
        withheld = dumps_secrets(flat)
        state.pending[call_id] = _Pending(tool=tool, withheld=withheld)
        state.calls.append(
            ActivityCall(
                source_path=state.path,
                call_id=call_id,
                agent=state.agent,
                agent_session_id=state.agent_session_id or Path(state.path).stem,
                cwd=state.cwd,
                tool=tool,
                summary=summarize_input(tool, call_input),
                services=self._services.tags(tool, source),
                withheld=withheld,
                at=at,
            )
        )

    def _result(
        self,
        state: _FileState,
        call_id: str,
        output: str,
        *,
        flagged: bool | None,
        at: datetime | None,
    ) -> None:
        pending = state.pending.pop(call_id, None)
        tool = "" if pending is None else pending.tool
        error = is_error(output, flagged=flagged, tool=tool)
        state.results.append(
            ActivityResult(
                source_path=state.path,
                call_id=call_id,
                error=error,
                excerpt=result_excerpt(
                    output,
                    error=error,
                    withheld=pending is not None and pending.withheld,
                ),
                at=at,
            )
        )


def _transcripts(root: Path) -> Iterator[Path]:
    """Every `*.jsonl` under `root`, without following symlinked directories."""
    seen = 0
    for directory, subdirs, names in os.walk(root, followlinks=False):
        subdirs.sort()
        for name in sorted(names):
            if not name.endswith(".jsonl"):
                continue
            seen += 1
            if seen > MAX_FILES_PER_ROOT:
                return
            yield Path(directory) / name


def _skip_line(handle: BinaryIO) -> int | None:
    """Read past the rest of an over-long line; `None` if it never ends."""
    skipped = 0
    while True:
        chunk = handle.readline(MAX_LINE_BYTES)
        if not chunk:
            return None
        skipped += len(chunk)
        if chunk.endswith(b"\n"):
            return skipped


def _timestamp(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return from_iso(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _text_of(content: object) -> str:
    """A result's text, from the string or block-list shapes both agents use."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = [
            str(block.get("text", ""))
            for block in content
            if isinstance(block, dict) and block.get("type") in ("text", "input_text")
        ]
        return "\n".join(parts)
    if content is None:
        return ""
    return json.dumps(content, default=str)[:65_536]
