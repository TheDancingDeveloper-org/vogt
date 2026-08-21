"""The CLI adapter."""

from __future__ import annotations

import json
import shlex
from pathlib import Path

import pytest

from vogt.adapters.cli.main import EXIT_ERROR, EXIT_OK, EXIT_USAGE, main, run
from vogt.application.context import AppContext


def test_init_then_status_reports_revision_zero(context: AppContext) -> None:
    assert run(["init"], context=context).exit_code == EXIT_OK
    result = run(["--json", "status"], context=context)
    assert result.exit_code == EXIT_OK
    assert json.loads(result.stdout)["revision"] == 0


def test_registering_a_project_needs_a_reason(instance: AppContext) -> None:
    result = run(
        ["project", "register", "--name", "X", "--root-path", "/srv/x"],
        context=instance,
    )
    assert result.exit_code == EXIT_USAGE
    assert "reason" in result.stderr


def test_a_blank_reason_is_rejected_as_a_usage_error(instance: AppContext) -> None:
    result = run(
        shlex.split('project register --name X --root-path /srv/x --reason "   "'),
        context=instance,
    )
    assert result.exit_code == EXIT_USAGE


def test_a_conflict_is_an_error_not_a_crash(instance: AppContext) -> None:
    argv = shlex.split(
        "project register --name Twice --root-path /srv/twice --reason first"
    )
    assert run(argv, context=instance).exit_code == EXIT_OK
    second = run(argv, context=instance)
    assert second.exit_code == EXIT_ERROR
    assert "conflict" in second.stderr


def test_status_before_init_explains_itself(context: AppContext) -> None:
    result = run(["status"], context=context)
    assert result.exit_code == EXIT_ERROR
    assert "not_initialized" in result.stderr
    assert "vogt init" in result.stderr


def test_no_command_prints_help(monkeypatch: pytest.MonkeyPatch) -> None:
    # Python 3.13+ argparse colourises help output, wrapping "usage: vogt" in
    # ANSI SGR codes so the plain substring check misses. The project targets
    # 3.11 (where there is no colour), but the suite must pass on newer
    # interpreters too; NO_COLOR forces argparse's plain formatter.
    monkeypatch.setenv("NO_COLOR", "1")
    result = run([])
    assert result.exit_code == EXIT_USAGE
    assert "usage: vogt" in result.stdout


def test_unknown_command_is_a_usage_error() -> None:
    assert run(["nope"]).exit_code == EXIT_USAGE


def test_every_command_builds_its_parser() -> None:
    """The parser is built for the whole registry, so one bad flag is fatal.

    `serve --no-auth` was exactly that. `BooleanOptionalAction` refuses an
    option name beginning with `--no-` from Python 3.13 onward, and it refuses
    it while the parser is being *constructed* — so on a new enough
    interpreter every `vogt` invocation, including `--help`, raised
    `ValueError` before reading its arguments. The suite ran on 3.11 and saw
    none of it; `requires-python` says 3.11 *or later*.

    Asserted against the built parser rather than one command's behaviour,
    because the failure was never about `--no-auth` in particular: any field a
    future operation phrases as a negative brings the same crash back.
    """
    from vogt.adapters.cli.main import build_parser
    from vogt.registry import default_registry

    parser = build_parser(default_registry())
    assert parser.prog == "vogt"


def test_a_negative_flag_is_a_plain_switch() -> None:
    """`--no-auth` sets it; there is no `--no-no-auth` to unset it."""
    from vogt.adapters.cli.main import build_parser
    from vogt.registry import default_registry

    parser = build_parser(default_registry())
    serve = ["serve", "--host", "127.0.0.1", "--port", "8000"]
    assert parser.parse_args([*serve, "--no-auth"]).no_auth is True
    assert parser.parse_args(serve).no_auth is None
    # `--read-only` is not phrased as a negative, so it keeps both forms.
    assert parser.parse_args([*serve, "--no-read-only"]).read_only is False


def test_choices_come_from_the_model(instance: AppContext) -> None:
    result = run(
        shlex.split(
            "project register --name X --root-path /srv/x "
            "--lifecycle-state nonsense --reason r"
        ),
        context=instance,
    )
    assert result.exit_code == EXIT_USAGE


def test_text_output_renders_nested_results(instance: AppContext) -> None:
    run(
        shlex.split(
            "project register --name Rendered --root-path /srv/rendered "
            '--reason "render me"'
        ),
        context=instance,
    )
    result = run(["project", "list"], context=instance)
    assert "slug: rendered" in result.stdout
    assert "repo_url: -" in result.stdout, "None renders as a dash, not 'None'"


def test_empty_lists_say_none(instance: AppContext) -> None:
    assert "events: (none)" in run(["events", "list"], context=instance).stdout


def test_main_writes_to_the_streams_it_is_given(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setenv("VOGT_DATA_DIR", str(tmp_path / "instance"))
    assert main(["init"]) == EXIT_OK
    assert "instance_id" in capsys.readouterr().out


def test_the_data_dir_flag_overrides_configuration(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("VOGT_DATA_DIR", str(tmp_path / "ignored"))
    chosen = tmp_path / "chosen"
    result = run(["--json", "--data-dir", str(chosen), "init"])
    assert result.exit_code == EXIT_OK
    assert json.loads(result.stdout)["data_dir"] == str(chosen)
    assert (chosen / "declared.sqlite3").exists()
