"""Reading the PR ↔ work-item edge a forge already records (#284).

A pull request nearly always says which work it does, in two places nobody
has to be asked to fill in: the **closing keywords** GitHub itself acts on
(`Closes #12`, `fixes owner/repo#9`) and the **branch name** a person picked
when they started (`gh-12-…`, `wi-9/…`). This module reads both into a single
list of `ParsedEdge`, so the sync can record an observed `implemented_by` edge
from the PR to the item it implements.

Observed-first, additive, forward-only (#287): the edge is *read*, never
declared by hand, and it reports rather than enforces — it does not block the
item's completion the way `depends_on` does. Every edge carries its
**provenance** ("from PR body", "from PR title", "from branch name") so a
reader can see why Vogt thinks the two are the same stream of work.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

#: GitHub's closing keywords, every tense, case-insensitive. A keyword, an
#: optional colon, then `#n` or the cross-repo `owner/repo#n`. `\b` anchors
#: the keyword so `encloses #3` is not a false positive.
_CLOSING = re.compile(
    r"\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b"
    r"\s*:?\s*"
    r"(?:(?P<owner>[A-Za-z0-9._-]+)/(?P<repo>[A-Za-z0-9._-]+))?"
    r"#(?P<number>\d+)",
    re.IGNORECASE,
)

#: The branch-naming shapes this build understands, until #283 lands a
#: configurable `branch_patterns`. Read off the fixture manifest's
#: `branch_patterns` (`gh-<n>-<slug>`, `feature/gh-<n>-<slug>`,
#: `wi-<n>/<slug>`): a known prefix, a separator, then the issue number.
# TODO(#283): source the prefixes/shapes from `config.branch_patterns` once
# #283 merges, and pass the compiled pattern in rather than defaulting here.
DEFAULT_BRANCH_PATTERN = re.compile(
    r"(?:^|/)(?:gh|wi|issue|feat|feature|fix|bug)[-/](?P<number>\d+)\b",
    re.IGNORECASE,
)

FROM_BODY = "from PR body"
FROM_TITLE = "from PR title"
FROM_BRANCH = "from branch name"


@dataclass(frozen=True)
class ParsedEdge:
    """One observed "this PR implements work item N" edge.

    `owner`/`repo` are set only for a cross-repo reference; `None` means the
    same repository as the PR, which the caller resolves against the PR's own
    `RepoRef`.
    """

    number: int
    provenance: str
    owner: str | None = None
    repo: str | None = None

    @property
    def _identity(self) -> tuple[str | None, str | None, int]:
        return (self.owner, self.repo, self.number)


def _closing_edges(text: str, provenance: str) -> list[ParsedEdge]:
    edges: list[ParsedEdge] = []
    for match in _CLOSING.finditer(text or ""):
        edges.append(
            ParsedEdge(
                number=int(match.group("number")),
                provenance=provenance,
                owner=match.group("owner"),
                repo=match.group("repo"),
            )
        )
    return edges


def _branch_edge(branch: str | None, pattern: re.Pattern[str]) -> list[ParsedEdge]:
    if not branch:
        return []
    match = pattern.search(branch)
    if match is None:
        return []
    return [ParsedEdge(number=int(match.group("number")), provenance=FROM_BRANCH)]


def parse_edges(
    *,
    title: str | None,
    body: str | None,
    branch: str | None,
    branch_pattern: re.Pattern[str] = DEFAULT_BRANCH_PATTERN,
) -> list[ParsedEdge]:
    """Every edge this PR asserts, deduplicated, most-explicit provenance first.

    Body closing keywords are read first, then the title's, then the branch —
    so when the same target turns up in more than one place the recorded
    provenance is the most deliberate one. Returned in discovery order; a PR
    that names no work yields an empty list rather than a guess.
    """
    candidates = (
        _closing_edges(body or "", FROM_BODY)
        + _closing_edges(title or "", FROM_TITLE)
        + _branch_edge(branch, branch_pattern)
    )
    seen: set[tuple[str | None, str | None, int]] = set()
    edges: list[ParsedEdge] = []
    for edge in candidates:
        if edge._identity in seen:
            continue
        seen.add(edge._identity)
        edges.append(edge)
    return edges


__all__ = [
    "DEFAULT_BRANCH_PATTERN",
    "FROM_BODY",
    "FROM_BRANCH",
    "FROM_TITLE",
    "ParsedEdge",
    "parse_edges",
]
