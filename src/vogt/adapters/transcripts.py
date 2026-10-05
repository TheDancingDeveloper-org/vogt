"""A session's own agent conversation, read from the agent's transcript.

A driver reading another agent's terminal gets the visible screen
(`session.screen`) or the raw output log, where replies are cut off,
scrolled away or mangled by redraws. Claude Code and Codex already write
every reply, whole, to a JSONL transcript per conversation. This module
finds the transcript a session's conversation is in and reads its last
assistant messages back (WI-872), and keeps a cheap one-line excerpt of the
latest one for session lists (WI-876).

**Finding the conversation.** In order, with the basis reported so a caller
can weigh a guess:

1. an id the session's command names — `--session-id <id>` (the engine pins
   a fresh Claude Code launch to the engine session id), `--resume <id>`,
   `codex resume <id>`, `--session <id>`;
2. the engine session id itself (a Claude Code session the engine started);
3. only for an explicit read, the newest transcript in the session's
   directory written since the session started (`cwd`). Two agents in the
   same directory make this a guess, and it says so.

**Reading.** Only the tail of one file is read, bounded, from the end; every
message is redacted with the agent-activity redactor before it leaves here.
Transcript content is untrusted data and nothing in it is interpreted beyond
the JSON shape the two agents write.
"""

from __future__ import annotations

import json
import os
import re
import threading
from collections.abc import Iterable, Iterator, Mapping
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path

from vogt.core.agent_activity import redact

#: The most of a transcript's tail read looking for replies.
TAIL_BYTES = 4 * 1024 * 1024
#: How much of a transcript's end is read for the model it runs.
RUNTIME_TAIL_BYTES = 256 * 1024
#: One message is cut here; a reply longer than this is rare and the head is
#: what a driver needs.
MAX_MESSAGE_CHARS = 20_000
#: Excerpts for session lists.
EXCERPT_CHARS = 300
#: Directory entries considered per root, as a guard against a wrong root.
MAX_ENTRIES = 20_000

_ID = re.compile(r"^[A-Za-z0-9_.][A-Za-z0-9_.-]{0,127}$")
_FLAG_IDS = re.compile(r"(?:--session-id|--resume|--session)[= ]+'?([A-Za-z0-9_.-]+)")
_UUID_TAIL = re.compile(
    r"([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$", re.I
)
_CODEX_RESUME = re.compile(r"\bcodex\s+resume\s+'?([A-Za-z0-9_.-]+)")


@dataclass(frozen=True)
class Transcript:
    """Where a session's conversation is written."""

    agent: str
    path: Path
    conversation_id: str
    #: `session-id`, `resume-id`, `engine-id` or `cwd` (the newest
    #: transcript in the session's directory since it started — a guess).
    basis: str


@dataclass(frozen=True)
class Reply:
    """One assistant message, redacted."""

    text: str
    at: datetime | None


def agent_of(command: str | None, template: str | None) -> tuple[str, ...]:
    """Which transcript formats to look in, most likely first."""
    hint = f"{command or ''} {template or ''}".lower()
    if "codex" in hint:
        return ("codex",)
    if "claude" in hint:
        return ("claude",)
    return ("claude", "codex")


def named_ids(command: str | None) -> list[tuple[str, str]]:
    """Conversation ids the command line names, with what named them."""
    if not command:
        return []
    found: list[tuple[str, str]] = []
    for match in _FLAG_IDS.finditer(command):
        flag = match.group(0).split()[0].split("=")[0]
        basis = "session-id" if flag == "--session-id" else "resume-id"
        found.append((match.group(1), basis))
    for match in _CODEX_RESUME.finditer(command):
        found.append((match.group(1), "resume-id"))
    return [(i, b) for i, b in found if _ID.match(i)]


def claude_key(cwd: str) -> str:
    """The directory name Claude Code files a conversation under for `cwd`."""
    return re.sub(r"[^A-Za-z0-9-]", "-", cwd)


def find(
    roots: Mapping[str, Path],
    *,
    engine_session_id: str,
    command: str | None,
    template: str | None,
    cwd: str | None,
    started_at: datetime | None,
    allow_cwd_guess: bool,
) -> Transcript | None:
    """The transcript a session's conversation is in, or `None`."""
    agents = agent_of(command, template)
    candidates = named_ids(command)
    if _ID.match(engine_session_id):
        candidates.append((engine_session_id, "engine-id"))
    for agent in agents:
        root = roots.get(agent)
        if root is None:
            continue
        root = Path(root).expanduser()
        if not root.is_dir():
            continue
        for conversation_id, basis in candidates:
            path = _by_id(agent, root, conversation_id)
            if path is not None:
                return Transcript(agent, path, conversation_id, basis)
        if allow_cwd_guess and cwd:
            guessed = _by_cwd(agent, root, cwd, started_at)
            if guessed is not None:
                return guessed
    return None


