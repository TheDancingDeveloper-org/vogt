"""The optional GitHub adapter.

Optional is a statement about *dependency*, not about sequencing (DESIGN
§1.1). Read-only collectors ship here at M2 because the estate this tool
governs keeps most of its real work on GitHub, and an MVP populated only by
source markers would be a demo rather than a daily driver. What stays
optional is that nothing in the core requires this package: the whole test
suite runs forge-less, and an absent adapter yields "not collected", never
failure (NFR-PO1, NFR-PO2).

Write-back, historical backfill and forge-derived drift are M5. Nothing here
mutates anything upstream — these collectors only ever read.
"""

from __future__ import annotations

from vogt.adapters.github.client import GitHubClient, GitHubUnavailable
from vogt.adapters.github.collectors import github_collectors

__all__ = ["GitHubClient", "GitHubUnavailable", "github_collectors"]
