"""Group 9 — the PWA loads, and a session attaches over WebSocket."""

from __future__ import annotations

import re
from urllib.parse import urlsplit

from ..client import WebSocketClient
from ..harness import (
    Case,
    Check,
    Context,
    SkipCase,
    Token,
    TransportError,
    failed,
    passed,
)


def _get(ctx: Context, path: str) -> tuple[int, dict[str, str], bytes]:
    response = ctx.client.http.request("GET", path, token=False)
    return response.status, response.headers, response.body


def index_loads(ctx: Context) -> Check:
    status, headers, body = _get(ctx, "/")
    text = body.decode("utf-8", "replace")
    if status != 200:
        return failed(f"/ -> {status}")
    if "<title>Vogt" not in text:
        return failed("/ does not look like the Vogt PWA (no <title>Vogt)")
    if "text/html" not in headers.get("content-type", ""):
        return failed(f"/ is {headers.get('content-type')!r}, not HTML")
    return passed("/ returns 200 HTML with the Vogt title")


def pwa_assets(ctx: Context) -> Check:
    problems: list[str] = []
    _status, _, body = _get(ctx, "/")
    index = body.decode("utf-8", "replace")
    for path in ("/manifest.webmanifest", "/sw.js"):
        code, _headers, _payload = _get(ctx, path)
        if code != 200:
            problems.append(f"{path} -> {code}")
    script = re.search(r'<script[^>]+src="([^"]+)"', index)
    if not script:
        problems.append("index has no module script")
    else:
        code, headers, _payload = _get(ctx, script.group(1))
        if code != 200:
            problems.append(f"{script.group(1)} -> {code}")
        if "javascript" not in headers.get("content-type", ""):
            problems.append(f"{script.group(1)} is {headers.get('content-type')!r}")
    if problems:
        return failed("; ".join(problems))
    return passed("index, manifest, service worker and the JS bundle all answer 200")


def no_mixed_content(ctx: Context) -> Check:
    _status, _, body = _get(ctx, "/")
    index = body.decode("utf-8", "replace")
    references = re.findall(r'(?:src|href)="(http://[^"]+)"', index)
    # Follow the page's own scripts and stylesheets so a plain-http URL baked
    # into the bundle is caught too, not only one in index.html.
    assets = re.findall(r'(?:src|href)="(/[^"]+\.(?:js|css))"', index)
    for asset in assets[:6]:
        code, _headers, payload = _get(ctx, asset)
        if code == 200:
            references += re.findall(
                r'"http://[^"]+"', payload.decode("utf-8", "replace")
            )
    if references:
        return failed(f"the PWA references plain-http assets: {references[:5]}")
    return passed(
        f"no plain-http asset references in index or {len(assets[:6])} local asset(s)"
    )


def websocket_attach(ctx: Context) -> Check:
    ctx.require_sessions()
    engine_id = str(ctx.state.get("sessions.codex.engine") or "")
    if not engine_id:
        raise SkipCase("no engine session id from S1")
    parts = urlsplit(ctx.options.base_url or "")
    scheme = "wss" if parts.scheme == "https" else "ws"
    url = f"{scheme}://{parts.netloc}/api/sessions/{engine_id}/attach"
    token = ctx.options.read_token or ctx.client.http.token
    if token is None:
        raise SkipCase("no token is loaded")
    try:
        with WebSocketClient.connect(url, token=Token(token.value)) as socket:
            socket.send_text('{"type":"auth","token":"' + token.value + '"}')
            socket.send_text('{"type":"resize","cols":120,"rows":40}')
            frame = socket.recv_text(timeout=15)
    except TransportError as exc:
        return failed(f"websocket attach failed: {exc}")
    if not frame:
        return failed("websocket attach returned an empty frame")
    return passed(f"attached to {engine_id}; first frame: {frame[:160]!r}")


CASES = [
    Case("U1", "pwa", "U1", "critical", "the PWA index loads", index_loads),
    Case(
        "U2",
        "pwa",
        "U1",
        "high",
        "manifest, service worker and bundle load",
        pwa_assets,
    ),
    Case(
        "U3",
        "pwa",
        "U1",
        "medium",
        "the PWA has no mixed-content references",
        no_mixed_content,
    ),
    Case(
        "U4",
        "pwa",
        "U2",
        "high",
        "a websocket attaches to a live session",
        websocket_attach,
    ),
]
