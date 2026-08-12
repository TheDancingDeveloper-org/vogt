"""The application layer — the only layer adapters may call.

CLI, REST and MCP are thin adapters over the use-cases here. Anything an
adapter does that this layer cannot is a parity bug by construction
(DESIGN §4).
"""

from __future__ import annotations
