"""`vogt serve` — one process, one port, everything path-routed.

`DEPLOYMENT.md` §1: the GUI, `/api`, `/mcp` and the health endpoints all
answer on the same listener. That is a deliberate inversion of cadastre's
production shape, where MCP and the API were two services on two ports and
a compose healthcheck could not speak for the MCP surface.

TLS terminates **in-process** from an operator-owned certificate (NFR-D6,
revised r4). There is no fronting proxy, no `tailscale serve`, and no entry
in Node B's Caddyfile: that Caddy is host infrastructure rather than a
Komodo stack, and a tailnet-only listener holding a Tailscale-issued
certificate gains nothing from being fronted.
"""

from __future__ import annotations

import contextlib
from collections.abc import AsyncIterator
from dataclasses import dataclass
from pathlib import Path

from fastapi import FastAPI, Request

from vogt import __version__
from vogt.adapters.http.access_log import AccessLogSettings, RequestLogMiddleware
from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.adapters.http.health import ServerInfo, add_health_routes
from vogt.adapters.http.scheduler import CollectorSchedule
from vogt.adapters.mcp.http import MCP_PATH, add_mcp_route
from vogt.application.context import AppContext, build_context
from vogt.application.services.auth import Authenticated, authenticate, local
from vogt.config import VogtConfig, load_config
from vogt.core.principal import Principal
from vogt.errors import InvalidRequest
from vogt.observability import configure_logging, logger, set_request_actor
from vogt.registry import OperationRegistry, default_registry

log = logger("serve")


@dataclass(frozen=True)
class ServeOptions:
    """Everything `serve` needs, already validated.

    `host` and `port` have no defaults anywhere in code, images, docs or
    examples (NFR-D2). They encode exposure: a wrong default here is the
    `:18081` incident. The compose file supplies them, and the compose file
    is allowed to know the answer for Node B.
    """

    host: str
    port: int
    tls_cert: Path | None = None
    tls_key: Path | None = None
    require_auth: bool = True
    writes_enabled: bool = True
    #: Collect in the background while serving (FR-L3). The interval itself
    #: is configuration; this is the switch, so `--no-schedule` can turn it
    #: off for a read-only or diagnostic run without editing config.
    schedule_collectors: bool = True

    def validate(self) -> None:
        if not self.host.strip():
            msg = "serve needs a listen address; there is no default (NFR-D2)"
            raise InvalidRequest(msg)
        if (self.tls_cert is None) != (self.tls_key is None):
            msg = "--tls-cert and --tls-key are given together or not at all"
            raise InvalidRequest(msg)
        for label, path in (("--tls-cert", self.tls_cert), ("--tls-key", self.tls_key)):
            if path is not None and not Path(path).is_file():
                msg = f"{label}: no such file: {path}"
                raise InvalidRequest(msg)

    @property
    def scheme(self) -> str:
        return "https" if self.tls_cert else "http"


