"""The context every use-case runs in.

Carries the configuration, both stores, the authenticated principal, and the
injectable clock and id factory. Adapters build one of these and hand it to
the registry; they never reach past it.
"""

from __future__ import annotations

from dataclasses import dataclass

from vogt.adapters.git import Cloner, clone_repository
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


def build_context(
    *,
    config: VogtConfig | None = None,
    principal: Principal | None = None,
    clock: Clock = utc_now,
    id_factory: IdFactory = new_id,
    cloner: Cloner = clone_repository,
) -> AppContext:
    """Build a context over the SQLite backend.

    The principal is passed in by the adapter that authenticated it — FR-S2
    means it is never read from request data, and this signature is where
    that rule is kept honest: there is nowhere for a caller to inject one.
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
    )
