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
    return parity._normalise(
        {"version": version, "status": "ok"},
        Path("/tmp/root"),
        Path("/tmp/data"),
        "http.version",
    )


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
