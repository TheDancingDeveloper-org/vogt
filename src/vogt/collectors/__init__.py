"""Collectors: the things that look, and the framework that records looking.

Two rules shape everything here.

**Scope is the registered project list** (FR-G15). Nothing crawls the
filesystem for repositories, nothing maintains a list of unregistered
candidates, and a collector is never handed a path that is not a project's
root. Discovery is where this class of tool accumulates fiddly machinery —
deciding what counts as a project, tuning exclusions until the noise stops —
and r3 deleted it deliberately (DESIGN §2.1).

**Collectors never touch declared data** (FR-O2, NFR-I2). They return
findings; the framework appends them to the observed store and writes the
coverage record. The worst a broken collector can do is produce stale or
missing evidence, visible as such.
"""

from __future__ import annotations

from vogt.collectors.base import (
    Collector,
    CollectorContext,
    CollectorError,
    Finding,
    finding,
)
from vogt.collectors.registry import (
    CollectorRegistry,
    core_collectors,
    is_on_demand,
)
from vogt.collectors.sweeper import Sweeper

__all__ = [
    "Collector",
    "CollectorContext",
    "CollectorError",
    "CollectorRegistry",
    "Finding",
    "Sweeper",
    "core_collectors",
    "finding",
    "is_on_demand",
]
