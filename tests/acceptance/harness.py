"""The live-instance acceptance suite's harness.

This module is the whole framework: what a case is, what it may return, how a
failure is turned into a readable result, and the guarantees the suite keeps
about credentials. It imports the standard library only, so
``python3 tests/acceptance/run.py`` works in a checkout that has never run
``uv sync``.

Three rules shape it:

* A case says what it checked (``PASS``), why it could not (``SKIP``), or what
  was wrong (``FAIL``). A skip is never a pass.
* Every string that leaves a case — evidence, error text, a report, a log
  line — goes through :func:`redact` first, with the run's own token among the
  secrets. The token is never in argv, never in the environment, and never in
  a report.
* Sessions started by a case are stopped by that case's cleanup, and again by
  an atexit sweep that looks for the run's unique tag. ``try``/``finally``
  covers an exception; the tag covers a killed process.
"""

from __future__ import annotations

import atexit
import contextlib
import json
import re
import secrets
import subprocess
import sys
import time
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Literal

Verdict = Literal["PASS", "FAIL", "SKIP"]
Severity = Literal["critical", "high", "medium", "low"]

#: Shape-based scrubbing for evidence that came from a server, a transcript or
#: an exception. The run token is replaced by exact match; these catch the
#: normal shapes a value takes when it is quoted back to us.
_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"(?i)\b(bearer)\s+[A-Za-z0-9._~+/=-]{8,}"), r"\1 <redacted>"),
    (re.compile(r"\bsk-[A-Za-z0-9_-]{8,}"), "sk-<redacted>"),
    (
        re.compile(
            r"(?i)\b(token|secret|password|passwd|api[_-]?key|credential)"
            r"(\s*[:=]\s*)(['\"]?)[^\s'\",}]{6,}\3"
        ),
        r"\1\2\3<redacted>\3",
    ),
)


def redact(text: str, *, secrets: Iterable[str] = ()) -> str:
    """``text`` with the run's secrets and credential-shaped strings masked.

    Exact matches go first: a credential that does not look like one still
    must not be printed. The patterns are a second net, not the first defence.
    """
    for secret in secrets:
        if secret:
            text = text.replace(secret, "<redacted>")
    for pattern, replacement in _PATTERNS:
        text = pattern.sub(replacement, text)
    return text


@dataclass(frozen=True)
class Token:
    """A bearer the suite presents. Its repr and str are always masked."""

    value: str = field(repr=False)

    def __str__(self) -> str:
        return "<redacted>"

    def __repr__(self) -> str:
        return "Token(<redacted>)"

    def header(self) -> str:
        return f"Bearer {self.value}"


class SkipCase(Exception):
    """The case cannot run here, and says why. Never counted as a pass."""


class TransportError(Exception):
    """The target could not be reached. Carries no credential."""


class HttpStatusError(Exception):
    """A non-2xx answer a case did not expect."""

    def __init__(self, status: int, method: str, path: str, body: str = "") -> None:
        self.status = status
        self.method = method
        self.path = path
        self.body = body
        super().__init__(
            f"{method} {path} -> {status}{f': {body[:300]}' if body else ''}"
        )


class RpcError(Exception):
    """A JSON-RPC error message (unknown tool, unknown method)."""

    def __init__(self, code: int, message: str) -> None:
        self.code = code
        self.message = message
        super().__init__(f"JSON-RPC {code}: {message}")


class ToolError(Exception):
    """A tool result with ``isError: true`` — a domain refusal, not a 5xx."""

    def __init__(self, message: str) -> None:
        self.message = message
        super().__init__(message)


@dataclass
class Check:
    """What one case concluded."""

    verdict: Verdict
    evidence: str
    data: dict[str, Any] = field(default_factory=dict)


def passed(evidence: str, **data: Any) -> Check:
    return Check("PASS", evidence, data)


def failed(evidence: str, **data: Any) -> Check:
    return Check("FAIL", evidence, data)


def skipped(reason: str, **data: Any) -> Check:
    return Check("SKIP", reason, data)


@dataclass(frozen=True)
class Case:
    """One check: a stable id, the group it belongs to, and its requirement.

    ``requirement`` is a matrix row from ``matrix.toml`` (``S1``) or a work
    item (``WI-4130``). The report's coverage table is built from it, so a
    case that names neither shows up as unattributed rather than hidden.
    """

    id: str
    group: str
    requirement: str
    severity: Severity
    title: str
    run: Callable[[Context], Check]


@dataclass
class KnownIssue:
    """A failure that is already known, with the item that owns it."""

    test: str
    ref: str
    reason: str
    match: str | None = None


