"""Vogt's test suite.

A package rather than a loose directory so that `tests.conftest` has one
unambiguous module name — mypy checks these files under `--strict` too
(NFR-Q1), and it refuses to type-check a file reachable by two names.
"""

from __future__ import annotations
