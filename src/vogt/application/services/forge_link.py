"""`forge.link` — the explicit act that makes a project upstream-truth (#181).

Linking is a decision, not an inference: a project is `linked` because
somebody said so, through this operation or through `project.import`'s
clone+consolidate (`forge.publish` joins them in #182). What this validates
is exactly what write-through will need — a `repo_url` some registered
provider matches, and a usable credential for it — using the same
resolution the write path uses (`provider_for` via `_writer_provider`), so
"linkable" cannot drift from "writable".

There is deliberately no `forge.unlink`: withdrawing the upstream-truth
model from a project that has been living in it is a migration question,
not a toggle, and it is out of #181's scope by decision.
"""

from __future__ import annotations

from vogt.adapters.forge import unsupported_reason
from vogt.application.context import AppContext
from vogt.application.models import ForgeLinkParams, ProjectResult
from vogt.application.services import _resolve
from vogt.application.services.writeback import _writer_provider
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor
from vogt.errors import LinkRefused
from vogt.storage.interface import ProjectUpdate, WriteTxn

FORGE_LINK = "forge.link"
FORGE_LINKED_EVENT = "forge.linked"


def link_project(ctx: AppContext, params: ForgeLinkParams) -> ProjectResult:
    """Set an already-registered project `linked`, preconditions named.

    Idempotent on a linked project — re-linking re-validates and re-affirms,
    which is the honest behaviour when a credential was rotated. Refusals
    name the missing precondition rather than a bare "cannot link".
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        project = _resolve.project(txn, params.project)
        unsupported = unsupported_reason(project.repo_url)
        if unsupported is not None:
            msg = (
                f"cannot link {project.slug!r}: {unsupported}. Set a supported "
                "repo_url with `project update`, or import the repository "
                "through `project import`."
            )
            raise LinkRefused(msg)
        provider, identity = _writer_provider(ctx, actor, project.repo_url)
        if provider is None:
            msg = (
                f"cannot link {project.slug!r}: no usable credential for "
                f"{project.repo_url} — link your own forge account "
                "(`forge account link`, #179) or configure the instance "
                "token file (FR-S7), then retry."
            )
            raise LinkRefused(msg)
        txn.update_project(
            project.id, ProjectUpdate(link_state="linked"), at=ctx.clock()
        )
        updated = txn.project_by_id(project.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=ProjectResult(project=updated),
            entity_kind="project",
            entity_id=project.id,
            payload={"link_state": "linked", "credential": identity},
            event_kind=FORGE_LINKED_EVENT,
            summary={
                "slug": project.slug,
                "from": project.link_state,
                "to": "linked",
                "credential": identity,
            },
        )

    return audited_write(ctx, operation=FORGE_LINK, reason=params.reason, body=body)


__all__ = ["FORGE_LINK", "FORGE_LINKED_EVENT", "link_project"]
