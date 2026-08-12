#!/usr/bin/env python3
"""Regenerate the artefacts derived from the config schema (NFR-Q4).

Run after changing `src/vogt/config.py`; `tests/test_config.py` fails until
the committed files match again, so the drift cannot ship.
"""

from __future__ import annotations

import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "src"))

from vogt.config import config_artifacts  # noqa: E402


def main() -> int:
    for path, content in config_artifacts(REPO_ROOT).items():
        path.parent.mkdir(parents=True, exist_ok=True)
        existing = path.read_text(encoding="utf-8") if path.exists() else None
        if existing == content:
            print(f"unchanged  {path.relative_to(REPO_ROOT)}")
            continue
        path.write_text(content, encoding="utf-8")
        print(f"written    {path.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
