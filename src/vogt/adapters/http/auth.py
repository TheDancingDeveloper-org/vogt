"""The password login — unauthenticated by construction.

One route, mounted beside the install surface rather than generated from the
operation registry, for the same reason: every registry route runs behind
`authorize_request`, and a login must answer a browser that holds no
credential yet. It is not a hole in the auth model. The service verifies a
password with a slow salted hash, throttles a username that keeps failing,
records every attempt in `auth_decisions`, and mints a *session* — a token
row like any other, bound to the person's own actor and expiring on its own.

`auth.logout` and `auth.whoami` are ordinary registry operations: both act on
the credential the call arrived with, which only an authenticated route has.
"""

from __future__ import annotations

from collections.abc import Callable

from fastapi import APIRouter, FastAPI

from vogt.application.context import AppContext
from vogt.application.models import LoginParams, LoginResult
from vogt.application.services.auth import login

ContextFactory = Callable[[], AppContext]

#: Under `/api` on purpose, like the install routes: the engine's front door
#: forwards it untouched, so a browser behind the door logs in at the same
#: path it would against the core directly.
LOGIN_PATH = "/api/auth/login"


def add_auth_routes(app: FastAPI, *, context_factory: ContextFactory) -> None:
    """Mount the login route. Always mounted: on a loopback (no-auth) listener
    a session is redundant rather than wrong."""
    router = APIRouter()

    @router.post(LOGIN_PATH, response_model=LoginResult, tags=["auth"])
    async def sign_in(params: LoginParams) -> LoginResult:
        """Exchange a username and password for a session token."""
        return login(context_factory(), params)

    app.include_router(router)
