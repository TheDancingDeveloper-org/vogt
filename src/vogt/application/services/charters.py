"""Charters: standing instructions per scope, injected into briefs (WI-2038).

A charter is a short Markdown text attached to the workspace, a project or an
initiative. Every session brief started on that scope carries it, in the order
workspace, then project, then initiative, then the brief itself, so a rule an
overseer would otherwise repeat by hand in every brief is written once.

Three rules shape this module:

- **Versioned, never edited in place.** Each set appends the next version, so
  the history is the rows and every version names who wrote it and why. The
  write is audited like any other.
- **Empty clears.** Setting an empty body is a new version that says "no
  charter"; the brief then carries nothing for that scope.
- **The workspace charter is a person's.** It reaches every session on every
  project, so an agent principal (a worker, or one steered by injected text)
  cannot rewrite it; project and initiative charters stay `work.write`, so
  oversight can set them.
- **Optimistic.** `expected_version` refuses a write made against a version
  that is no longer current, as `preference.set` does.
"""

from __future__ import annotations

from dataclasses import dataclass

from vogt.application.context import AppContext
from vogt.application.models import (
    CharterGetParams,
    CharterGetResult,
    CharterScopeKind,
    CharterSetParams,
    CharterSetResult,
    CharterView,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Charter
from vogt.errors import CharterVersionConflict, InvalidRequest, PersonRequired
from vogt.storage.interface import ReadView, WriteTxn

CHARTER_SET = "charter.set"
CHARTER_SET_EVENT = "charter.set"


@dataclass(frozen=True)
class _Scope:
    kind: CharterScopeKind
    id: str
    ref: str | None


def _scope(view: ReadView, kind: CharterScopeKind, ref: str | None) -> _Scope:
    if kind == "workspace":
        if ref:
            raise InvalidRequest("the workspace charter takes no ref")
        return _Scope(kind, "", None)
    if not ref:
        raise InvalidRequest(f"a {kind} charter needs ref (the {kind} slug)")
    if kind == "project":
        return _Scope(kind, _resolve.project(view, ref).id, ref)
    return _Scope(kind, _resolve.initiative(view, ref).id, ref)


def _view(view: ReadView, scope: _Scope, row: Charter) -> CharterView:
    author = view.actor_by_id(row.author_actor_id)
    return CharterView(
        scope=scope.kind,
        ref=scope.ref,
        version=row.version,
        body=row.body,
        author=row.author_actor_id if author is None else author.identity_ref,
        created_at=row.created_at,
        reason=row.reason,
    )


def get_charter(ctx: AppContext, params: CharterGetParams) -> CharterGetResult:
    with ctx.declared.read() as view:
        scope = _scope(view, params.scope, params.ref)
        current = view.charter(scope_kind=scope.kind, scope_id=scope.id)
        history = (
            view.charter_history(
                scope_kind=scope.kind, scope_id=scope.id, limit=params.limit
            )
            if params.history
            else []
        )
        return CharterGetResult(
            current=None if current is None else _view(view, scope, current),
            history=[_view(view, scope, row) for row in history],
        )


def set_charter(ctx: AppContext, params: CharterSetParams) -> CharterSetResult:
    body_text = params.body.strip()
    if params.scope == "workspace" and ctx.principal.kind == "agent":
        msg = (
            "the workspace charter reaches every session on every project, so "
            f"only a person sets it, not an agent ({ctx.principal.identity_ref})"
        )
        raise PersonRequired(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[CharterSetResult]:
        scope = _scope(txn, params.scope, params.ref)
        existing = txn.charter(scope_kind=scope.kind, scope_id=scope.id)
        current = 0 if existing is None else existing.version
        if params.expected_version is not None and params.expected_version != current:
            msg = (
                f"the {scope.kind} charter is at version {current}, not "
                f"{params.expected_version}; re-read it and apply the change "
                "on top of the current text"
            )
            raise CharterVersionConflict(msg)
        charter = Charter(
            scope_kind=scope.kind,
            scope_id=scope.id,
            version=current + 1,
            body=body_text,
            author_actor_id=actor.id,
            created_at=ctx.clock(),
            reason=params.reason,
        )
        txn.insert_charter(charter)
        return WriteOutcome(
            result=CharterSetResult(charter=_view(txn, scope, charter)),
            entity_kind="charter",
            entity_id=f"{scope.kind}:{scope.id}",
            payload=charter.model_dump(mode="json"),
            event_kind=CHARTER_SET_EVENT,
            summary={
                "scope": scope.kind,
                "ref": scope.ref,
                "version": charter.version,
                "cleared": not body_text,
            },
        )

    return audited_write(ctx, operation=CHARTER_SET, reason=params.reason, body=body)


def charters_for(
    view: ReadView, *, project_id: str | None, initiative_id: str | None
) -> list[tuple[str, Charter]]:
    """The charters a brief carries, in inheritance order: workspace, then
    project, then initiative. Each is `(heading, charter)`; a scope with no
    charter, or a cleared one, is left out."""
    scopes: list[tuple[str, CharterScopeKind, str]] = [("Workspace", "workspace", "")]
    if project_id is not None:
        project = view.project_by_id(project_id)
        label = project.slug if project is not None else project_id
        scopes.append((f"Project — {label}", "project", project_id))
    if initiative_id is not None:
        initiative = view.initiative_by_id(initiative_id)
        label = initiative.slug if initiative is not None else initiative_id
        scopes.append((f"Initiative — {label}", "initiative", initiative_id))
    found: list[tuple[str, Charter]] = []
    for heading, kind, scope_id in scopes:
        row = view.charter(scope_kind=kind, scope_id=scope_id)
        if row is not None and row.body.strip():
            found.append((heading, row))
    return found


__all__ = ["charters_for", "get_charter", "set_charter"]
