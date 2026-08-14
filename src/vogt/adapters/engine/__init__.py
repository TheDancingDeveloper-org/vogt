"""Talking to the session engine (FR-E1, FR-E8).

The engine is the other half of the merged product — the Rust process that
owns PTYs, scrollback and activity state. Vogt asks it for four things and
nothing else: start a session, list sessions, describe one, stop one. That
list is the whole coupling, and keeping it that short is what makes the
two-process shape (NFR-D11) worth having rather than merely tolerable.

The adapter is optional in exactly the way the forge adapter is. No engine
configured means the `session.*` operations report that, and every other
operation is unaffected — FR-E9 read from Vogt's side.
"""

from vogt.adapters.engine.client import (
    EngineClient,
    EngineSession,
    EngineUnavailable,
)

__all__ = ["EngineClient", "EngineSession", "EngineUnavailable"]
