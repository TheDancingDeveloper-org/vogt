"""Sessions' own typed reports of where they are (WI-2039).

A worker posts a short structured message — `progress`, `result`,
`question` or `blocker` — with the facts oversight otherwise scrapes from a
transcript: summary, branch, SHA, PR, checks run, blockers, next step. The
report is stored once and read three ways: the session's row (`session.list`,
`session.sweep`, `session.digest` carry the latest one), the Inbox (the
latest report per session is an entry), and the work item (a note comment).

Two rules:

- **A report is the session's own.** From inside a session the token names
  it; an agent naming a different session is refused. A person (or a
  caller with no session) names the session with `id`.
- **Credentials never land.** Every free-text field is passed through the
  agent-activity redactor before it is stored, so a token pasted into
  `checks` or `blockers` is not copied into the comment, the Inbox or the
  audit event. The SHA is left as given: it is a commit id, not text.
- **The text is untrusted.** It is the agent's account, shown verbatim and
  never acted on; the claim check still judges a DONE against git and the
  forge.
"""

from __future__ import annotations

from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    SessionReportParams,
    SessionReportResult,
    SessionReportView,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.agent_activity import redact
from vogt.core.entities import Actor, Comment, SessionReport, SessionReportFields
from vogt.errors import InvalidRequest, ReportRefused
from vogt.storage.interface import ReadView, WriteTxn

SESSION_REPORT = "session.report"
SESSION_REPORTED_EVENT = "session.reported"
#: How many characters of a report the one-line digest form keeps.
_LINE_LIMIT = 200


def report(ctx: AppContext, params: SessionReportParams) -> SessionReportResult:
    """Record a session's typed report; it shows on its row, the Inbox and
    its work item."""
    reason = writes.validate_reason(params.reason)
    summary = redact(params.summary.strip())
    if not summary:
        raise InvalidRequest("a report has a summary")
    own = _own_engine_session(ctx)
    engine_id = _reporting_session(ctx, params.id, own)
    fields = SessionReportFields(
        branch=_blank(params.branch),
        sha=_blank(params.sha, scrub=False),
        pr=_blank(params.pr),
        checks=[redact(c.strip()) for c in params.checks if c.strip()],
        blockers=[redact(b.strip()) for b in params.blockers if b.strip()],
        next_step=_blank(params.next_step),
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionReportResult]:
        work_item_id = _work_item_id(txn, params.work_item, engine_id)
        now = ctx.clock()
        row = SessionReport(
            id=ctx.id_factory("rpt"),
            actor_id=actor.id,
            engine_session_id=engine_id,
            work_item_id=work_item_id,
            kind=params.kind,
            summary=summary,
            fields=fields,
            created_at=now,
        )
        txn.insert_session_report(row)
        if work_item_id is not None:
            txn.insert_comment(
                Comment(
                    id=ctx.id_factory("cmt"),
                    work_item_id=work_item_id,
                    actor_id=actor.id,
                    actor_display_name=actor.display_name,
                    body=comment_body(row),
                    created_at=now,
                )
            )
        result = view_of(txn, row)
        return WriteOutcome(
            result=SessionReportResult(report=result),
            entity_kind="session_report",
            entity_id=row.id,
            payload=row.model_dump(mode="json"),
            event_kind=SESSION_REPORTED_EVENT,
            summary={
                "report_id": row.id,
                "kind": row.kind,
                "engine_session_id": engine_id,
                "work_item": result.work_item,
                "summary": summary[:200],
            },
        )

    return audited_write(ctx, operation=SESSION_REPORT, reason=reason, body=body)


def latest_reports(
    view: ReadView, engine_session_ids: list[str]
) -> dict[str, SessionReportView]:
    """The newest report per engine session, as views, for the row joins."""
    if not engine_session_ids:
        return {}
    found = view.latest_session_reports(engine_session_ids)
    return {key: view_of(view, row) for key, row in found.items()}


