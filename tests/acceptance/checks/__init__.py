"""Every acceptance check module, imported in report order."""

from __future__ import annotations

from ..harness import Case
from . import (
    auth,
    budget,
    health,
    local,
    mcp_surface,
    persistence,
    providers,
    pwa,
    security,
    sessions,
    tracker,
    work,
)

MODULES = (
    health,
    auth,
    mcp_surface,
    work,
    sessions,
    providers,
    tracker,
    security,
    pwa,
    persistence,
    budget,
    local,
)


def all_cases() -> list[Case]:
    cases: list[Case] = []
    for module in MODULES:
        cases.extend(module.CASES)
    return cases
