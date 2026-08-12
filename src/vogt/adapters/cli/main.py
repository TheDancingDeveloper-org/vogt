"""`vogt` — the CLI adapter.

Commands, flags and help text are generated from the operation registry, so
the CLI cannot drift from the other surfaces without the parity tests
noticing (FR-A1, FR-A3).
"""

from __future__ import annotations

import argparse
import io
import sys
from collections.abc import Sequence
from contextlib import redirect_stderr, redirect_stdout
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from pydantic import ValidationError

from vogt import __version__
from vogt.adapters.cli.args import add_model_arguments, collect_params
from vogt.adapters.cli.render import to_json, to_text
from vogt.application.context import AppContext, build_context
from vogt.config import load_config
from vogt.errors import VogtError
from vogt.registry import OperationRegistry, default_registry
from vogt.registry.operation import Operation

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_USAGE = 2

DEST_OPERATION = "_operation"


@dataclass(frozen=True)
class CliResult:
    """The outcome of one CLI invocation, so tests need not capture streams."""

    exit_code: int
    stdout: str
    stderr: str


def build_parser(registry: OperationRegistry) -> argparse.ArgumentParser:
    """Build the whole command tree from the registry."""
    parser = argparse.ArgumentParser(
        prog="vogt",
        description=(
            "Vogt — per-repo and estate-wide product development state, "
            "with provenance and freshness on every answer."
        ),
    )
    parser.add_argument("--version", action="version", version=f"vogt {__version__}")
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=None,
        help="Instance data directory (overrides VOGT_DATA_DIR).",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit the raw result as JSON instead of formatted text.",
    )
    subparsers = parser.add_subparsers(dest="command", metavar="<command>")
    groups: dict[str, Any] = {}

    for operation in registry.for_transport("cli"):
        path = operation.cli.path
        if len(path) == 1:
            command = subparsers.add_parser(path[0], help=operation.summary)
        else:
            head = path[0]
            if head not in groups:
                group_parser = subparsers.add_parser(head, help=f"{head} operations")
                groups[head] = group_parser.add_subparsers(
                    dest=f"{head}_command", metavar="<subcommand>"
                )
            command = groups[head].add_parser(path[1], help=operation.summary)
        add_model_arguments(command, operation.params_model)
        command.set_defaults(**{DEST_OPERATION: operation.name})

    return parser


def run(
    argv: Sequence[str] | None = None,
    *,
    registry: OperationRegistry | None = None,
    context: AppContext | None = None,
) -> CliResult:
    """Execute one invocation and return its result without touching sys."""
    active_registry = registry if registry is not None else default_registry()
    parser = build_parser(active_registry)
    # argparse writes usage, --help and --version straight to the process
    # streams and exits. Capturing them keeps CliResult the whole truth, which
    # is what lets tests assert on messages rather than on exit codes alone.
    captured_out, captured_err = io.StringIO(), io.StringIO()
    try:
        with redirect_stdout(captured_out), redirect_stderr(captured_err):
            namespace = parser.parse_args(list(argv) if argv is not None else None)
    except SystemExit as exc:
        code = exc.code if isinstance(exc.code, int) else EXIT_USAGE
        return CliResult(
            exit_code=code,
            stdout=captured_out.getvalue(),
            stderr=captured_err.getvalue(),
        )

    operation_name = getattr(namespace, DEST_OPERATION, None)
    if operation_name is None:
        return CliResult(exit_code=EXIT_USAGE, stdout=parser.format_help(), stderr="")

    operation: Operation[Any, Any] = active_registry.get(str(operation_name))
    ctx = context if context is not None else _context_for(namespace)

    try:
        params = operation.params_model.model_validate(
            collect_params(namespace, operation.params_model)
        )
        result = operation.run(ctx, params)
    except ValidationError as exc:
        return CliResult(
            exit_code=EXIT_USAGE,
            stdout="",
            stderr=f"error: invalid arguments for {operation.name}:\n{exc}\n",
        )
    except VogtError as exc:
        return CliResult(
            exit_code=EXIT_ERROR,
            stdout="",
            stderr=f"error: {exc.code}: {exc}\n",
        )

    rendered = to_json(result) if namespace.json else to_text(result)
    return CliResult(exit_code=EXIT_OK, stdout=rendered + "\n", stderr="")


def _context_for(namespace: argparse.Namespace) -> AppContext:
    data_dir: Path | None = getattr(namespace, "data_dir", None)
    config = load_config(data_dir=data_dir) if data_dir else load_config()
    return build_context(config=config)


def main(
    argv: Sequence[str] | None = None,
    *,
    stdout: TextIO | None = None,
    stderr: TextIO | None = None,
) -> int:
    """Console-script entry point."""
    result = run(argv)
    (stdout or sys.stdout).write(result.stdout)
    (stderr or sys.stderr).write(result.stderr)
    return result.exit_code


if __name__ == "__main__":  # pragma: no cover - exercised via the console script
    raise SystemExit(main())
