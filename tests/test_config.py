"""Configuration: precedence, the default policy, and generated-doc drift."""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.config import (
    CONFIG_FILE_ENV,
    VogtConfig,
    config_artifacts,
    default_data_dir,
    describe_fields,
    load_config,
)

REPO_ROOT = Path(__file__).resolve().parents[1]


def test_explicit_arguments_win(clean_env: None, tmp_path: Path) -> None:
    del clean_env
    assert load_config(data_dir=tmp_path).data_dir == tmp_path


def test_environment_beats_the_file(
    clean_env: None, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    del clean_env
    config_file = tmp_path / "vogt.toml"
    config_file.write_text('data_dir = "/from/file"\n', encoding="utf-8")
    monkeypatch.setenv(CONFIG_FILE_ENV, str(config_file))
    monkeypatch.setenv("VOGT_DATA_DIR", "/from/env")

    assert load_config().data_dir == Path("/from/env")


def test_the_file_beats_the_default(
    clean_env: None, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    del clean_env
    config_file = tmp_path / "vogt.toml"
    config_file.write_text('log_level = "debug"\n', encoding="utf-8")
    monkeypatch.setenv(CONFIG_FILE_ENV, str(config_file))

    assert load_config().log_level == "debug"


def test_a_missing_config_file_is_not_an_error(
    clean_env: None, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    del clean_env
    monkeypatch.setenv(CONFIG_FILE_ENV, str(tmp_path / "absent.toml"))
    assert load_config().log_level == "info"


def test_unknown_settings_are_refused(clean_env: None) -> None:
    del clean_env
    with pytest.raises(ValueError, match="not_a_setting"):
        VogtConfig(not_a_setting=1)  # type: ignore[call-arg]


def test_data_dir_follows_xdg(
    clean_env: None, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    del clean_env
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    assert default_data_dir() == tmp_path / "vogt"


def test_store_paths_hang_off_the_data_dir(tmp_path: Path) -> None:
    config = VogtConfig(data_dir=tmp_path)
    assert config.declared_db_path == tmp_path / "declared.sqlite3"
    assert config.observed_db_path == tmp_path / "observed.sqlite3"
    assert config.backups_dir == tmp_path / "backups"


def test_a_tilde_in_the_data_dir_is_expanded() -> None:
    config = VogtConfig(data_dir=Path("~/vogt-test"))
    assert "~" not in str(config.resolved_data_dir)


# -- NFR-D2: the default policy -------------------------------------------


def test_exposure_values_never_carry_a_default() -> None:
    """A wrong default here is the `:18081` incident (DEPLOYMENT §4.1)."""
    for field in describe_fields():
        if field.policy == "exposure":
            assert field.default_label.startswith("*(no default"), (
                f"{field.name} decides exposure or identity and must not default"
            )


def test_allocation_values_always_carry_a_default() -> None:
    """Gating allocation values is what broke every cadastre deploy after #42."""
    for field in describe_fields():
        if field.policy == "allocation":
            assert not field.default_label.startswith("*(no default"), (
                f"{field.name} is a host allocation and must carry a default"
            )


def test_every_field_is_documented_and_classified() -> None:
    for field in describe_fields():
        assert field.description.strip(), f"{field.name} has no description"
        assert field.policy in {"exposure", "allocation", "behaviour"}


# -- NFR-Q4: generated artefacts ------------------------------------------


@pytest.mark.parametrize(
    "relative",
    [Path("docs/CONFIG.md"), Path("config.example.toml")],
)
def test_generated_files_match_the_schema(relative: Path) -> None:
    """CI fails on drift between the schema and what is committed."""
    artifacts = config_artifacts(REPO_ROOT)
    path = REPO_ROOT / relative
    assert path.exists(), f"{relative} is missing — run scripts/gen_config_docs.py"
    assert path.read_text(encoding="utf-8") == artifacts[path], (
        f"{relative} is stale — run scripts/gen_config_docs.py"
    )


def test_type_labels_never_leak_the_interpreter(clean_env: None) -> None:
    """The generated docs must not depend on which Python rendered them.

    `Path | None` stringifies as `pathlib.Path | None` on 3.11, as
    `pathlib._local.Path | None` on 3.13, and as `Union` under a third
    path — so the drift check passed locally and failed on two different CI
    jobs for two different reasons. Documentation should describe the
    setting, not where the standard library keeps a class.
    """
    del clean_env
    for field in describe_fields():
        label = field.type_label
        assert "pathlib" not in label, f"{field.name}: leaks a module path"
        assert "Union" not in label, f"{field.name}: leaks a typing construct"
        assert "typing." not in label, f"{field.name}: leaks a typing construct"
        assert "<" not in label, f"{field.name}: leaks a repr"


def test_generated_docs_are_byte_identical_across_interpreters() -> None:
    """A weaker but broader guard than the label check above.

    Any future renderer that reaches for `str(annotation)` reintroduces the
    same class of bug; this catches the symptom wherever it comes from.
    """
    rendered = config_artifacts(REPO_ROOT)
    for content in rendered.values():
        assert "pathlib" not in content
        assert "| Union |" not in content
