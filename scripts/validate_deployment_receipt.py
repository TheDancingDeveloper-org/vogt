#!/usr/bin/env python3
"""Validate a production deployment receipt against Vogt's checked-in schema.

The production runner deliberately has no project dependency installation step.
This validator implements the small, explicit JSON Schema vocabulary used by
``deploy/vogt-deployment-receipt.schema.json`` using only the standard library.
It fails closed if that schema grows a keyword it does not understand.
"""

from __future__ import annotations

import json
import re
import sys
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import cast

_SUPPORTED = frozenset(
    {
        "$schema",
        "title",
        "type",
        "additionalProperties",
        "required",
        "properties",
        "minLength",
        "pattern",
        "const",
    }
)


class ReceiptValidationError(ValueError):
    """The receipt or its schema does not satisfy the handoff contract."""


def _as_mapping(value: object, name: str) -> Mapping[str, object]:
    if not isinstance(value, Mapping):
        raise ReceiptValidationError(f"{name} must be an object")
    return cast(Mapping[str, object], value)


def _matches_type(value: object, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, Mapping)
    if expected == "string":
        return isinstance(value, str)
    raise ReceiptValidationError(f"unsupported schema type: {expected}")


def _validate(value: object, schema: Mapping[str, object], path: str) -> None:
    unsupported = set(schema) - _SUPPORTED
    if unsupported:
        names = ", ".join(sorted(unsupported))
        raise ReceiptValidationError(f"unsupported schema keyword(s): {names}")

    expected = schema.get("type")
    if expected is not None:
        if not isinstance(expected, str):
            raise ReceiptValidationError(f"{path}: schema type must be a string")
        if not _matches_type(value, expected):
            raise ReceiptValidationError(f"{path}: expected {expected}")

    if "const" in schema and value != schema["const"]:
        raise ReceiptValidationError(f"{path}: value does not match const")

    if isinstance(value, str):
        minimum = schema.get("minLength")
        if minimum is not None:
            if not isinstance(minimum, int) or isinstance(minimum, bool):
                raise ReceiptValidationError(f"{path}: minLength must be an integer")
            if len(value) < minimum:
                raise ReceiptValidationError(
                    f"{path}: string is shorter than minLength"
                )
        pattern = schema.get("pattern")
        if pattern is not None:
            if not isinstance(pattern, str):
                raise ReceiptValidationError(f"{path}: pattern must be a string")
            if re.search(pattern, value) is None:
                raise ReceiptValidationError(f"{path}: string does not match pattern")

    if not isinstance(value, Mapping):
        return

    properties_value = schema.get("properties", {})
    properties = _as_mapping(properties_value, f"{path}.properties")
    required_value = schema.get("required", ())
    if not isinstance(required_value, Sequence) or isinstance(
        required_value, (str, bytes)
    ):
        raise ReceiptValidationError(f"{path}: required must be an array")
    for name in required_value:
        if not isinstance(name, str):
            raise ReceiptValidationError(f"{path}: required names must be strings")
        if name not in value:
            raise ReceiptValidationError(f"{path}: missing required property {name}")

    additional = schema.get("additionalProperties", True)
    if not isinstance(additional, bool):
        raise ReceiptValidationError(f"{path}: additionalProperties must be boolean")
    if not additional:
        unexpected = set(value) - set(properties)
        if unexpected:
            names = ", ".join(sorted(str(name) for name in unexpected))
            raise ReceiptValidationError(f"{path}: unexpected property(s): {names}")

    for name, child_schema in properties.items():
        if name in value:
            child = _as_mapping(child_schema, f"{path}.{name} schema")
            _validate(value[name], child, f"{path}.{name}")


def validate_receipt(receipt_path: Path, schema_path: Path) -> None:
    """Raise ``ReceiptValidationError`` unless the receipt matches the schema."""
    try:
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        schema = json.loads(schema_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise ReceiptValidationError(str(exc)) from exc
    _validate(receipt, _as_mapping(schema, "schema"), "$")


def main(argv: Sequence[str]) -> int:
    if len(argv) != 3:
        print(f"usage: {argv[0]} RECEIPT.json SCHEMA.json", file=sys.stderr)
        return 2
    try:
        validate_receipt(Path(argv[1]), Path(argv[2]))
    except ReceiptValidationError as exc:
        print(f"invalid deployment receipt: {exc}", file=sys.stderr)
        return 1
    print("deployment receipt matches the checked-in schema")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
