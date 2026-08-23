"""The first-run install surface (#292) — unauthenticated by construction.

Two routes, mounted beside the health probes rather than generated from the
operation registry, and the difference is the point: every registry route
runs behind `authorize_request`, and these must answer a browser that holds
no credential yet. They are not a hole in the auth model — the bootstrap
answers only while the token store is empty (`services/install.py` explains
why that is acceptable on a loopback-published port), and the status route
states a single boolean an unauthenticated caller could infer anyway by
trying to bootstrap.

Optional hardening that is deliberately not v1 — a boot code printed to the
server log, or refusing bootstraps that did not arrive over loopback — would
be a check added here, in front of the service call, so the service keeps
owning the one invariant that matters (zero tokens, checked in the write
transaction).
"""

from __future__ import annotations

from collections.abc import Callable

from fastapi import APIRouter, FastAPI

from vogt.application.context import AppContext
from vogt.application.models import (
    InstallBootstrapParams,
    InstallBootstrapResult,
    InstallStatusResult,
)
from vogt.application.services.install import install_bootstrap, install_status

ContextFactory = Callable[[], AppContext]

#: Under `/api` on purpose: the engine's front door proxies `/api/vogt/*` to
#: the core's `/api/*`, so the two-container stack reaches these at
#: `/api/vogt/install/...` without growing new proxy routes.
INSTALL_STATUS_PATH = "/api/install/status"
INSTALL_BOOTSTRAP_PATH = "/api/install/bootstrap"


def add_install_routes(app: FastAPI, *, context_factory: ContextFactory) -> None:
    """Mount the install-mode routes. Always mounted: on a loopback (no-auth)
    listener they are redundant rather than wrong, and a status route that
    exists only sometimes is harder for a wizard to reason about."""
    router = APIRouter()

    @router.get(
        INSTALL_STATUS_PATH, response_model=InstallStatusResult, tags=["install"]
    )
    async def status() -> InstallStatusResult:
        """Whether the first-run bootstrap is still open."""
        return install_status(context_factory())

    @router.post(
        INSTALL_BOOTSTRAP_PATH,
        response_model=InstallBootstrapResult,
        tags=["install"],
    )
    async def bootstrap(params: InstallBootstrapParams) -> InstallBootstrapResult:
        """Name the first operator and mint the first token — exactly once."""
        return install_bootstrap(context_factory(), params)

    app.include_router(router)