def build_server(
    options: ServeOptions,
    *,
    config: VogtConfig | None = None,
    registry: OperationRegistry | None = None,
) -> FastAPI:
    """Assemble the one application that serves everything."""
    options.validate()
    active_registry = registry if registry is not None else default_registry()
    # Resolved here rather than left to each `build_context` call, because the
    # schedule needs to read its interval before any request arrives.
    resolved_config = config if config is not None else load_config()

    def context(principal: Principal | None = None) -> AppContext:
        return build_context(config=resolved_config, principal=principal)

    # NFR-I3: migrate before anything can be served, not merely report on it.
    # The deployed topology runs `command: serve` and never runs `init`, so
    # until r13 an image carrying a new migration started against the old
    # schema and failed at whatever first touched a missing table. Doing it
    # here rather than in `run` means every path that assembles the app gets
    # it, including the tests — a fix that only the production entrypoint
    # exercises is one nothing can catch regressing.
    #
    # Idempotent and locked (`migrator.py`), so two containers starting
    # together cannot both apply it, and a start against a current schema
    # costs one lock acquisition. Failure is deliberately not swallowed: a
    # server that could not migrate must not come up and answer with a schema
    # nobody asked for.
    _startup = context()
    _startup.declared.migrate()
    _startup.observed.migrate()

    #: A scheduled sweep has no human behind it. Attributing it to whoever
    #: started the process would put that person's name on evidence they
    #: never asked for, and provenance that says something untrue is worse
    #: than provenance that says "a machine did this".
    scheduler_principal = Principal(
        identity_ref="service:vogt-scheduler",
        kind="agent",
        display_name="collector schedule",
    )
    schedule = CollectorSchedule(
        lambda: context(scheduler_principal),
        interval_seconds=(
            resolved_config.sweep_interval_seconds if options.schedule_collectors else 0
        ),
    )

    @contextlib.asynccontextmanager
    async def lifespan(app: FastAPI) -> AsyncIterator[None]:
        del app
        await schedule.start()
        try:
            yield
        finally:
            await schedule.stop()

    def resolve(request: Request) -> tuple[AppContext, Authenticated]:
        """Derive the principal from authentication only (FR-S2).

        The returned context carries the *authenticated* principal, not the
        OS user running the server. That is the whole point: `audited_write`
        attributes every row to `ctx.principal`, so a context built with the
        local principal would stamp an agent's writes with the server
        operator's name and quietly destroy provenance for everything done
        over the network.

        There is deliberately no branch here that reads an identity from the
        request body or a query parameter.
        """
        if not options.require_auth:
            ctx = context()
            caller = local(ctx)
            set_request_actor(caller.principal.identity_ref)
            return ctx, caller

        header = request.headers.get("authorization", "")
        bearer = header[7:] if header.lower().startswith("bearer ") else None
        # Authentication reads the token, so it runs against a context whose
        # principal is irrelevant; the request then runs under the principal
        # that token resolved to.
        caller = authenticate(
            context(), bearer=bearer, writes_enabled=options.writes_enabled
        )
        # The access line is written after the handler has run, so naming the
        # actor here is what lets it say *who* made a request rather than only
        # which address did (NFR-OB1). Set on the request's context, not on
        # the application: two requests are two contexts.
        set_request_actor(caller.principal.identity_ref)
        return context(caller.principal), caller

    app = build_app(
        registry=active_registry,
        context_factory=context,
        authorize_request=None if not options.require_auth else resolve,
        writes_enabled=options.writes_enabled,
        lifespan=lifespan,
    )
    # Reachable for tests and for a future readiness field, without making
    # the schedule a global.
    app.state.collector_schedule = schedule
    add_health_routes(
        app,
        context_factory=context,
        info=ServerInfo(
            writes_enabled=options.writes_enabled,
            auth_required=options.require_auth,
        ),
        api_prefix=API_PREFIX,
        mcp_path=MCP_PATH,
    )
    add_mcp_route(app, registry=active_registry, resolve=resolve, path=MCP_PATH)
    # Outermost, so the line it writes covers everything inside it — routing,
    # authentication, the error handlers, and the requests that never reach a
    # route at all. A 404 that took four seconds is a fact worth having.
    app.add_middleware(
        RequestLogMiddleware,
        settings=AccessLogSettings(
            enabled=resolved_config.log_requests,
            slow_request_ms=resolved_config.log_slow_request_ms,
            quiet_paths=tuple(resolved_config.log_quiet_paths),
        ),
    )
    return app


def run(  # pragma: no cover - exercised by running the server, not by tests
    options: ServeOptions, *, config: VogtConfig | None = None
) -> None:
    """Start the listener."""
    import uvicorn

    resolved = config if config is not None else load_config()
    configure_logging(level=resolved.log_level, fmt=resolved.log_format)
    app = build_server(options, config=resolved)
    log.info(
        "serving",
        extra={
            "vogt": {
                "version": __version__,
                "bind": f"{options.host}:{options.port}",
                "scheme": options.scheme,
                "auth": options.require_auth,
                "writes": options.writes_enabled,
            }
        },
    )
    uvicorn.run(
        app,
        host=options.host,
        port=options.port,
        ssl_certfile=None if options.tls_cert is None else str(options.tls_cert),
        ssl_keyfile=None if options.tls_key is None else str(options.tls_key),
        # Ours is already installed and carries the request id; uvicorn's
        # dictConfig would replace it with handlers that do not.
        log_config=None,
        # Replaced by `RequestLogMiddleware`, which knows the duration.
        access_log=False,
        log_level=resolved.log_level,
    )
