"""The context every use-case runs in.

Carries the configuration, both stores, the authenticated principal, and the
injectable clock and id factory. Adapters build one of these and hand it to
the registry; they never reach past it.
"""

from __future__ import annotations

from dataclasses import dataclass, field

from vogt.adapters.engine import EngineClient
from vogt.adapters.git import Cloner, Pusher, clone_repository, push_branch
from vogt.adapters.github.client import Transport
from vogt.application.identity import PublicIdentity, identity_from_config
from vogt.config import VogtConfig, load_config
from vogt.core.clock import Clock, utc_now
from vogt.core.ids import IdFactory, new_id
from vogt.core.principal import Principal, local_principal
from vogt.storage.interface import DeclaredStore, ObservedStore
from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.observed import SqliteObservedStore


@dataclass(frozen=True)
class AppContext:
    """One request's worth of application state."""

    config: VogtConfig
    declared: DeclaredStore
    observed: ObservedStore
    principal: Principal
    clock: Clock
    id_factory: IdFactory
    #: How `project.import` obtains a checkout (FR-P6). Injectable for the
    #: same reason the clock is: a use-case that shells out to `git` over the
    #: network is one the test suite could otherwise only run by having a
    #: network, and "works offline" is a property this product asserts rather
    #: than hopes for (NFR-PO2).
    cloner: Cloner = clone_repository
    #: How `forge.publish` pushes the local default branch (#182). Injectable
    #: for the same reason the cloner is; the real function is exercised
    #: against a local bare remote in the suite, and it can never force —
    #: the argv is built in one place and carries no force flag.
    pusher: Pusher = push_branch
    #: The session engine, or `None` when none is configured — which is the
    #: shape v1 shipped in and stays supported (FR-E9 read from this side).
    #: `None` is not an outage: the `session.*` operations say no engine is
    #: configured, and every other operation is unaffected.
    engine: EngineClient | None = None
    #: How the forge is spoken to, or `None` for the real network. Injectable
    #: for the same reason the cloner and engine are: linking a per-actor PAT
    #: validates it against the forge, and the write path builds a provider
    #: from a linked PAT — both would otherwise reach GitHub, which a test
    #: cannot depend on (#179). `None` is the deployed shape.
    forge_transport: Transport | None = None
    #: Where a client should reach this instance (FR-A8). Defaults to what the
    #: configuration says, which is the whole answer when this process is the
    #: door. Behind a front door the adapter resolves it per request from what
    #: the door states, because the door owns the address and the mount points
    #: and this process cannot see either — see `identity.py` for why that is
    #: not the rule FR-S2 forbids.
    public_identity: PublicIdentity = field(default_factory=PublicIdentity)


def build_context(
    *,
    config: VogtConfig | None = None,
    principal: Principal | None = None,
    clock: Clock = utc_now,
    id_factory: IdFactory = new_id,
    cloner: Cloner = clone_repository,
    pusher: Pusher = push_branch,
    engine: EngineClient | None = None,
    public_identity: PublicIdentity | None = None,
    forge_transport: Transport | None = None,
) -> AppContext:
    """Build a context over the SQLite backend.

    The principal is passed in by the adapter that authenticated it — FR-S2
    means it is never read from request data, and this signature is where
    that rule is kept honest: there is nowhere for a caller to inject one.

    `public_identity` is passed the same way and for a related reason, though
    it is a different kind of fact: an address grants nothing and says nothing
    about the caller, but it *is* resolved from the request behind a front
    door, so the resolution belongs to the adapter and the gate that permits
    it belongs to the configuration (`fronted`). Defaulting to the config's
    own answer keeps the core-only shape exactly as it was.
    """
    resolved_config = config if config is not None else load_config()
    return AppContext(
        config=resolved_config,
        declared=SqliteDeclaredStore(
            resolved_config.declared_db_path,
            clock=clock,
            id_factory=id_factory,
            synchronous=resolved_config.sqlite_synchronous,
        ),
        observed=SqliteObservedStore(
            resolved_config.observed_db_path,
            clock=clock,
            synchronous=resolved_config.sqlite_synchronous,
        ),
        principal=principal if principal is not None else local_principal(),
        clock=clock,
        id_factory=id_factory,
        cloner=cloner,
        pusher=pusher,
        forge_transport=forge_transport,
        # Injectable for the same reason the cloner is: a use-case that talks
        # to another process over HTTP is one the suite could otherwise only
        # exercise by running that process.
        engine=engine
        or EngineClient.from_config(
            resolved_config.engine_url, resolved_config.engine_token_file
        ),
        public_identity=(
            public_identity
            if public_identity is not None
            else identity_from_config(resolved_config)
        ),
    )
