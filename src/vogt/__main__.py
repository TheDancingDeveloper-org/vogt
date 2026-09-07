"""`python -m vogt`."""

from __future__ import annotations

from vogt.adapters.cli.main import main

if __name__ == "__main__":  # pragma: no cover - trivial delegation
    raise SystemExit(main())
