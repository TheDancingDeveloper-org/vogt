"""Instance lifecycle and status."""

from __future__ import annotations

from pathlib import Path

from vogt import __version__
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS, StdioServer
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext
from vogt.application.models import (
    InitParams,
    InitResult,
    McpStdioParams,
    McpStdioResult,
    ServeParams,
    ServeResult,
    StatusParams,
    StatusResult,
    StoreCounts,
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
                work_items=counts.work_items,
                initiatives=counts.initiatives,
            ),
        )


def serve_mcp_stdio(ctx: AppContext, params: McpStdioParams) -> McpStdioResult:
    """Serve MCP over stdin/stdout until the stream closes (FR-A5).

    Local-only: it takes over this process's streams, which is meaningful
    exactly where the data directory is. A remote MCP client uses the
    streamable-HTTP transport at `/mcp` instead, which arrives at M4.
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
    """Start the one server that answers everything (NFR-D1).

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
