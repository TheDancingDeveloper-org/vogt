"""A small client for the session engine's API.

Built on `urllib` rather than a third-party HTTP library, for the reason the
GitHub adapter gives: this is an optional adapter making a handful of
requests, and the core must stay installable and fully functional without
it (NFR-PO1, NFR-PO3).

The token is read from a *file* and never from argv or a URL (FR-S7), and it
carries only the engine's `sessions` capability — Vogt starts and stops
terminals; it has no business writing that pod's files.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol

from vogt.errors import Conflict, InvalidRequest, NotFound, VogtError

USER_AGENT = "vogt"
DEFAULT_TIMEOUT_SECONDS = 20


class Transport(Protocol):
    """How this client actually talks, so tests never need an engine.

    Carries method and body as well as the URL, because what a test of
    `session.start` has to assert is the *spec that was sent* — the working
    directory above all (FR-E3). A transport that only saw the URL could not
    tell a session opened in the registry's tree from one opened in `$HOME`.
    """

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]: ...


class EngineUnavailable(VogtError):
    """The engine could not be reached, or refused the request.

    Never fatal to anything but the session operations: a Vogt with no
    reachable engine still answers every question it ever answered. The
    inverse — an engine with no core — is FR-E9, and is the engine's own
    concern.
    """

    code = "engine_unavailable"
    http_status = 502


@dataclass(frozen=True)
class EngineSession:
    """One terminal, as the engine describes it.

    A deliberately partial view: the engine's summary carries scrollback
    positions, continuity badges and more, none of which Vogt stores or
    reasons about. What Vogt needs is an identity, where it is running, and
    whether it is alive — the rest stays the engine's business, and reading
    only these four fields is what keeps that true.
    """

    id: str
    name: str
    activity: str
    cwd: str
    exit_code: int | None = None
    activity_changed_at: str | None = None

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineSession:
        return cls(
            id=str(payload.get("id", "")),
            name=str(payload.get("name", "")),
            activity=str(payload.get("activity", "unknown")),
            cwd=str(payload.get("cwd", "")),
            exit_code=payload.get("exit_code"),
            activity_changed_at=_optional_str(payload.get("activity_changed_at")),
        )


@dataclass(frozen=True)
class EngineArchivedSession:
    """One terminal that has ended, as the engine's history records it.

    The engine archives a session when its process exits: `created_at`,
    `ended_at` and the exit code, in its own SQLite. This is the only place
    a *duration* can be had — Vogt's `stopped_at` says when Vogt asked for a
    kill, which is a different fact and is usually a different moment (
    `SCHEMA.md` §2.6). Absent means the engine has no archive for that id,
    which is "not collected", never "it exited with no code".
    """

    id: str
    created_at: str
    ended_at: str | None = None
    exit_code: int | None = None

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineArchivedSession:
        return cls(
            id=str(payload.get("id", "")),
            created_at=str(payload.get("created_at", "")),
            ended_at=_optional_str(payload.get("ended_at")),
            exit_code=_optional_int(payload.get("exit_code")),
        )


@dataclass(frozen=True)
class EngineHistorySession:
    """One row of the engine's session-history listing (`SessionMetadata`).

    The fuller shape the history list returns — name and scrollback size on
    top of the `EngineArchivedSession` facts — so a caller browsing history
    can label a session without a second round trip. A live session that the
    history list includes has a NULL `ended_at`/`exit_code`, same as the GUI.
    """

    id: str
    name: str
    created_at: str
    ended_at: str | None = None
    exit_code: int | None = None
    cwd: str | None = None
    command: str | None = None
    scrollback_bytes: int = 0

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineHistorySession:
        raw_bytes = payload.get("scrollback_bytes")
        return cls(
            id=str(payload.get("id", "")),
            name=str(payload.get("name", "")),
            created_at=str(payload.get("created_at", "")),
            ended_at=_optional_str(payload.get("ended_at")),
            exit_code=_optional_int(payload.get("exit_code")),
            cwd=_optional_str(payload.get("cwd")),
            command=_optional_str(payload.get("command")),
            scrollback_bytes=raw_bytes if isinstance(raw_bytes, int) else 0,
        )


@dataclass(frozen=True)
class EngineHistoryMatch:
    """One hit from a session-output search (`SearchResult`).

    `live` distinguishes a match in a *running* session's scrollback (found by
    the engine's on-demand scan) from one in the archived FTS index. The
    snippet is plain text — terminal output is untrusted, so the engine never
    marks it up and neither does anything downstream.
    """

    session_id: str
    session_name: str
    created_at: str
    match_snippet: str
    rank: float = 0.0
    live: bool = False

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineHistoryMatch:
        raw_rank = payload.get("rank")
        return cls(
            session_id=str(payload.get("session_id", "")),
            session_name=str(payload.get("session_name", "")),
            created_at=str(payload.get("created_at", "")),
            match_snippet=str(payload.get("match_snippet", "")),
            rank=float(raw_rank) if isinstance(raw_rank, (int, float)) else 0.0,
            live=bool(payload.get("live", False)),
        )


@dataclass(frozen=True)
class EngineSessionLog:
    """The tail of a session's raw output log (`SessionLogPreview`).

    `text` is the rendered tail (ANSI-stripped when the caller asked for it);
    `bytes`/`total_bytes`/`truncated` describe the raw window that was read, so
    a caller knows whether it is looking at the whole run.
    """

    session_id: str
    text: str
    bytes: int = 0
    total_bytes: int = 0
    truncated: bool = False

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineSessionLog:
        raw_bytes = payload.get("bytes")
        raw_total = payload.get("total_bytes")
        return cls(
            session_id=str(payload.get("session_id", "")),
            text=str(payload.get("text", "")),
            bytes=raw_bytes if isinstance(raw_bytes, int) else 0,
            total_bytes=raw_total if isinstance(raw_total, int) else 0,
            truncated=bool(payload.get("truncated", False)),
        )


@dataclass(frozen=True)
class EngineTaskFinding:
    """Something a bound agent-task run reported about itself (FR-E7).

    Today there is exactly one producer: the notify-phrase watcher, which is
    the mechanism a task uses to say "I found something". It has always
    become a push notification; recording it on the run is what lets it also
    become evidence.
    """

    at: str
    text: str
    source: str = "notify-phrase"

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineTaskFinding:
        return cls(
            at=str(payload.get("at", "")),
            text=str(payload.get("text", "")),
            source=str(payload.get("source", "notify-phrase")),
        )


@dataclass(frozen=True)
class EngineTaskRun:
    """One execution of an agent task."""

    id: str
    session_id: str
    started_at: str
    status: str
    completed_at: str | None = None
    exit_code: int | None = None
    summary: str | None = None
    findings: tuple[EngineTaskFinding, ...] = ()

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineTaskRun:
        raw = payload.get("findings")
        findings = raw if isinstance(raw, list) else []
        return cls(
            id=str(payload.get("id", "")),
            session_id=str(payload.get("session_id", "")),
            started_at=str(payload.get("started_at", "")),
            status=str(payload.get("status", "running")),
            completed_at=_optional_str(payload.get("completed_at")),
            exit_code=_optional_int(payload.get("exit_code")),
            summary=_optional_str(payload.get("summary")),
            findings=tuple(
                EngineTaskFinding.from_payload(row)
                for row in findings
                if isinstance(row, dict)
            ),
        )


@dataclass(frozen=True)
class EngineAgentTask:
    """A scheduled agent task, and what Vogt subject it was bound to (FR-E7).

    The binding is carried as the names a person types — a project slug, a
    work-item ref — because the engine has no way to resolve a Vogt id and
    should not learn one. Resolution happens on this side, where the registry
    is.
    """

    id: str
    name: str
    cwd: str | None = None
    project: str | None = None
    work_item: str | None = None
    runs: tuple[EngineTaskRun, ...] = ()

    @property
    def is_bound(self) -> bool:
        return self.project is not None or self.work_item is not None

    @classmethod
    def from_payload(cls, payload: dict[str, Any]) -> EngineAgentTask:
        raw = payload.get("runs")
        runs = raw if isinstance(raw, list) else []
        return cls(
            id=str(payload.get("id", "")),
            name=str(payload.get("name", "")),
            cwd=_optional_str(payload.get("cwd")),
            project=_optional_str(payload.get("vogt_project")),
            work_item=_optional_str(payload.get("vogt_work_item")),
            runs=tuple(
                EngineTaskRun.from_payload(row) for row in runs if isinstance(row, dict)
            ),
        )


def _optional_str(value: object) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _optional_int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


@dataclass(frozen=True)
class EngineClient:
    """Access to one session engine."""

    base_url: str
    token: str | None = None
    transport: Transport | None = None
    timeout: int = DEFAULT_TIMEOUT_SECONDS
    #: Kept for the error message when a call fails, so an operator is told
    #: which engine did not answer without the token being anywhere near it.
    label: str = field(default="engine")

    @classmethod
    def from_config(
        cls,
        url: str | None,
        token_file: Path | None,
        *,
        transport: Transport | None = None,
    ) -> EngineClient | None:
        """Build a client, or `None` when no engine is configured.

        `None` is an ordinary answer, not an error: a Vogt with no engine is
        the shape v1 shipped in, and the session operations say so rather
        than failing in a way that reads like an outage.
        """
        if not url or not url.strip():
            return None
        token: str | None = None
        if token_file is not None:
            resolved = Path(token_file).expanduser()
            if resolved.is_file():
                token = resolved.read_text(encoding="utf-8").strip() or None
        return cls(base_url=url.strip().rstrip("/"), token=token, transport=transport)

    # -- the nine things Vogt asks of the engine ---------------------------

    def create_session(
        self,
        *,
        name: str,
        command: list[str] | None,
        cwd: str,
        env: dict[str, str] | None = None,
        prompt: str | None = None,
        model: str | None = None,
        effort: str | None = None,
    ) -> EngineSession:
        """Start a terminal, in `cwd`, running `command`.

        `cwd` is required here even though the engine would default it. The
        default is the engine's `workspace_root`, and a session that opened
        there when Vogt meant a project's tree would be *plausible* and
        wrong — FR-E3 exists because that is the failure worth designing
        out.
        """
        spec: dict[str, Any] = {"name": name, "cwd": cwd}
        if command:
            spec["command"] = command
        if prompt:
            # The engine writes this to a file on its own state directory and
            # tells the child where it is (FR-E4). Vogt sends the text rather
            # than a path because the filesystem the agent will read it from
            # is the engine's, not Vogt's — even when they share a container.
            spec["prompt"] = prompt
        if env:
            # The engine takes pairs, not an object, so that ordering is the
            # caller's and duplicate keys are visible rather than merged.
            spec["env"] = [[key, value] for key, value in env.items()]
        # Sent only when asked for (FR-T11), so a session that named neither
        # is byte-for-byte the request this client has always made and the
        # engine's own defaults keep applying.
        if model:
            spec["model"] = model
        if effort:
            spec["effort"] = effort
        payload = self._call("/api/sessions", method="POST", payload=spec)
        return EngineSession.from_payload(payload if isinstance(payload, dict) else {})

    def list_sessions(self) -> list[EngineSession]:
        payload = self._call("/api/sessions")
        rows = payload if isinstance(payload, list) else []
        return [EngineSession.from_payload(row) for row in rows]

    def get_session(self, session_id: str) -> EngineSession | None:
        """One session, or `None` if the engine has forgotten it.

        Forgetting is normal: a session the engine restarted without is gone,
        and a work item that still records its id should read as "the session
        is over", not as an error.
        """
        payload = self._call(
            f"/api/sessions/{urllib.parse.quote(session_id)}", allow_missing=True
        )
        if not isinstance(payload, dict):
            return None
        # `GET /api/sessions/{id}` answers a detail object wrapping the same
        # summary the list returns; both shapes are read the same way.
        summary = payload.get("summary", payload)
        return EngineSession.from_payload(summary)

    def kill_session(self, session_id: str) -> bool:
        """Stop a session. `False` when the engine no longer had it."""
        payload = self._call(
            f"/api/sessions/{urllib.parse.quote(session_id)}/kill",
            method="POST",
            payload={},
            allow_missing=True,
        )
        return payload is not None

    def archived_session(self, session_id: str) -> EngineArchivedSession | None:
        """What the engine's history says about a terminal that has ended.

        `None` covers three different things the caller must not conflate
        with each other: history is switched off in that engine, the session
        is still running and has not been archived, and the archive was
        pruned. All three mean "the engine cannot tell us", which is why the
        outcome collector reports an unknown outcome rather than assuming the
        session ended cleanly.
        """
        payload = self._call(
            f"/api/history/{urllib.parse.quote(session_id)}", allow_missing=True
        )
        if not isinstance(payload, dict):
            return None
        return EngineArchivedSession.from_payload(payload)

    def list_agent_tasks(self) -> list[EngineAgentTask]:
        """Every scheduled agent task, with its runs (FR-E7).

        Read whole rather than filtered, because the engine has no index on
        the binding and the list is a handful of tasks. Filtering to the
        bound ones happens here, where the project registry is.
        """
        payload = self._call("/api/agent-tasks")
        rows = payload if isinstance(payload, list) else []
        return [
            EngineAgentTask.from_payload(row) for row in rows if isinstance(row, dict)
        ]

    def history_sessions(
        self, *, limit: int = 50, offset: int = 0
    ) -> list[EngineHistorySession]:
        """The engine's archived-session listing, newest-first, paginated.

        Reads whole rows (name, size, outcome) so a caller can browse history
        without a detail fetch per row. The engine may include live sessions
        in this list; those carry a NULL `ended_at`.
        """
        query = urllib.parse.urlencode({"limit": limit, "offset": offset})
        payload = self._call(f"/api/history/sessions?{query}")
        rows = payload if isinstance(payload, list) else []
        return [
            EngineHistorySession.from_payload(row)
            for row in rows
            if isinstance(row, dict)
        ]

    def search_history(
        self, query: str, *, limit: int = 20, include_live: bool = True
    ) -> list[EngineHistoryMatch]:
        """Full-text search over session output, live sessions included.

        `include_live` (default true) supplements the archived FTS index with
        a bounded scan of each running session's scrollback, so output that
        has not been archived yet is still found; those hits carry `live:
        true`. Snippets are plain text.
        """
        params = urllib.parse.urlencode(
            {
                "q": query,
                "limit": limit,
                "include_live": "true" if include_live else "false",
            }
        )
        payload = self._call(f"/api/history/search?{params}")
        rows = payload if isinstance(payload, list) else []
        return [
            EngineHistoryMatch.from_payload(row)
            for row in rows
            if isinstance(row, dict)
        ]

    def history_log(
        self,
        session_id: str,
        *,
        tail_bytes: int = 64 * 1024,
        strip_ansi: bool = True,
    ) -> EngineSessionLog | None:
        """The tail of a session's raw output log, or `None` when there is none.

        Works for a live session too — the engine reads the on-disk log by id
        with no archive row required (this is how a running session can be
        replayed). `strip_ansi` defaults true so a caller gets readable text;
        pass false for the raw escape stream.
        """
        params = urllib.parse.urlencode(
            {
                "tail_bytes": tail_bytes,
                "strip_ansi": "true" if strip_ansi else "false",
            }
        )
        payload = self._call(
            f"/api/history/{urllib.parse.quote(session_id)}/log?{params}",
            allow_missing=True,
        )
        if not isinstance(payload, dict):
            return None
        return EngineSessionLog.from_payload(payload)

    # -- transport ---------------------------------------------------------

    # -- runtime-pinned agent CLIs (#590) ------------------------------------

    def agent_clis(self, *, upstream: bool = False) -> dict[str, Any]:
        """The engine's agent CLI report: active, baked and (asked) upstream."""
        query = "?upstream=true" if upstream else ""
        payload = self._call(f"/api/agent-clis{query}")
        return payload if isinstance(payload, dict) else {}

    def update_agent_cli(self, tool: str, version: str) -> dict[str, Any]:
        """Ask the engine to make `version` of `tool` current; the new report.

        The engine's refusals are the caller's to hear verbatim — a malformed
        version, an unknown tool, an install that failed its smoke check —
        so the four statuses it uses for them are mapped to Vogt's errors
        rather than flattened into "the engine did not answer".
        """
        url = f"{self.base_url}/api/agent-clis/{urllib.parse.quote(tool, safe='')}"
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "User-Agent": USER_AGENT,
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        body = json.dumps({"version": version}).encode("utf-8")
        status, response = self._fetch(url, headers, body=body, method="POST")
        text = response.decode("utf-8", errors="replace").strip()
        said = _engine_error_text(text)
        if status == 400:
            raise InvalidRequest(
                said or f"the {self.label} refused the version {version!r}"
            )
        if status == 404:
            msg = f"the {self.label} knows no agent CLI named {tool!r}"
            raise NotFound(msg)
        if status == 409:
            raise Conflict(said or f"{tool} {version} was not made current")
        if status in (401, 403):
            msg = (
                f"the {self.label} refused this request ({status}): the token "
                "lacks the `agent-clis-write` capability"
            )
            raise EngineUnavailable(msg)
        if status >= 400:
            msg = f"the {self.label} answered {status} for POST /api/agent-clis/{tool}"
            raise EngineUnavailable(msg)
        payload = json.loads(text) if text else {}
        return payload if isinstance(payload, dict) else {}

    def _call(
        self,
        path: str,
        *,
        method: str = "GET",
        payload: dict[str, Any] | None = None,
        allow_missing: bool = False,
    ) -> dict[str, Any] | list[Any] | None:
        url = f"{self.base_url}{path}"
        headers = {"Accept": "application/json", "User-Agent": USER_AGENT}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        body: bytes | None = None
        if payload is not None:
            headers["Content-Type"] = "application/json"
            body = json.dumps(payload).encode("utf-8")

        status, response = self._fetch(url, headers, body=body, method=method)
        if status == 404 and allow_missing:
            return None
        if status in (401, 403):
            msg = (
                f"the {self.label} refused this request ({status}): the token "
                "is missing, wrong, or lacks the `sessions` capability"
            )
            raise EngineUnavailable(msg)
        if status >= 400:
            msg = f"the {self.label} answered {status} for {method} {path}"
            raise EngineUnavailable(msg)
        text = response.decode("utf-8").strip()
        return json.loads(text) if text else {}

    def _fetch(
        self,
        url: str,
        headers: dict[str, str],
        *,
        body: bytes | None = None,
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if self.transport is not None:
            return self.transport(url, headers, body or b"", method)
        request = urllib.request.Request(url, headers=headers, data=body, method=method)
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return int(response.status), bytes(response.read())
        except urllib.error.HTTPError as exc:  # pragma: no cover - network shape
            return int(exc.code), bytes(exc.read())
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            # Deliberately does not include the URL: it is loopback and
            # uninteresting, and the useful half of the answer is that
            # sessions are unavailable while everything else still works.
            msg = f"the {self.label} is not answering: {exc}"
            raise EngineUnavailable(msg) from exc


def _engine_error_text(text: str) -> str:
    """The engine's `{"error": "..."}` body as a sentence, or the raw text."""
    try:
        payload = json.loads(text)
    except ValueError:
        return text
    if isinstance(payload, dict) and isinstance(payload.get("error"), str):
        return str(payload["error"])
    return text


__all__ = [
    "EngineAgentTask",
    "EngineArchivedSession",
    "EngineClient",
    "EngineHistoryMatch",
    "EngineHistorySession",
    "EngineSession",
    "EngineSessionLog",
    "EngineTaskFinding",
    "EngineTaskRun",
    "EngineUnavailable",
    "Transport",
]
