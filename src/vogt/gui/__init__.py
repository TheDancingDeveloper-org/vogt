"""The GUI: static assets, and the path they are served from (FR-U1, FR-U2).

Deliberately buildless. `DESIGN.md` §10 said "React SPA"; this is plain ES
modules with no bundler, and the reason is packaging rather than taste. Vogt
installs as a Python wheel and ships as one image. A framework build means
either a Node toolchain in the wheel build — so `pip install vogt` needs npm
present — or committed build output, which is a generated artifact in version
control that nothing verifies. Neither is worth it for six views over an API
that already exists.

What the design actually required survives intact: the GUI consumes only the
public REST surface, adds no capability of its own, and is served from the
same single port. Those are tested in `tests/test_gui.py`, against this
directory's real contents.
"""

from __future__ import annotations

from pathlib import Path

#: Where the served assets live. Package data, so it is present in the wheel
#: and in the image without a separate copy step.
STATIC_ROOT = Path(__file__).parent / "static"

#: The GUI's mount point. `/api` stays the API's, `/mcp` the MCP transport's,
#: and the health endpoints keep their own paths (FR-A7).
GUI_PREFIX = "/ui"

__all__ = ["GUI_PREFIX", "STATIC_ROOT"]
