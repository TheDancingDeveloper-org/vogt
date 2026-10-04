"""Instance lifecycle and status."""

from __future__ import annotations

from pathlib import Path
from typing import Literal

from vogt import __version__
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS, StdioServer
from vogt.adapters.mcp.surface import McpSurface
from vogt.adapters.peer import PeerUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    CloneInfo,
    DiagnosticCheck,
    DiagnosticsParams,
    DiagnosticsResult,
    InitParams,
    InitResult,
    McpStdioParams,
    McpStdioResult,
    MigrateParams,
    MigrateResult,
    PeerDiagnostics,
    RecentLog,
    ServeParams,
    ServeResult,
    StatusParams,
    StatusResult,
    StoreCounts,
    StoreMigration,
)
from vogt.application.services.auth import (
    adopt_bootstrap_agent_token,
    adopt_bootstrap_core_token,
)
from vogt.errors import InvalidRequest, VogtError
from vogt.observability import (
    PROCESS_STARTED_AT,
    RECENT_PROBLEMS_CAPACITY,
    capturing_problems,
    recent_problems,
    redact,
)


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

    # After the instance exists, never before: adoption is a declared write
    # and there is nothing to write into until bootstrap has run.
    bootstrap_core_token = adopt_bootstrap_core_token(ctx)
    bootstrap_agent_token = adopt_bootstrap_agent_token(ctx)

    return InitResult(
        instance_id=instance_id,
        data_dir=str(ctx.config.resolved_data_dir),
        created=created,
        bootstrap_core_token=bootstrap_core_token,
        bootstrap_agent_token=bootstrap_agent_token,
        declared_schema_version=declared_report.version,
        observed_schema_version=observed_report.version,
        migrations_applied=[
            *(f"declared:{name}" for name in declared_report.applied),
            *(f"observed:{name}" for name in observed_report.applied),
        ],
    )


def migrate_instance(ctx: AppContext, params: MigrateParams) -> MigrateResult:
    """Bring both stores forward to this build's schema.

    `init` has always done this as part of creating an instance, and for a
    year that was the whole answer — which meant the verb an operator reaches
    for after a digest bump was `init`, a word that reads like "start over"
    on a live data directory. Nobody is served by having to know it is
    idempotent.

    So this is the same act under the name it actually has. It does not
    bootstrap and cannot create an instance: run it against an empty data
    directory and it says so rather than quietly conjuring one, because the
    two operations answer different questions and a `migrate` that silently
    created an estate would be the more expensive surprise.
    """
    del params
    if not ctx.declared.is_initialized():
        msg = (
            "no instance in this data directory to migrate — `vogt init` "
            "creates one, and is idempotent against an existing instance"
        )
        raise InvalidRequest(msg)

    declared_report = ctx.declared.migrate()
    observed_report = ctx.observed.migrate()
    return MigrateResult(
        data_dir=str(ctx.config.resolved_data_dir),
        declared_schema_version=declared_report.version,
        observed_schema_version=observed_report.version,
        declared_schema_expected=ctx.declared.bundled_schema_version(),
        observed_schema_expected=ctx.observed.bundled_schema_version(),
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
        stamp = view.clone_stamp()
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
                work_items=counts.work_items,
                initiatives=counts.initiatives,
            ),
            clone=None
            if stamp is None
            else CloneInfo(
                source_instance_id=stamp.source_instance_id,
                cloned_at=stamp.cloned_at,
                backup_taken_at=stamp.backup_taken_at,
            ),
        )