@dataclass
class Options:
    """Everything the suite may vary between runs."""

    base_url: str | None = None
    token_file: Path | None = None
    read_token_file: Path | None = None
    read_token: Token | None = None
    with_sessions: bool = False
    allow_writes: bool = False
    allow_restart: bool = False
    expect_digest: str | None = None
    kube_context: str = "estate-ro"
    timeout: float = 30.0
    fake: bool = False
    report: Path | None = None
    json_report: Path | None = None
    only: tuple[str, ...] = ()
    prod_instance_id: str | None = None
    dry_run: bool = False
    #: Values shared by every case in the run (a throwaway project's slug, the
    #: session id S1 started). One dict, because a case's Context is fresh.
    state: dict[str, Any] = field(default_factory=dict)


@dataclass
class CaseResult:
    case: Case
    check: Check
    duration_ms: int
    known: KnownIssue | None = None

    @property
    def verdict(self) -> str:
        if self.known is not None and self.check.verdict == "FAIL":
            return "FAIL-known"
        return self.check.verdict

    @property
    def counts_as_failure(self) -> bool:
        return self.verdict == "FAIL"


#: Cleanup that must outlive one case: a throwaway project is created by one
#: check and mutated by later ones, so it is torn down after the whole run
#: (and again from atexit) rather than between the checks that share it.
_SHARED_CLEANUP: list[tuple[str, Callable[[], None]]] = []


def on_run_cleanup(label: str, action: Callable[[], None]) -> None:
    _SHARED_CLEANUP.append((label, action))


def run_shared_cleanup() -> list[str]:
    problems: list[str] = []
    for label, action in reversed(_SHARED_CLEANUP):
        try:
            action()
        except Exception as exc:
            problems.append(redact(f"{label}: {type(exc).__name__}: {exc}"))
    _SHARED_CLEANUP.clear()
    return problems


def run_tag() -> str:
    """A unique tag for this run, embedded in every session's reason."""
    stamp = datetime.now(UTC).strftime("%Y%m%dT%H%M%SZ")
    return f"acceptance-{stamp}-{secrets.token_hex(3)}"


class Context:
    """One case's view of the run: client, options, and cleanup."""

    def __init__(self, client: Any, options: Options, tag: str) -> None:
        self.client = client
        self.options = options
        self.tag = tag
        self.state: dict[str, Any] = options.state
        self.cleanup: list[tuple[str, Callable[[], None]]] = []
        self.notes: list[str] = []
        self.started_sessions: list[str] = []
        self._stopped: set[str] = set()

    # -- gates ---------------------------------------------------------
    def require_live(self) -> None:
        if self.options.fake:
            raise SkipCase("fake-server mode: this check needs a live instance")

    def require_sessions(self) -> None:
        if not self.options.with_sessions:
            raise SkipCase("--with-sessions was not given (read-only mode)")

    def require_writes(self) -> None:
        if not self.options.allow_writes:
            raise SkipCase("--allow-writes was not given (read-only mode)")

    def require_restart(self) -> None:
        if not self.options.allow_restart:
            raise SkipCase(
                "--allow-restart was not given: restarting the engine is an "
                "operator action, not something this suite does"
            )

    def require_kubectl(self) -> None:
        if self.options.fake:
            raise SkipCase("fake-server mode has no cluster")
        try:
            proc = subprocess.run(
                [
                    "kubectl",
                    "--context",
                    self.options.kube_context,
                    "version",
                    "--request-timeout=5s",
                ],
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )
        except (FileNotFoundError, subprocess.TimeoutExpired) as exc:
            raise SkipCase(
                f"no read-only cluster access: {type(exc).__name__}"
            ) from None
        if proc.returncode != 0:
            detail = redact(
                (proc.stderr or proc.stdout).strip().splitlines()[-1:][0]
                if (proc.stderr or proc.stdout).strip()
                else ""
            )
            raise SkipCase(
                f"kubectl context {self.options.kube_context!r} is not usable"
                + (f": {detail}" if detail else "")
            )

    # -- cleanup -------------------------------------------------------
    def on_cleanup(self, label: str, action: Callable[[], None]) -> None:
        self.cleanup.append((label, action))

    def track_session(self, session_id: str) -> None:
        self.started_sessions.append(session_id)

    def stop_session(self, session_id: str) -> None:
        if session_id in self._stopped:
            return
        self._stopped.add(session_id)
        with contextlib.suppress(Exception):  # best-effort; the sweep retries
            self.client.tool(
                "session_stop", {"id": session_id, "reason": f"{self.tag}: stop"}
            )

    def run_cleanup(self) -> list[str]:
        """Run this case's cleanup; return failures, never raise."""
        problems: list[str] = []
        for label, action in reversed(self.cleanup):
            try:
                action()
            except Exception as exc:
                problems.append(redact(f"{label}: {type(exc).__name__}: {exc}"))
        self.cleanup.clear()
        for session_id in list(self.started_sessions):
            try:
                self.stop_session(session_id)
            except Exception as exc:
                problems.append(redact(f"stop {session_id}: {exc}"))
        return problems


