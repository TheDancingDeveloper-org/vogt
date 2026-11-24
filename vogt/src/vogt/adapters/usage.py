"""Where the usage board's evidence lives (WI-2040): account transcripts on
disk and LiteLLM's `/key/info`.

The reading is `vogt.core.usage_board`; this module only fetches text. Reads
are bounded: the newest few transcripts an account wrote in the last week,
the tail of each, so a large history costs a request nothing.
"""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
from collections.abc import Callable
from pathlib import Path

#: How many of an account's newest transcripts are read, and how much of each.
MAX_FILES = 20
TAIL_BYTES = 512 * 1024
#: Transcripts older than this say nothing about a limit that still binds.
MAX_AGE_SECONDS = 7 * 24 * 3600
KEY_INFO_TIMEOUT_SECONDS = 10

#: `(url, key) -> (status, body)`; injectable so tests make no request.
KeyInfoFetch = Callable[[str, str], tuple[int, bytes]]


def transcript_tails(agent: str, directory: Path) -> list[str]:
    """The tails of the account's newest transcripts, newest first."""
    root = directory.expanduser() / ("projects" if agent == "claude" else "sessions")
    if not root.is_dir():
        return []
    cutoff = time.time() - MAX_AGE_SECONDS
    found: list[tuple[float, Path]] = []
    for path in root.rglob("*.jsonl"):
        try:
            mtime = path.stat().st_mtime
        except OSError:
            continue
        if mtime >= cutoff:
            found.append((mtime, path))
    found.sort(reverse=True)
    tails: list[str] = []
    for _mtime, path in found[:MAX_FILES]:
        try:
            with path.open("rb") as handle:
                size = handle.seek(0, 2)
                handle.seek(max(0, size - TAIL_BYTES))
                tails.append(handle.read().decode("utf-8", errors="replace"))
        except OSError:
            continue
    return tails


def fetch_key_info(url: str, key: str) -> tuple[int, bytes]:
    """GET `<url>/key/info` authenticated as the key itself."""
    request = urllib.request.Request(
        url.rstrip("/") + "/key/info",
        headers={"Authorization": f"Bearer {key}", "Accept": "application/json"},
    )
    try:
        with urllib.request.urlopen(
            request, timeout=KEY_INFO_TIMEOUT_SECONDS
        ) as response:
            return response.status, response.read()
    except urllib.error.HTTPError as exc:
        return exc.code, exc.read()


def key_info(
    url: str, key: str, fetch: KeyInfoFetch = fetch_key_info
) -> tuple[dict[str, object] | None, str | None]:
    """The parsed `/key/info` answer, or why there is none."""
    try:
        status, body = fetch(url, key)
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        return None, f"LiteLLM unreachable: {exc}"
    if status >= 400:
        return None, f"LiteLLM /key/info answered {status}"
    try:
        parsed = json.loads(body)
    except (json.JSONDecodeError, UnicodeDecodeError):
        return None, "LiteLLM /key/info did not answer JSON"
    if not isinstance(parsed, dict):
        return None, "LiteLLM /key/info did not answer a JSON object"
    return parsed, None
