"""The M0 use-cases.

Every one of these is reachable from all three transports, because the
registry generates all three from these signatures rather than from three
hand-written surfaces (DESIGN §4.1).
"""

from __future__ import annotations

from vogt import __version__
from vogt.application.context import AppContext
from vogt.application.models import (
    DEFAULT_EXCLUSIONS,
    AuditListResult,
    EventListResult,
    InitParams,
    InitResult,
    ListAuditParams,
    ListEventsParams,
    ListProjectsParams,
    ProjectListResult,
    ProjectResult,
    RegisterProjectParams,
    StatusParams,
    StatusResult,
    StoreCounts,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Project
from vogt.core.ids import slugify
from vogt.errors import Conflict, InvalidRequest
from vogt.storage.interface import WriteTxn

PROJECT_REGISTER = "project.register"
PROJECT_REGISTERED_EVENT = "project.registered"


def init_instance(ctx: AppContext, params: InitParams) -> InitResult:
    """Create or bring forward the instance in the configured data directory.

    Idempotent: running it against an existing instance migrates it and
    reports `created=false`, because "make sure this is ready" is a thing
    both a human and a startup probe need to be able to say twice.
    """
    del params
    ctx.config.resolved_data_dir.mkdir(parents=True, exist_ok=True)
    declared_report = ctx.declared.migrate()
    observed_report = ctx.observed.migrate()

    created = not ctx.declared.is_initialized()
    if created:
        result = ctx.declared.bootstrap(ctx.principal)
        instance_id = result.instance_id
        ctx.observed.bind_instance(instance_id)
    else:
        with ctx.declared.read() as view:
            instance_id = view.instance_id()

    return InitResult(
        instance_id=instance_id,
        data_dir=str(ctx.config.resolved_data_dir),
        created=created,
        declared_schema_version=declared_report.version,
        observed_schema_version=observed_report.version,
        migrations_applied=[
            *(f"declared:{name}" for name in declared_report.applied),
            *(f"observed:{name}" for name in observed_report.applied),
        ],
    )


def status(ctx: AppContext, params: StatusParams) -> StatusResult:
    """Report what this instance is and how much is in it."""
    del params
    with ctx.declared.read() as view:
        counts = view.counts()
        return StatusResult(
            vogt_version=__version__,
            instance_id=view.instance_id(),
            data_dir=str(ctx.config.resolved_data_dir),
            principal=ctx.principal.identity_ref,
            revision=view.current_revision(),
            declared_schema_version=ctx.declared.schema_version(),
            observed_schema_version=ctx.observed.schema_version(),
            counts=StoreCounts(
                projects=counts.projects,
                actors=counts.actors,
                events=counts.events,
                audit=counts.audit,
            ),
        )


def register_project(ctx: AppContext, params: RegisterProjectParams) -> ProjectResult:
    """Register an existing folder or repository as a project (FR-P1).

    Registration is never refused on contract grounds (FR-G11) — the contract
    is a status to be read, not a barrier to pass. It *is* refused for a
    duplicate slug, which is a naming collision rather than a judgement about
    the project.
    """
    slug = slugify(params.name)
    if not slug:
        msg = f"cannot derive a slug from name {params.name!r}"
        raise InvalidRequest(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        if txn.project_by_slug(slug) is not None:
            msg = f"a project with slug {slug!r} is already registered"
            raise Conflict(msg)
        now = ctx.clock()
        project = Project(
            id=ctx.id_factory("prj"),
            slug=slug,
            name=params.name,
            root_path=params.root_path,
            repo_url=params.repo_url,
            lifecycle_state=params.lifecycle_state,
            compliance_status="not_checked",
            exclusions=list(DEFAULT_EXCLUSIONS),
            trust_state="unverified",
            created_at=now,
            updated_at=now,
        )
        txn.insert_project(project)
        return WriteOutcome(
            result=ProjectResult(project=project),
            entity_kind="project",
            entity_id=project.id,
            payload=project.model_dump(mode="json"),
            event_kind=PROJECT_REGISTERED_EVENT,
            summary={"slug": project.slug, "name": project.name},
        )

    return audited_write(
        ctx, operation=PROJECT_REGISTER, reason=params.reason, body=body
    )


def list_projects(ctx: AppContext, params: ListProjectsParams) -> ProjectListResult:
    with ctx.declared.read() as view:
        return ProjectListResult(
            projects=view.list_projects(limit=params.limit, offset=params.offset),
            total=view.counts().projects,
        )


def list_events(ctx: AppContext, params: ListEventsParams) -> EventListResult:
    """Read the cursor-based notification feed (FR-N1).

    `next_cursor` is the seq of the last row returned — pass it back as
    `after`. When the feed is empty it is the caller's own cursor, so a
    polling client never rewinds.
    """
    with ctx.declared.read() as view:
        events = view.list_events(after=params.after, limit=params.limit)
    next_cursor = events[-1].seq if events else params.after
    return EventListResult(events=events, next_cursor=next_cursor)


def list_audit(ctx: AppContext, params: ListAuditParams) -> AuditListResult:
    with ctx.declared.read() as view:
        return AuditListResult(
            records=view.list_audit(
                limit=params.limit,
                actor_id=params.actor_id,
                operation=params.operation,
                entity_id=params.entity_id,
            )
        )
