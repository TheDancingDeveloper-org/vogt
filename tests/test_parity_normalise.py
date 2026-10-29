"""The parity script's normaliser keeps a wrong version visible.

The ``[version]`` rule exists so a golden survives a release bump: the build
stamp on the answers that report the build itself is not behaviour. It must
only substitute the version under test. Blanking the field outright hides a
real divergence, which is what a dev build reporting ``local/dev`` against
Python's release turned out to be.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

_SPEC = importlib.util.spec_from_file_location(
    "parity", Path(__file__).resolve().parents[1] / "scripts" / "parity.py"
)
assert _SPEC is not None and _SPEC.loader is not None
parity = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(parity)


def _version_answer(version: str) -> dict[str, object]:
    normalised: dict[str, object] = parity._normalise(
        {"version": version, "status": "ok"},
        Path("/tmp/root"),
        Path("/tmp/data"),
        "http.version",
    )
    return normalised


def test_the_release_version_is_substituted() -> None:
    import tomllib

    release = str(
        tomllib.loads(Path("pyproject.toml").read_text())["project"]["version"]
    )
    assert _version_answer(release)["version"] == "<version>"


def test_a_dev_build_stamp_stays_visible() -> None:
    # Neither side reports this. A Rust build that fell back to it is exactly
    # the divergence the rule must not hide.
    assert _version_answer("local/dev")["version"] == "local/dev"


def test_any_other_version_stays_visible() -> None:
    answer = _version_answer("0.0.0-not-the-build")
    assert answer["version"] == "0.0.0-not-the-build"
    assert answer["status"] == "ok"


def test_a_version_on_another_operation_is_untouched() -> None:
    answer = parity._normalise(
        {"version": "9.9.9"}, Path("/tmp/root"), Path("/tmp/data"), "work.get"
    )
    assert answer["version"] == "9.9.9"


def test_a_structured_kind_is_not_a_set_membership_error() -> None:
    # Some responses carry a structured `kind` (a dict) instead of a string.
    # Testing that against a set of strings must not raise TypeError.
    answer = parity._normalise(
        {"kind": {"name": "not-a-string"}, "content_digest": "abc", "status": "ok"},
        Path("/tmp/root"),
        Path("/tmp/data"),
        "observations.list",
    )
    assert answer["kind"] == {"name": "not-a-string"}
    assert answer["content_digest"] == "abc"


def test_a_structured_operation_name_is_not_a_set_membership_error() -> None:
    answer = parity._normalise(
        {"operation": {"name": "project.register"}, "payload_digest": "abc"},
        Path("/tmp/root"),
        Path("/tmp/data"),
        "audit.list",
    )
    assert answer["operation"] == {"name": "project.register"}
    assert answer["payload_digest"] == "abc"


def test_a_missing_golden_file_is_an_error_before_the_run(
    tmp_path: Path, capsys: object
) -> None:
    # A golden that recorded only the CLI transport must not traceback on
    # http.json, and must not start the script to find that out.
    empty = tmp_path / "golden"
    empty.mkdir()
    assert parity._missing_golden(empty, "http", "") == (
        f"no http golden at {empty / 'http.json'} "
        "(record with: parity.py record --impl python --transport http)"
    )
    (empty / "http.json").write_text("[]\n")
    assert parity._missing_golden(empty, "http", "") == (
        f"no http audit golden at {empty / 'audit.http.json'} "
        "(a full record writes audit.http.json)"
    )
    # A filtered run compares no audit trail.
    assert parity._missing_golden(empty, "http", "actor") is None
    (empty / "audit.http.json").write_text("[]\n")
    assert parity._missing_golden(empty, "http", "") is None
    code = parity.main(
        ["check", "--impl", "python", "--transport", "mcp", "--golden", str(empty)]
    )
    assert code == 1
    captured = capsys.readouterr()  # type: ignore[attr-defined]
    assert "no mcp golden" in captured.err
    assert "Traceback" not in captured.err
