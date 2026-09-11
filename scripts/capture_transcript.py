#!/usr/bin/env python3
"""Capture the raw PTY output of a command to a file.

The fidelity fixtures under web/tests/fixtures/transcripts/ are ideally captured
from a real dev-stack session via `GET /api/history/:id/download`. When no stack
is reachable (an agent session cannot attach to prod), this runs a real program
under a PTY on this box instead, so the bytes carry genuine escape sequences,
SGR colour, cursor moves, alt-screen switches and UTF-8 — exactly what the
parser-fidelity tests exercise — rather than anything synthetic.

Run the result through scripts/sanitise_transcript.py before committing it.

Usage:
    capture_transcript.py OUTPUT.bin [--max-bytes N] -- CMD [ARG ...]
"""

from __future__ import annotations

import argparse
import contextlib
import os
import pty
import select
import sys
from pathlib import Path


def capture(cmd: list[str], max_bytes: int, cols: int = 120, rows: int = 40) -> bytes:
    pid, fd = pty.fork()
    if pid == 0:  # child
        os.environ["TERM"] = "xterm-256color"
        os.environ["COLUMNS"] = str(cols)
        os.environ["LINES"] = str(rows)
        try:
            os.execvp(cmd[0], cmd)
        except FileNotFoundError:
            os._exit(127)
    # parent: set window size then drain until EOF or the byte cap.
    try:
        import fcntl
        import struct
        import termios

        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    except Exception:
        pass

    chunks: list[bytes] = []
    total = 0
    while total < max_bytes:
        try:
            readable, _, _ = select.select([fd], [], [], 10.0)
        except OSError:
            break
        if not readable:
            break
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        chunks.append(data)
        total += len(data)
    with contextlib.suppress(OSError):
        os.close(fd)
    with contextlib.suppress(OSError):
        os.waitpid(pid, 0)
    return b"".join(chunks)[:max_bytes]


def main(argv: list[str]) -> int:
    # Split on the first "--" ourselves: argparse.REMAINDER would greedily
    # swallow --max-bytes into the command.
    if "--" in argv:
        sep = argv.index("--")
        pre, cmd = argv[:sep], argv[sep + 1 :]
    else:
        pre, cmd = argv, []
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("output")
    ap.add_argument("--max-bytes", type=int, default=2 * 1024 * 1024)
    args = ap.parse_args(pre)
    if not cmd:
        ap.error("a command is required after --")
    data = capture(cmd, args.max_bytes)
    Path(args.output).write_bytes(data)
    print(f"capture_transcript: {len(data)} bytes -> {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
