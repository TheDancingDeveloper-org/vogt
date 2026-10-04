"""A small client for a peer Vogt instance's diagnostics.

A dev instance asking prod "what are you running, and are you well" — and
the reverse — through the peer's own REST surface, so an agent confirming a
deploy needs neither tailnet nor orchestrator access. Built on `urllib` for
the reason the engine client gives: an optional adapter making one request
must not add a dependency to the core.

The token is read from a *file* and never from argv or a URL. What the peer
answers is untrusted data from another process: it is returned as parsed
JSON, never interpreted.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from vogt.adapters.engine.client import Transport
from vogt.errors import VogtError

USER_AGENT = "vogt"
DEFAULT_TIMEOUT_SECONDS = 10

#: Where a peer serves its diagnostics, relative to its REST base.
DIAGNOSTICS_PATH = "/instance/diagnostics"

#: Ceiling on a peer's answer. Diagnostics are a few kilobytes; anything
#: larger is not a diagnostics answer and is not worth holding.
MAX_RESPONSE_BYTES = 512 * 1024


class PeerUnavailable(VogtError):
    """The peer could not be reached, refused, or answered nonsense."""

    code = "peer_unavailable"
    http_status = 502

    def __init__(self, status: str, message: str) -> None:
        super().__init__(message)
        #: `unreachable`, `refused` or `invalid_response`.
        self.status = status


@dataclass(frozen=True)
class PeerClient:
    """Access to one peer instance's REST surface."""

    base_url: str
    token: str | None = None
    transport: Transport | None = None
    timeout: int = DEFAULT_TIMEOUT_SECONDS

    @classmethod
    def from_config(
        cls,
        url: str | None,
        token_file: Path | None,
        *,
        transport: Transport | None = None,
    ) -> PeerClient | None:
        """Build a client, or `None` when no peer is configured."""
        if not url or not url.strip():
            return None
        token: str | None = None
        if token_file is not None:
            resolved = Path(token_file).expanduser()
            if resolved.is_file():
                token = resolved.read_text(encoding="utf-8").strip() or None
        return cls(base_url=url.strip().rstrip("/"), token=token, transport=transport)

    def diagnostics(self, *, log_lines: int) -> dict[str, Any]:
        """The peer's own `instance.diagnostics`, without asking it for *its* peer."""
        url = f"{self.base_url}{DIAGNOSTICS_PATH}?peer=false&log_lines={log_lines}"
        headers = {"Accept": "application/json", "User-Agent": USER_AGENT}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        status, body = self._fetch(url, headers)
        if status in (401, 403):
            msg = (
                f"the peer refused the request ({status}): the token is missing, "
                "wrong, or lacks the read scope"
            )
            raise PeerUnavailable("refused", msg)
        if status >= 400:
            msg = f"the peer answered {status} for {DIAGNOSTICS_PATH}"
            raise PeerUnavailable("refused" if status < 500 else "unreachable", msg)
        try:
            payload = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, ValueError) as exc:
            msg = "the peer's answer is not JSON"
            raise PeerUnavailable("invalid_response", msg) from exc
        if not isinstance(payload, dict):
            msg = "the peer's answer is not a JSON object"
            raise PeerUnavailable("invalid_response", msg)
        return payload

    def _fetch(self, url: str, headers: dict[str, str]) -> tuple[int, bytes]:
        if self.transport is not None:
            return self.transport(url, headers, b"", "GET")
        request = urllib.request.Request(url, headers=headers, method="GET")
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return int(response.status), bytes(response.read(MAX_RESPONSE_BYTES))
        except urllib.error.HTTPError as exc:  # pragma: no cover - network shape
            return int(exc.code), b""
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            msg = f"the peer is not answering: {exc}"
            raise PeerUnavailable("unreachable", msg) from exc


__all__ = ["PeerClient", "PeerUnavailable"]
