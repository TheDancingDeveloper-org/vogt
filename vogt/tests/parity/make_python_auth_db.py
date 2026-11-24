"""Write a declared store with the Python core, for the Rust token cross-check.

WI-1053's hard gate: a token and a password that the Python core stored must
still authenticate once the Rust core reads the same database. This script
builds that database with the Python services (not with hand-written SQL) and
records the secrets it minted beside it. The Rust test
`engine/core/tests/python_auth_db.rs` opens a copy and signs in with them.

Run from the repo root to regenerate:

    uv run python tests/parity/make_python_auth_db.py

The secrets are random on every run, so a regenerated fixture differs byte for
byte. They are test credentials for a throwaway instance and nothing else.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
import sys
import tempfile
from pathlib import Path

from vogt.application.context import build_context
from vogt.application.models import (
    CreateActorParams,
    CreateUserParams,
    InitParams,
    IssueTokenParams,
)
from vogt.application.services import create_actor, init_instance, issue_token
from vogt.application.services.auth import create_user
from vogt.config import VogtConfig
from vogt.core.principal import Principal

OUT = Path(__file__).resolve().parents[2] / "engine/core/tests/fixtures/python-auth"
WHY = "WI-1053 token cross-check fixture"
PASSWORD = "correct horse battery staple"


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        data_dir = Path(tmp) / "instance"
        ctx = build_context(
            config=VogtConfig(data_dir=data_dir, session_transcript_roots={}),
            principal=Principal(
                identity_ref="local:fixture", kind="human", display_name="fixture"
            ),
        )
        init_instance(ctx, InitParams())
        create_actor(
            ctx,
            CreateActorParams(
                identity_ref="agent:cross-check",
                kind="agent",
                display_name="Cross-check agent",
                reason=WHY,
            ),
        )
        api = issue_token(
            ctx,
            IssueTokenParams(
                actor="agent:cross-check",
                name="python-issued",
                scopes="read,work.write",
                reason=WHY,
            ),
        )
        create_user(
            ctx,
            CreateUserParams(
                username="pyuser", password=PASSWORD, scopes="admin", reason=WHY
            ),
        )
        db = data_dir / "declared.sqlite3"
        # Fold the WAL in so the one file is the whole store.
        with sqlite3.connect(db) as conn:
            conn.execute("PRAGMA wal_checkpoint(TRUNCATE)")
            conn.execute("PRAGMA journal_mode=DELETE")
        OUT.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(db, OUT / "declared.sqlite3")
        (OUT / "secrets.json").write_text(
            json.dumps(
                {
                    "api_token": {
                        "secret": api.secret,
                        "actor": "agent:cross-check",
                        "scopes": ["read", "work.write"],
                    },
                    "user": {"username": "pyuser", "password": PASSWORD},
                },
                indent=2,
            )
            + "\n"
        )
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
