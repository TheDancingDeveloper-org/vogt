"""Human rendering of operation results.

Generic on purpose: results are pydantic models, so one renderer covers every
operation and there is nothing to forget to update when one is added. `--json`
returns the same data verbatim for scripts and for the parity tests.
"""

from __future__ import annotations

import json
from typing import Any

from pydantic import BaseModel


def to_json(result: BaseModel) -> str:
    return json.dumps(result.model_dump(mode="json"), indent=2, sort_keys=False)


def to_text(result: BaseModel) -> str:
    lines: list[str] = []
    _render(result.model_dump(mode="json"), lines, indent=0)
    return "\n".join(lines)


def _render(value: Any, lines: list[str], *, indent: int) -> None:
    pad = "  " * indent
    if isinstance(value, dict):
        for key, item in value.items():
            if isinstance(item, dict):
                lines.append(f"{pad}{key}:")
                _render(item, lines, indent=indent + 1)
            elif isinstance(item, list):
                if not item:
                    lines.append(f"{pad}{key}: (none)")
                else:
                    lines.append(f"{pad}{key}:")
                    _render(item, lines, indent=indent + 1)
            else:
                lines.append(f"{pad}{key}: {_scalar(item)}")
    elif isinstance(value, list):
        for item in value:
            if isinstance(item, dict):
                lines.append(f"{pad}-")
                _render(item, lines, indent=indent + 1)
            else:
                lines.append(f"{pad}- {_scalar(item)}")
    else:  # pragma: no cover - top-level scalars do not occur
        lines.append(f"{pad}{_scalar(value)}")


def _scalar(value: Any) -> str:
    if value is None:
        return "-"
    if isinstance(value, bool):
        return "yes" if value else "no"
    return str(value)
