#!/usr/bin/env python3
"""Prove codex-acp answers an ACP initialize (WI-1633).

`--version` only shows the package is installed. The adapter starts a Codex
app-server child and treats stdin closing as shutdown, so a one-line pipe
(`printf … | codex-acp`) exits 0 with nothing on stdout whenever that child
dies before the reply is written — the shape of a wrapper that passes
`--no-daemon` after `app-server`. This keeps stdin open until the first
JSON-RPC result, then closes it.
"""

from __future__ import annotations

import json
import os
import select
import subprocess
import sys


def main() -> int:
    binary = sys.argv[1] if len(sys.argv) > 1 else "codex-acp"
    request = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "vogt-acp-smoke", "version": "0"},
        },
    }
    process = subprocess.Popen(
        [binary],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    assert process.stdin is not None and process.stdout is not None
    process.stdin.write((json.dumps(request) + "\n").encode())
    process.stdin.flush()
    deadline_reads = 20
    buf = b""
    while deadline_reads:
        ready, _, _ = select.select([process.stdout], [], [], 1)
        deadline_reads -= 1
        if not ready:
            continue
        chunk = os.read(process.stdout.fileno(), 65536)
        if not chunk:
            break
        buf += chunk
        if b"\n" in buf:
            break
    process.stdin.close()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
    line = buf.split(b"\n", 1)[0].strip()
    try:
        message = json.loads(line)
    except json.JSONDecodeError:
        err = b""
        if process.stderr is not None:
            err = process.stderr.read(800)
        sys.stderr.write(
            f"codex-acp initialize returned no JSON-RPC result "
            f"(exit {process.returncode}): {err.decode(errors='replace')}\n"
        )
        return 1
    result = message.get("result") if isinstance(message, dict) else None
    if not isinstance(result, dict) or message.get("id") != 1:
        sys.stderr.write(f"codex-acp initialize was not a result for id 1: {line!r}\n")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
