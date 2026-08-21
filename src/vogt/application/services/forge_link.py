"""`forge.link` — the explicit act that makes a project upstream-truth (#181).

Linking is a decision, not an inference: a project is `linked` because
somebody said so, through this operation, through `project.import`'s
clone+consolidate, or through `forge.publish` (#182). What this validates is
exactly what write-through will need — a `repo_url` some registered provider
matches, and a usable credential for it — using the same resolution the
write path uses (`provider_for` via `_writer_provider`), so "linkable"
cannot drift from "writable".

Since #183 linking also **migrates**: every open native work item the
project still holds is published upstream and re-keyed, so the moment a
project is linked its work surface is entirely upstream-truth. The policy
gate runs before the link (no project ends up linked-but-unmigratable by
policy), and a mid-migration provider failure fails loud with the migrated
and still-native items named — re-linking resumes, because linking is
idempotent and the migration population is whatever is still native.

There is deliberately no `forge.unlink`: withdrawing the upstream-truth
model from a project that has been living in it is a migration question,
not a toggle, and it is out of #181's scope by decision.
"""

from __future__ import annotations

from vogt.adapters.forge import unsupported_reason
from vogt.application.context import AppContext
from vogt.application.models import ForgeLinkParams, ForgeLinkResult
from vogt.application.services import _resolve, native_migration
from vogt.application.services.writeback import _writer_provider
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Project
from vogt.errors import LinkRefused
from vogt.storage.interface import ProjectUpdate, WriteTxn

FORGE_LINK = "forge.link"
FORGE_LINKED_EVENT = "forge.linked"


def link_project(ctx: AppContext, params: ForgeLinkParams) -> ForgeLinkResult:
    """Set an already-registered project `linked`, preconditions named.

    Idempotent on a linked project — re-linking re-validates, re-affirms,
    and re-runs the migration over whatever is still native, which is both
    the honest behaviour when a credential was rotated and the resume path
    after a partial migration. Refusals name the missing precondition
    rather than a bare "cannot link".
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        pending = native_migration.open_native_items(view, project)

    # The config travels with the question: a Forgejo host is supported
    # exactly when `[forge_token_files]` names it (#176).
    unsupported = unsupported_reason(project.repo_url, ctx.config)
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
    # Before the link commits, not after: a policy that will refuse the
    # migration's creates must refuse the whole act (#183), or the project
    # ends up linked with native items no re-run could ever move.
    native_migration.require_migratable(project, pending)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[Project]:
        del actor
        txn.update_project(
            project.id, ProjectUpdate(link_state="linked"), at=ctx.clock()
        )
        updated = txn.project_by_id(project.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=updated,
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

    linked = audited_write(ctx, operation=FORGE_LINK, reason=params.reason, body=body)

    migrated = []
    if pending:
        repo = provider.parse(project.repo_url)
        assert repo is not None  # unsupported_reason cleared it above
        migrated = native_migration.migrate_open_native_items(
            ctx,
            project=linked,
            items=pending,
            provider=provider,
            identity=identity,
            repo=repo,
            reason=params.reason,
            operation=FORGE_LINK,
        )
    return ForgeLinkResult(project=linked, migrated=migrated)


__all__ = ["FORGE_LINK", "FORGE_LINKED_EVENT", "link_project"]
