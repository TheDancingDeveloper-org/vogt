"""Turning a pydantic parameter model into argparse flags.

The CLI has no hand-written argument list: adding a field to a parameter
model adds the flag, its type, its choices and its help text. That is the
mechanical half of "nothing is CLI-only, nothing is missing from the CLI"
(FR-A1).
"""

from __future__ import annotations

import argparse
import json
from collections.abc import Callable
from dataclasses import dataclass
from types import UnionType
from typing import Any, Literal, Union, get_args, get_origin

from pydantic import BaseModel
from pydantic.fields import FieldInfo


@dataclass(frozen=True)
class ArgSpec:
    """How one model field becomes one command-line flag."""

    flag: str
    dest: str
    required: bool
    kind: Callable[[str], Any]
    choices: tuple[str, ...] | None
    is_bool: bool
    is_list: bool
    help_text: str


def _unwrap_optional(annotation: Any) -> tuple[Any, bool]:
    origin = get_origin(annotation)
    if origin in (Union, UnionType):
        args = [arg for arg in get_args(annotation) if arg is not type(None)]
        if len(args) == 1:
            return args[0], True
    return annotation, False


def spec_for(name: str, field: FieldInfo) -> ArgSpec:
    """Describe the flag for one parameter-model field."""
    annotation, optional = _unwrap_optional(field.annotation)
    origin = get_origin(annotation)
    choices: tuple[str, ...] | None = None
    is_list = False
    kind: Callable[[str], Any] = str

    if origin is Literal:
        choices = tuple(str(arg) for arg in get_args(annotation))
    elif origin in (list, tuple):
        is_list = True
        element = get_args(annotation)[0] if get_args(annotation) else str
        # Structured batched reads remain usable from the generated CLI:
        # repeat `--cells '{"lane_key":"","state":"open"}'`.
        kind = (
            json.loads
            if isinstance(element, type) and issubclass(element, BaseModel)
            else str
        )
    elif annotation is bool:
        kind = bool
    elif annotation is int:
        kind = int
    elif annotation is float:
        kind = float

    return ArgSpec(
        flag=f"--{name.replace('_', '-')}",
        dest=name,
        required=field.is_required() and not optional,
        kind=kind,
        choices=choices,
        is_bool=annotation is bool,
        is_list=is_list,
        help_text=field.description or "",
    )


def add_model_arguments(
    parser: argparse.ArgumentParser, model: type[BaseModel]
) -> None:
    """Add one flag per field of `model` to `parser`."""
    for name, field in model.model_fields.items():
        spec = spec_for(name, field)
        if spec.is_bool:
            # A field already phrased as a negative — `no_auth` — has no
            # sensible `--no-no-auth`, and argparse says so: since 3.13,
            # `BooleanOptionalAction` raises `ValueError` on an option name
            # that starts with `--no-`, at parser-construction time. That is
            # every command, not just the one flag, so on a new enough
            # interpreter the CLI did not run at all. Such a field takes the
            # plain switch it always meant.
            if spec.flag.startswith("--no-"):
                parser.add_argument(
                    spec.flag,
                    dest=spec.dest,
                    action="store_true",
                    default=None,
                    help=spec.help_text or None,
                )
                continue
            parser.add_argument(
                spec.flag,
                dest=spec.dest,
                action=argparse.BooleanOptionalAction,
                default=None,
                help=spec.help_text or None,
            )
            continue
        parser.add_argument(
            spec.flag,
            dest=spec.dest,
            required=spec.required,
            default=None,
            action="append" if spec.is_list else "store",
            type=spec.kind if spec.choices is None else str,
            choices=list(spec.choices) if spec.choices else None,
            help=spec.help_text or None,
        )


def collect_params(
    namespace: argparse.Namespace, model: type[BaseModel]
) -> dict[str, Any]:
    """Read parsed flags back out, omitting anything the caller did not set.

    Omission matters: a flag left off must fall through to the model's own
    default rather than overwrite it with `None`.
    """
    values: dict[str, Any] = {}
    for name in model.model_fields:
        value = getattr(namespace, name, None)
        if value is not None:
            values[name] = value
    return values
