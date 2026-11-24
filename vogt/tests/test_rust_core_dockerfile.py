"""Tests for `scripts/check_rust_core_dockerfile.py`, against fixture images."""

from __future__ import annotations

from pathlib import Path

from check_rust_core_dockerfile import check, local_path_dependencies

REPO_ROOT = Path(__file__).resolve().parents[1]


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _fixture(root: Path) -> None:
    _write(root / "engine/Cargo.toml", '[workspace]\nmembers = ["core", "contract"]\n')
    _write(
        root / "engine/core/Cargo.toml",
        '[package]\nname = "vogt-core"\nversion = "0.1.0"\n'
        '\n[dependencies]\nvogt-engine-contract = { path = "../contract" }\n',
    )
    _write(root / "engine/contract/Cargo.toml", '[package]\nname = "contract"\n')
    _write(root / "engine/contract/src/lib.rs", "pub fn contract() {}\n")
    _write(root / "deploy/rust-core.Dockerfile", "COPY engine/core core\n")
    _write(
        root / "deploy/rust-core.Dockerfile.dockerignore",
        "*\n!engine/core/\n!engine/core/**\n",
    )


def test_the_repository_image_copies_every_core_path_dependency() -> None:
    assert check(REPO_ROOT) == []


def test_a_missing_copy_is_reported(tmp_path: Path) -> None:
    _fixture(tmp_path)
    problems = check(tmp_path)
    assert len(problems) == 1
    assert problems[0].dependency == tmp_path / "engine/contract"
    assert "no COPY instruction" in problems[0].reasons[0]


def test_a_dockerignore_exclusion_is_reported(tmp_path: Path) -> None:
    _fixture(tmp_path)
    _write(
        tmp_path / "deploy/rust-core.Dockerfile",
        "COPY engine/core core\nCOPY engine/contract contract\n",
    )
    problems = check(tmp_path)
    assert len(problems) == 1
    assert "ignore file excludes engine/contract" in problems[0].reasons[0]


def test_the_ignore_file_is_checked_after_a_copy(tmp_path: Path) -> None:
    _fixture(tmp_path)
    _write(
        tmp_path / "deploy/rust-core.Dockerfile",
        "COPY engine/core core\nCOPY engine/contract contract\n",
    )
    _write(
        tmp_path / "deploy/rust-core.Dockerfile.dockerignore",
        "*\n!engine/core/\n!engine/core/**\n!engine/contract/\n!engine/contract/**\n",
    )
    assert check(tmp_path) == []


def test_transitive_path_dependencies_are_followed(tmp_path: Path) -> None:
    _fixture(tmp_path)
    _write(
        tmp_path / "engine/contract/Cargo.toml",
        '[package]\nname = "contract"\n\n'
        '[dependencies]\nnested = { path = "../nested" }\n',
    )
    _write(tmp_path / "engine/nested/Cargo.toml", '[package]\nname = "nested"\n')
    found = local_path_dependencies(tmp_path)
    assert found == {
        (tmp_path / "engine/contract").resolve(),
        (tmp_path / "engine/nested").resolve(),
    }


def test_a_copy_from_another_stage_does_not_count(tmp_path: Path) -> None:
    _fixture(tmp_path)
    _write(
        tmp_path / "deploy/rust-core.Dockerfile",
        "FROM rust AS build\nCOPY engine/core core\n"
        "COPY --from=contract /contract contract\n",
    )
    problems = check(tmp_path)
    assert len(problems) == 1
    assert "no COPY instruction" in problems[0].reasons[0]