def view_of(view: ReadView, row: SessionReport) -> SessionReportView:
    item = None if row.work_item_id is None else view.work_item_by_id(row.work_item_id)
    actor = view.actor_by_id(row.actor_id)
    return SessionReportView(
        id=row.id,
        kind=row.kind,
        summary=row.summary,
        session_id=row.engine_session_id,
        work_item=None if item is None else item.ref,
        branch=row.fields.branch,
        sha=row.fields.sha,
        pr=row.fields.pr,
        checks=list(row.fields.checks),
        blockers=list(row.fields.blockers),
        next_step=row.fields.next_step,
        reporter=None if actor is None else actor.identity_ref,
        created_at=row.created_at,
    )


def one_line(report: SessionReportView) -> str:
    """The digest's form: `result: summary [branch=… sha=… pr=…]`, clipped."""
    facts = " ".join(
        f"{name}={value}"
        for name, value in (
            ("branch", report.branch),
            ("sha", None if report.sha is None else report.sha[:12]),
            ("pr", report.pr),
        )
        if value
    )
    line = f"{report.kind}: {report.summary}"
    if facts:
        line = f"{line} [{facts}]"
    return line if len(line) <= _LINE_LIMIT else line[: _LINE_LIMIT - 1] + "…"


def comment_body(row: SessionReport) -> str:
    """The note left on the work item: the report, readable as a comment."""
    lines = [f"Session report ({row.kind}): {row.summary}"]
    fields = row.fields
    facts = [
        f"{name}: {value}"
        for name, value in (
            ("branch", fields.branch),
            ("sha", fields.sha),
            ("pr", fields.pr),
        )
        if value
    ]
    if facts:
        lines.append(" · ".join(facts))
    if fields.checks:
        lines.append("checks: " + "; ".join(fields.checks))
    if fields.blockers:
        lines.append("blockers: " + "; ".join(fields.blockers))
    if fields.next_step:
        lines.append(f"next: {fields.next_step}")
    if row.engine_session_id:
        lines.append(f"session: {row.engine_session_id}")
    return "\n".join(lines)


# -- helpers ---------------------------------------------------------------


def _own_engine_session(ctx: AppContext) -> str | None:
    """The engine session the calling agent runs in, from its identity.

    `grants._own_engine_session`'s rule, repeated here because `grants`
    imports `sessions`, which reads reports for its rows.
    """
    ref = ctx.principal.identity_ref
    if ref.startswith("agent:engine:"):
        return ref.removeprefix("agent:engine:")
    if ref.startswith("agent:session:"):
        with ctx.declared.read() as view:
            session = view.session_by_id(ref.removeprefix("agent:session:"))
        return None if session is None else session.engine_session_id
    return None


def _reporting_session(ctx: AppContext, wanted: str | None, own: str | None) -> str:
    if wanted is None or not wanted.strip():
        if own is None:
            msg = (
                "give `id`: this caller is not a session Vogt started, so it "
                "does not name one"
            )
            raise InvalidRequest(msg)
        return own
    target = wanted.strip()
    if target.startswith("ses_"):
        with ctx.declared.read() as view:
            session = view.session_by_id(target)
        if session is None:
            msg = f"no session {target!r}"
            raise InvalidRequest(msg)
        target = session.engine_session_id
    if own is not None and target != own:
        msg = "a session reports only for itself"
        raise ReportRefused(msg)
    if own is None and ctx.principal.kind == "agent":
        msg = "an agent that is not a session cannot report for one"
        raise ReportRefused(msg)
    return target


def _work_item_id(txn: WriteTxn, ref: str | None, engine_id: str) -> str | None:
    if ref is not None and ref.strip():
        return _resolve.work_item(txn, ref.strip()).id
    session = txn.session_by_engine_id(engine_id)
    return None if session is None else session.work_item_id


def _blank(value: str | None, *, scrub: bool = True) -> str | None:
    if value is None:
        return None
    cleaned = value.strip()
    if not cleaned:
        return None
    return redact(cleaned) if scrub else cleaned
