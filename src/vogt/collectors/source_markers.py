"""`source-markers` — TODOs and FIXMEs, and the reason most of them are noise.

The real estate this tool governs holds thousands of markers in source and
hundreds more in Markdown. An unfiltered observed-first backlog is several
thousand items deep on day one, mostly worthless, and the ranked global view
— the headline feature — is destroyed by its own input (DESIGN §3.6).

So this collector observes *every* marker it finds, and promotes only those
matching a configured pattern (default `TODO(vogt)` / `FIXME(vogt)`). The
rest stay queryable and counted; they simply do not claim to be work. That
inverts the default from "everything is work until dismissed" to "work is
what somebody marked as work", which is the only version that scales.
"""

from __future__ import annotations

import re
from collections.abc import Iterable
from pathlib import Path

from vogt.collectors.base import CollectorContext, Finding, finding, walk_project
from vogt.core.entities import Project

KIND_MARKER = "marker"

#: A marker is a *leading annotation*, not a mention. The pattern is anchored
#: so that only comment leaders, list bullets and whitespace may precede it.
#:
#: This is not fussiness. Sweeping this repository unanchored promoted 21
#: "markers", every one of which was documentation *about* the promotion
#: pattern — `docs/DESIGN.md` explaining that `TODO(vogt):` enters the
#: backlog, the generated `config.example.toml` listing the default patterns,
#: a Markdown table cell naming them. Vogt read its own description of markers
#: and filed it as work. Any project that documents its conventions would have
#: hit the same thing.
MARKER_PATTERN = re.compile(
    r"^[\s\-*+#/;%<!>=|]*"
    r"\b(?P<tag>TODO|FIXME|HACK|XXX)\b(?P<scope>\([^)]*\))?\s*:?\s*(?P<text>.*)"
)

#: A line longer than this is minified or generated; scanning it finds
#: nothing useful and costs a lot.
MAX_LINE_LENGTH = 500


class SourceMarkerCollector:
    """Markers in source, promoted by convention."""

    @property
    def name(self) -> str:
        return "source-markers"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        root = Path(project.root_path).expanduser()
        extensions = tuple(ctx.config.marker_file_extensions)
        promote_patterns = tuple(ctx.config.marker_promotion_patterns)
        exclusions = tuple(project.exclusions)

        for path in walk_project(root, exclusions=exclusions, extensions=extensions):
            try:
                text = path.read_text(encoding="utf-8", errors="strict")
            except (OSError, UnicodeDecodeError):
                # Binary or unreadable: not an error, just not a source file.
                continue
            relative = path.relative_to(root).as_posix()
            for number, line in enumerate(text.splitlines(), start=1):
                if len(line) > MAX_LINE_LENGTH:
                    continue
                match = MARKER_PATTERN.match(line)
                if match is None:
                    continue
                marker = match.group(0).strip()
                promoted = any(pattern in marker for pattern in promote_patterns)
                yield finding(
                    kind=KIND_MARKER,
                    subject_key=f"mark:{project.slug}/{relative}#L{number}",
                    project=project,
                    promoted=promoted,
                    payload={
                        "tag": match.group("tag"),
                        "scope": (match.group("scope") or "").strip("()") or None,
                        "text": match.group("text").strip()[:500],
                        "path": relative,
                        "line": number,
                        "promoted_by": (
                            next(
                                (p for p in promote_patterns if p in marker),
                                None,
                            )
                        ),
                    },
                )