def instance_diagnostics(
    ctx: AppContext, params: DiagnosticsParams
) -> DiagnosticsResult:
    """Is this instance what it should be, and is it well — in one read.

    Version, the image digest the deployment stated, uptime, named readiness
    checks, per-store migration state and the process's recent problems
    (redacted) — plus, on request, the same answer from a configured peer
    instance, so an agent confirming a deploy needs no tailnet route to it
    and no orchestrator access.

    Every check reports rather than raises: a diagnostics read that fails
    when something is wrong would fail exactly when it is needed.
    """
    from vogt.application.services.views import freshness_of

    checks: list[DiagnosticCheck] = []
    migrations: dict[str, StoreMigration] = {}
    instance_id: str | None = None

    for name, store in (("declared", ctx.declared), ("observed", ctx.observed)):
        try:
            applied = store.schema_version()
            expected = store.bundled_schema_version()
        except Exception as exc:  # report, never raise
            checks.append(
                DiagnosticCheck(
                    name=f"{name}_store",
                    status="failing",
                    detail=redact(f"store did not answer: {exc}"),
                )
            )
            continue
        migrations[name] = StoreMigration(
            applied=applied, expected=expected, pending=max(expected - applied, 0)
        )
        checks.append(
            DiagnosticCheck(
                name=f"{name}_store",
                status="ok" if applied == expected else "failing",
                detail=None
                if applied == expected
                else f"schema {applied}, this build expects {expected} — run migrate",
            )
        )
    try:
        with ctx.declared.read() as view:
            instance_id = view.instance_id()
    except Exception as exc:  # report, never raise
        checks.append(
            DiagnosticCheck(name="instance", status="failing", detail=redact(str(exc)))
        )

    if ctx.engine is None:
        checks.append(
            DiagnosticCheck(
                name="engine",
                status="not_configured",
                detail="no engine_url; session operations are unavailable",
            )
        )
    else:
        try:
            ctx.engine.healthz()
            checks.append(DiagnosticCheck(name="engine", status="ok"))
        except VogtError as exc:
            checks.append(
                DiagnosticCheck(name="engine", status="degraded", detail=str(exc))
            )

    try:
        freshness = freshness_of(ctx)
        checks.append(
            DiagnosticCheck(
                name="collection",
                status="ok" if freshness.status == "fresh" else "degraded",
                detail=freshness.detail
                if freshness.age_seconds is None
                else f"oldest sweep {freshness.age_seconds}s ago"
                + (f"; {freshness.detail}" if freshness.detail else ""),
            )
        )
    except Exception as exc:  # report, never raise
        checks.append(
            DiagnosticCheck(
                name="collection", status="degraded", detail=redact(str(exc))
            )
        )

    counted = [check.status for check in checks if check.status != "not_configured"]
    overall: Literal["ok", "degraded", "failing"] = (
        "failing"
        if "failing" in counted
        else ("degraded" if "degraded" in counted else "ok")
    )
    now = ctx.clock()
    return DiagnosticsResult(
        status=overall,
        vogt_version=__version__,
        image_digest=ctx.config.image_digest,
        instance_id=instance_id,
        started_at=PROCESS_STARTED_AT,
        uptime_seconds=max(int((now - PROCESS_STARTED_AT).total_seconds()), 0),
        checks=checks,
        migrations=migrations,
        recent_log=RecentLog(
            capturing=capturing_problems(),
            lines=recent_problems(params.log_lines),
            capacity=RECENT_PROBLEMS_CAPACITY,
        ),
        peer=_peer_diagnostics(ctx, params),
    )


def _peer_diagnostics(ctx: AppContext, params: DiagnosticsParams) -> PeerDiagnostics:
    """The peer half: asked only on request, and never recursively."""
    if not params.peer:
        return PeerDiagnostics(status="not_requested")
    if ctx.peer is None:
        return PeerDiagnostics(
            status="not_configured",
            detail="set diagnostics_peer_url (and diagnostics_peer_token_file)",
        )
    try:
        answer = ctx.peer.diagnostics(log_lines=params.log_lines)
    except PeerUnavailable as exc:
        return PeerDiagnostics(
            status=exc.status,  # type: ignore[arg-type]
            url=ctx.peer.base_url,
            detail=str(exc),
        )
    return PeerDiagnostics(status="ok", url=ctx.peer.base_url, diagnostics=answer)


def serve_mcp_stdio(ctx: AppContext, params: McpStdioParams) -> McpStdioResult:
    """Serve MCP over stdin/stdout until the stream closes.

    Local-only: it takes over this process's streams, which is meaningful
    exactly where the data directory is. A remote MCP client uses the
    streamable-HTTP transport at `/mcp` instead.
    """
    del params
    server = StdioServer(McpSurface(context_factory=lambda: ctx))
    report = server.serve()
    return McpStdioResult(
        protocol_version=report.protocol_version,
        messages_handled=report.messages_handled,
        supported_protocol_versions=list(SUPPORTED_PROTOCOL_VERSIONS),
    )


def serve(ctx: AppContext, params: ServeParams) -> ServeResult:
    """Start the one server that answers everything.

    Local-only, like `init`: it takes over this process, and a running
    server being asked over HTTP to start another one is not a meaningful
    request.
    """
    from vogt.adapters.http.app import API_PREFIX
    from vogt.adapters.http.server import ServeOptions, run
    from vogt.adapters.mcp.http import MCP_PATH

    options = ServeOptions(
        host=params.host,
        port=params.port,
        tls_cert=None if params.tls_cert is None else Path(params.tls_cert),
        tls_key=None if params.tls_key is None else Path(params.tls_key),
        require_auth=not params.no_auth,
        writes_enabled=not params.read_only,
        schedule_collectors=not params.no_schedule,
    )
    options.validate()
    run(options, config=ctx.config)
    return ServeResult(
        url=f"{options.scheme}://{options.host}:{options.port}",
        api_path=API_PREFIX,
        mcp_path=MCP_PATH,
        auth_required=options.require_auth,
        writes_enabled=options.writes_enabled,
        collecting=options.schedule_collectors,
    )