@dataclass
class RunSummary:
    results: list[CaseResult]
    tag: str
    started_at: datetime
    finished_at: datetime
    mode: str
    target: str
    fake: bool

    def count(self, verdict: str) -> int:
        return sum(1 for r in self.results if r.verdict == verdict)

    @property
    def failures(self) -> list[CaseResult]:
        return [r for r in self.results if r.counts_as_failure]


def select(cases: Sequence[Case], only: Sequence[str]) -> list[Case]:
    """Cases matching ``--only`` (group names or case-id prefixes)."""
    if not only:
        return list(cases)
    wanted = [token.strip().lower() for token in only if token.strip()]
    chosen: list[Case] = []
    for case in cases:
        haystack = (case.group.lower(), case.id.lower())
        if any(
            token == haystack[0] or haystack[1].startswith(token) for token in wanted
        ):
            chosen.append(case)
    return chosen


def known_issue_for(
    issues: Sequence[KnownIssue], result: CaseResult
) -> KnownIssue | None:
    if result.check.verdict != "FAIL":
        return None
    evidence = result.check.evidence
    for issue in issues:
        if issue.test != result.case.id:
            continue
        if issue.match is None or issue.match in evidence:
            return issue
    return None


def run_cases(
    cases: Sequence[Case],
    *,
    client: Any,
    options: Options,
    tag: str,
    known_issues: Sequence[KnownIssue] = (),
    echo: Callable[[str], None] = print,
) -> RunSummary:
    """Run every case, in order, with cleanup after each one."""
    started = datetime.now(UTC)
    results: list[CaseResult] = []
    for case in cases:
        ctx = Context(client, options, tag)
        began = time.monotonic()
        check: Check
        try:
            check = case.run(ctx)
        except SkipCase as exc:
            check = skipped(str(exc))
        except AssertionError as exc:
            check = failed(redact(str(exc) or "assertion failed"))
        except (TransportError, HttpStatusError, RpcError, ToolError) as exc:
            check = failed(redact(f"{type(exc).__name__}: {exc}"))
        except Exception as exc:  # a case must not kill the run
            check = failed(redact(f"{type(exc).__name__}: {exc}"))
        finally:
            problems = ctx.run_cleanup()
        if problems:
            note = "cleanup failed: " + "; ".join(problems)
            if check.verdict == "PASS":
                check = failed(note, **check.data)
            else:
                check = Check(check.verdict, f"{check.evidence}; {note}", check.data)
            if ctx.notes:
                check = Check(
                    check.verdict,
                    f"{check.evidence}; {'; '.join(ctx.notes)}",
                    check.data,
                )
        duration = int((time.monotonic() - began) * 1000)
        result = CaseResult(case=case, check=check, duration_ms=duration)
        result.known = known_issue_for(known_issues, result)
        results.append(result)
        echo(
            f"{result.verdict:<10} {case.id:<28} {case.group:<12} "
            f"{case.requirement:<10} {redact(check.evidence, secrets=client.secrets)}"
        )
    shared_problems = run_shared_cleanup()
    if shared_problems:
        echo(redact("cleanup: " + "; ".join(shared_problems), secrets=client.secrets))
    finished = datetime.now(UTC)
    mode = (
        "fake" if options.fake else ("write" if options.allow_writes else "read-only")
    )
    if options.with_sessions and not options.fake:
        mode += "+sessions"
    return RunSummary(
        results=results,
        tag=tag,
        started_at=started,
        finished_at=finished,
        mode=mode,
        target=options.base_url or "in-process fake",
        fake=options.fake,
    )


def load_sessions_for_sweep(client: Any, tag: str) -> list[str]:
    """Sessions whose reason carries this run's tag (the atexit sweep)."""
    try:
        payload = client.tool("session_sweep", {"states": [], "mode": "full"})
    except Exception:
        return []
    rows = payload.get("rows", []) if isinstance(payload, dict) else []
    found: list[str] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        # The live sweep row is {attention, session: SessionSummary, ...};
        # the fake used to answer a flat row and is kept working too.
        session = row.get("session")
        record = session if isinstance(session, dict) else row
        reason = str(record.get("reason") or "")
        if tag in reason:
            identifier = record.get("id") or record.get("session_id")
            if isinstance(identifier, str):
                found.append(identifier)
    return found


def sweep_sessions(client: Any, tag: str) -> list[str]:
    """Stop every session tagged with this run. Returns what was stopped."""
    stopped: list[str] = []
    for session_id in load_sessions_for_sweep(client, tag):
        try:
            client.tool(
                "session_stop", {"id": session_id, "reason": f"{tag}: atexit sweep"}
            )
            stopped.append(session_id)
        except Exception:
            continue
    return stopped


def install_atexit_sweep(client: Any, tag: str) -> None:
    def _sweep() -> None:
        sys.stdout.flush() if sys.stdout else None
        sweep_sessions(client, tag)

    atexit.register(_sweep)


def read_json_file(path: Path) -> Any:
    return json.loads(path.read_text("utf-8"))
