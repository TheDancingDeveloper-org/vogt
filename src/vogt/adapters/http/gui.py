"""Serving the GUI from the same single port (FR-U1, NFR-D4).

The GUI is static files and nothing else. It gets no route of its own, no
server-side rendering, and no endpoint that exists to serve it — every answer
it shows comes from `/api`, which is what keeps the parity rule (FR-A1) true
by construction rather than by review.

Mounted at `/ui`, with `/` redirecting there. `/api`, `/mcp` and the health
paths keep their own prefixes, so adding the GUI cannot shadow them.
"""

from __future__ import annotations

from fastapi import FastAPI
from fastapi.responses import RedirectResponse
from fastapi.staticfiles import StaticFiles

from vogt.gui import GUI_PREFIX, STATIC_ROOT


def mount_gui(app: FastAPI) -> bool:
    """Mount the GUI, or don't, and say which.

    Returns `False` when the assets are absent. That is a real case — an
    editable checkout with the package data not yet in place — and the service
    must still serve the API rather than failing to start. The GUI is a
    surface over the API, so the API not needing it is the correct dependency
    direction.
    """
    if not (STATIC_ROOT / "index.html").is_file():
        return False

    app.mount(
        GUI_PREFIX,
        StaticFiles(directory=STATIC_ROOT, html=True),
        name="gui",
    )

    @app.get("/", include_in_schema=False)
    async def _root() -> RedirectResponse:
        """Send a browser at the root to the GUI.

        Excluded from the schema deliberately: this is a convenience for
        humans, not an operation, and an operation is the only thing that
        belongs in the OpenAPI document (FR-A4).
        """
        return RedirectResponse(url=f"{GUI_PREFIX}/")

    return True


__all__ = ["mount_gui"]
