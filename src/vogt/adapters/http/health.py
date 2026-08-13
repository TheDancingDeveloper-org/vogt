"""Health, version, and connection info — plain HTTP, always (FR-A7).

`DEPLOYMENT.md` §1 and §4.4 exist because of a specific cadastre failure:
its MCP port served *only* `/mcp`, so `/health/ready` and `/version` answered
`-32004` and "is it up?" required an MCP client. Compose healthchecks, curl
and uptime monitors all broke.

So: **any port that serves MCP also serves plain HTTP health and version**,
none of it authenticated, and none of it pinning an MCP protocol version.
A probe that speaks `initialize` is a probe that breaks when the protocol
moves.
"""

from __future__ import annotations

from dataclasses import dataclass

from fastapi import APIRouter, FastAPI, Response
from pydantic import BaseModel

from vogt import __version__
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS
from vogt.application.context import AppContext
from vogt.errors import VogtError


class Liveness(BaseModel):
    status: str = "ok"
    version: str = __version__


class Readiness(BaseModel):
    """Ready means the stores are migrated and answering.

    Readiness gates traffic until migration completes (NFR-I3,
    `DEPLOYMENT.md` §5), which is why this reports schema versions rather
    than a bare boolean: a red probe should say what is wrong.
    """

    status: str
    detail: str | None = None
    declared_schema_version: int = 0
    observed_schema_version: int = 0
    instance_id: str | None = None


class VersionInfo(BaseModel):
    version: str = __version__
    name: str = "vogt"


class ConnectionInfo(BaseModel):
    """What a client needs to connect, from the server itself (DEPLOY §4.3).

    One generated document rather than per-client hand-copied config: the
    `:18081` incident began as a URL that lived in seven places and was
    retired in one.
    """

    name: str = "vogt"
    version: str = __version__
    #: Where clients reach this instance. `None` when nobody has configured
    #: one — the server binds a container port and is published elsewhere, so
    #: this is the one connection fact it cannot infer (FR-A8). Reported as
    #: absent rather than guessed: from a client, a wrong URL and an
    #: unreachable one look the same.
    url: str | None = None
    api_path: str
    mcp_path: str
    health_path: str
    supported_mcp_protocol_versions: list[str]
    authentication: str
    writes_enabled: bool


@dataclass(frozen=True)
class ServerInfo:
    """What `serve` was started with, for the endpoints to report."""

    writes_enabled: bool = True
    auth_required: bool = False


def add_health_routes(
    app: FastAPI,
    *,
    context_factory: object,
    info: ServerInfo,
    api_prefix: str,
    mcp_path: str,
) -> None:
    """Mount the unauthenticated probe endpoints."""
    router = APIRouter()

    @router.get("/health/live", response_model=Liveness, tags=["health"])
    async def live() -> Liveness:
        """Liveness: the process is up. Deliberately cheap and storage-free."""
        return Liveness()

    @router.get("/health/ready", tags=["health"])
    async def ready(response: Response) -> Readiness:
        """Readiness: the stores are migrated and answering."""
        ctx: AppContext = context_factory()  # type: ignore[operator]
        try:
            declared = ctx.declared.schema_version()
            observed = ctx.observed.schema_version()
            with ctx.declared.read() as view:
                instance_id = view.instance_id()
        except VogtError as exc:
            response.status_code = 503
            return Readiness(status="not_ready", detail=str(exc))
        except Exception as exc:  # a probe must answer, not raise
            response.status_code = 503
            return Readiness(status="not_ready", detail=f"{type(exc).__name__}: {exc}")
        return Readiness(
            status="ready",
            declared_schema_version=declared,
            observed_schema_version=observed,
            instance_id=instance_id,
        )

    @router.get("/version", response_model=VersionInfo, tags=["health"])
    async def version() -> VersionInfo:
        return VersionInfo()

    @router.get("/connection-info", response_model=ConnectionInfo, tags=["health"])
    async def connection_info() -> ConnectionInfo:
        ctx: AppContext = context_factory()  # type: ignore[operator]
        return ConnectionInfo(
            url=(ctx.config.public_url or "").rstrip("/") or None,
            api_path=api_prefix,
            mcp_path=mcp_path,
            health_path="/health/ready",
            supported_mcp_protocol_versions=list(SUPPORTED_PROTOCOL_VERSIONS),
            authentication="bearer token" if info.auth_required else "none (loopback)",
            writes_enabled=info.writes_enabled,
        )

    app.include_router(router)
