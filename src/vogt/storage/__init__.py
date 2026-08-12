"""Storage: the seam between the application layer and a database.

Two stores with strictly different write disciplines (`SCHEMA.md` §1):
`declared` is authoritative, mutable, audited and revisioned; `observed` is
append-only evidence. Cross-store joins happen in the application layer, not
in SQL, so the two stay independently portable and backupable.
"""

from __future__ import annotations