def _by_id(agent: str, root: Path, conversation_id: str) -> Path | None:
    if agent == "claude":
        name = f"{conversation_id}.jsonl"
        for entry in _entries(root):
            candidate = entry / name
            if candidate.is_file():
                return candidate
        return None
    suffix = f"-{conversation_id}.jsonl"
    for path in _codex_files(root):
        if path.name.endswith(suffix):
            return path
    return None


def _by_cwd(
    agent: str, root: Path, cwd: str, started_at: datetime | None
) -> Transcript | None:
    since = started_at.timestamp() if started_at is not None else 0.0
    if agent == "claude":
        directory = root / claude_key(cwd)
        if not directory.is_dir():
            return None
        files = [p for p in directory.glob("*.jsonl") if _mtime(p) >= since]
    else:
        files = [
            p for p in _codex_files(root) if _mtime(p) >= since and _codex_cwd(p) == cwd
        ]
    if not files:
        return None
    newest = max(files, key=_mtime)
    conversation = newest.stem
    if agent == "codex":
        match = _UUID_TAIL.search(newest.stem)
        conversation = match.group(1) if match else newest.stem
    return Transcript(agent, newest, conversation, "cwd")


def _entries(root: Path) -> Iterator[Path]:
    try:
        with os.scandir(root) as it:
            for count, entry in enumerate(it):
                if count >= MAX_ENTRIES:
                    return
                if entry.is_dir(follow_symlinks=False):
                    yield Path(entry.path)
    except OSError:
        return


def _codex_files(root: Path) -> Iterator[Path]:
    """Rollouts under `root/YYYY/MM/DD/`, newest dates first."""
    seen = 0
    for year in sorted(_entries(root), reverse=True):
        for month in sorted(_entries(year), reverse=True):
            for day in sorted(_entries(month), reverse=True):
                try:
                    names = sorted((p.name for p in day.iterdir()), reverse=True)
                except OSError:
                    continue
                for name in names:
                    seen += 1
                    if seen > MAX_ENTRIES:
                        return
                    if name.endswith(".jsonl"):
                        yield day / name


def _codex_cwd(path: Path) -> str | None:
    try:
        with path.open("rb") as handle:
            first = handle.readline(256 * 1024)
        entry = json.loads(first)
    except (OSError, ValueError):
        return None
    payload = entry.get("payload") if isinstance(entry, dict) else None
    cwd = payload.get("cwd") if isinstance(payload, dict) else None
    return cwd if isinstance(cwd, str) else None


def _mtime(path: Path) -> float:
    try:
        return path.stat().st_mtime
    except OSError:
        return 0.0


# -- reading ---------------------------------------------------------------


def last_replies(transcript: Transcript, n: int) -> list[Reply]:
    """The last `n` assistant messages, oldest first, redacted."""
    lines = _tail_lines(transcript.path, TAIL_BYTES)
    if transcript.agent == "claude":
        messages = _claude_messages(lines)
    else:
        messages = _codex_messages(lines)
    picked = messages[-n:] if n > 0 else []
    return [
        Reply(text=_cut(redact(text), MAX_MESSAGE_CHARS), at=at) for text, at in picked
    ]


def _tail_lines(path: Path, limit: int) -> list[bytes]:
    try:
        size = path.stat().st_size
        with path.open("rb") as handle:
            start = max(0, size - limit)
            handle.seek(start)
            data = handle.read(limit)
    except OSError:
        return []
    lines = data.split(b"\n")
    if start > 0 and lines:
        lines = lines[1:]  # the first line is cut mid-way
    return [line for line in lines if line.strip()]


def _parsed(lines: Iterable[bytes]) -> Iterator[dict[str, object]]:
    for raw in lines:
        try:
            entry = json.loads(raw)
        except ValueError:
            continue
        if isinstance(entry, dict):
            yield entry


def _claude_messages(lines: list[bytes]) -> list[tuple[str, datetime | None]]:
    """Assistant text, one item per API message (its streamed entries joined)."""
    order: list[str] = []
    texts: dict[str, list[str]] = {}
    times: dict[str, datetime | None] = {}
    for index, entry in enumerate(_parsed(lines)):
        if entry.get("type") != "assistant" or entry.get("isSidechain") is True:
            continue
        message = entry.get("message")
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if not isinstance(content, list):
            continue
        parts = [
            str(block.get("text", ""))
            for block in content
            if isinstance(block, dict) and block.get("type") == "text"
        ]
        parts = [p for p in parts if p.strip()]
        if not parts:
            continue
        key = str(message.get("id") or f"line-{index}")
        if key not in texts:
            order.append(key)
            texts[key] = []
            times[key] = _when(entry.get("timestamp"))
        texts[key].extend(parts)
    return [("\n\n".join(texts[key]), times[key]) for key in order]


