#!/usr/bin/env python3
"""Sanitise a captured terminal transcript before it is checked in as a fixture.

A transcript is raw PTY bytes -- escape sequences, colour, UTF-8, whatever a
program wrote. Captured from a real session it can carry things a fixture must
not: absolute home paths, a bearer token echoed onto a command line, an email
address, an API key in an environment dump. This pass rewrites those to inert
placeholders in place in the byte stream and then *asserts* that nothing
matching a secret pattern survives, so a fixture can never be committed with a
live credential in it.

Usage:
    sanitise_transcript.py INPUT.bin OUTPUT.bin
    sanitise_transcript.py --check INPUT.bin        # assert only, no write

Exit non-zero if a secret pattern remains after rewriting.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# (pattern, replacement) applied in order, over the raw bytes. Replacements are
# kept close to the original length so escape-sequence and line boundaries stay
# meaningful for the fidelity tests.
_REWRITES: list[tuple[re.Pattern[bytes], bytes]] = [
    # Bearer / auth tokens on a command line or in a header.
    (
        re.compile(rb"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]+"),
        rb"\1REDACTED_TOKEN",
    ),
    (
        re.compile(rb"(?i)(token[=:\s\"']+)[A-Za-z0-9._\-]{16,}"),
        rb"\1REDACTED_TOKEN",
    ),
    # Generic long secrets: sk-..., ghp_..., AKIA..., xoxb-..., JWTs.
    (re.compile(rb"sk-[A-Za-z0-9]{20,}"), rb"sk-REDACTED"),
    (re.compile(rb"gh[pousr]_[A-Za-z0-9]{20,}"), rb"ghx_REDACTED"),
    (re.compile(rb"AKIA[0-9A-Z]{16}"), rb"AKIAREDACTEDREDACT00"),
    (re.compile(rb"xox[baprs]-[A-Za-z0-9-]{10,}"), rb"xoxb-REDACTED"),
    (
        re.compile(
            rb"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"
        ),
        rb"eyJ.REDACTED.JWT",
    ),
    # Email addresses.
    (
        re.compile(rb"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),
        rb"user@example.invalid",
    ),
    # Absolute home paths -> a neutral placeholder.
    (re.compile(rb"/home/[A-Za-z0-9._\-]+"), rb"/home/user"),
    (re.compile(rb"/Users/[A-Za-z0-9._\-]+"), rb"/Users/user"),
]

# After rewriting, NONE of these may appear. If one does, the sanitiser failed
# to cover a case and the fixture is refused rather than committed with a leak.
_FORBIDDEN: list[re.Pattern[bytes]] = [
    re.compile(rb"(?i)bearer\s+[A-Za-z0-9._\-]{16,}"),
    re.compile(rb"sk-[A-Za-z0-9]{20,}"),
    re.compile(rb"gh[pousr]_[A-Za-z0-9]{20,}"),
    re.compile(rb"AKIA[0-9A-Z]{16}"),
    re.compile(rb"xox[baprs]-[A-Za-z0-9-]{10,}"),
    re.compile(rb"eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}"),
    # A home path for a real user (the placeholder "user" is allowed).
    re.compile(rb"/home/(?!user\b)[A-Za-z0-9._\-]+"),
    re.compile(rb"/Users/(?!user\b)[A-Za-z0-9._\-]+"),
]


def sanitise(data: bytes) -> bytes:
    for pattern, repl in _REWRITES:
        data = pattern.sub(repl, data)
    return data


def assert_clean(data: bytes) -> None:
    leaks = []
    for pattern in _FORBIDDEN:
        match = pattern.search(data)
        if match:
            leaks.append(f"{pattern.pattern!r} matched {match.group(0)[:32]!r}")
    if leaks:
        raise SystemExit(
            "sanitise_transcript: secret pattern survived sanitisation:\n  "
            + "\n  ".join(leaks)
        )


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("input")
    ap.add_argument("output", nargs="?")
    ap.add_argument("--check", action="store_true", help="assert only; do not write")
    args = ap.parse_args(argv)

    data = Path(args.input).read_bytes()

    if args.check:
        assert_clean(data)
        print(f"sanitise_transcript: {args.input} clean ({len(data)} bytes)")
        return 0

    if not args.output:
        ap.error("OUTPUT is required unless --check is given")
    cleaned = sanitise(data)
    assert_clean(cleaned)
    out = Path(args.output)
    out.resolve().parent.mkdir(parents=True, exist_ok=True)
    out.write_bytes(cleaned)
    print(
        f"sanitise_transcript: {args.input} -> {args.output} "
        f"({len(data)} -> {len(cleaned)} bytes, clean)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