def _codex_messages(lines: list[bytes]) -> list[tuple[str, datetime | None]]:
    found: list[tuple[str, datetime | None]] = []
    for entry in _parsed(lines):
        if entry.get("type") != "response_item":
            continue
        payload = entry.get("payload")
        if not isinstance(payload, dict):
            continue
        if payload.get("type") != "message" or payload.get("role") != "assistant":
            continue
        content = payload.get("content")
        if not isinstance(content, list):
            continue
        text = "\n".join(
            str(block.get("text", ""))
            for block in content
            if isinstance(block, dict) and block.get("type") in ("output_text", "text")
        ).strip()
        if text:
            found.append((text, _when(entry.get("timestamp"))))
    return found


def _when(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


def _cut(text: str, limit: int) -> str:
    return text if len(text) <= limit else text[: limit - 1] + "…"


def reply(text: str, at: datetime | None) -> Reply:
    """A reply from somewhere other than a transcript file (opencode's store,
    read by the engine), redacted and cut the same way."""
    return Reply(text=_cut(redact(text), MAX_MESSAGE_CHARS), at=at)


def excerpt(text: str) -> str:
    """The one-line list excerpt of a reply's (redacted) text."""
    return _cut(" ".join(text.split()), EXCERPT_CHARS)


# -- excerpts for lists ----------------------------------------------------

_EXCERPTS: dict[tuple[str, int, int], str | None] = {}
_EXCERPTS_LOCK = threading.Lock()
_EXCERPTS_MAX = 512


def last_reply_excerpt(transcript: Transcript) -> str | None:
    """A redacted one-line excerpt of the latest reply, cached by the file's
    size and modification time so a list re-reads only what changed."""
    try:
        stat = transcript.path.stat()
    except OSError:
        return None
    key = (str(transcript.path), stat.st_mtime_ns, stat.st_size)
    with _EXCERPTS_LOCK:
        if key in _EXCERPTS:
            return _EXCERPTS[key]
    replies = last_replies(transcript, 1)
    excerpt = (
        _cut(" ".join(replies[-1].text.split()), EXCERPT_CHARS) if replies else None
    )
    with _EXCERPTS_LOCK:
        if len(_EXCERPTS) >= _EXCERPTS_MAX:
            _EXCERPTS.clear()
        _EXCERPTS[key] = excerpt
    return excerpt


# -- the model a conversation is running ------------------------------------

_RUNTIMES: dict[tuple[str, int, int], tuple[str | None, str | None]] = {}
_RUNTIMES_MAX = 512


def runtime(transcript: Transcript) -> tuple[str | None, str | None]:
    """The model (and, for Codex, the reasoning effort) the conversation's
    latest turn ran on, as the agent CLI recorded it — the resolved value,
    whatever the session was started with. Claude Code writes `model` on
    every assistant message; Codex writes `model` and `effort` in each
    `turn_context`. Cached like the excerpt."""
    try:
        stat = transcript.path.stat()
    except OSError:
        return (None, None)
    key = (str(transcript.path), stat.st_mtime_ns, stat.st_size)
    with _EXCERPTS_LOCK:
        if key in _RUNTIMES:
            return _RUNTIMES[key]
    # The model is on every assistant message (every Codex turn), so a
    # short tail holds the latest one.
    lines = _tail_lines(transcript.path, RUNTIME_TAIL_BYTES)
    model: str | None = None
    effort: str | None = None
    for entry in _parsed(lines):
        if transcript.agent == "claude":
            message = entry.get("message")
            if entry.get("type") == "assistant" and isinstance(message, dict):
                found = message.get("model")
                # Claude Code writes `<synthetic>` on messages it made up
                # itself (an interruption notice); that is no model.
                if isinstance(found, str) and found and not found.startswith("<"):
                    model = found
        elif entry.get("type") == "turn_context":
            payload = entry.get("payload")
            if isinstance(payload, dict):
                if isinstance(payload.get("model"), str):
                    model = str(payload["model"])
                for name in ("effort", "reasoning_effort", "model_reasoning_effort"):
                    if isinstance(payload.get(name), str):
                        effort = str(payload[name])
    found_runtime = (model, effort)
    with _EXCERPTS_LOCK:
        if len(_RUNTIMES) >= _RUNTIMES_MAX:
            _RUNTIMES.clear()
        _RUNTIMES[key] = found_runtime
    return found_runtime


def clear_excerpt_cache() -> None:
    """Forget every cached excerpt (tests)."""
    with _EXCERPTS_LOCK:
        _EXCERPTS.clear()
        _RUNTIMES.clear()


def excerpt_cache_size() -> int:
    with _EXCERPTS_LOCK:
        return len(_EXCERPTS)


__all__ = [
    "Reply",
    "Transcript",
    "agent_of",
    "claude_key",
    "clear_excerpt_cache",
    "excerpt_cache_size",
    "find",
    "last_replies",
    "last_reply_excerpt",
    "named_ids",
    "runtime",
]
