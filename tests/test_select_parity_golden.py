"""Golden selection orders by commit time, not directory mtime (WI-2662)."""

from __future__ import annotations

import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "select_parity_golden.sh"


def _git(repo: Path, *args: str, env: dict[str, str] | None = None) -> str:
    merged = os.environ.copy()
    merged.update(
        {
            "GIT_AUTHOR_NAME": "tester",
            "GIT_AUTHOR_EMAIL": "tester@example.com",
            "GIT_COMMITTER_NAME": "tester",
            "GIT_COMMITTER_EMAIL": "tester@example.com",
        }
    )
    if env:
        merged.update(env)
    out = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        text=True,
        env=merged,
    )
    return out.stdout.strip()


def _commit(repo: Path, when: str, message: str) -> str:
    env = {
        "GIT_AUTHOR_DATE": when,
        "GIT_COMMITTER_DATE": when,
    }
    _git(repo, "commit", "--allow-empty", "-m", message, env=env)
    return _git(repo, "rev-parse", "HEAD")


def test_fallback_picks_the_newest_ancestor_by_commit_time(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-b", "main")
    # Recordings sit behind main. The branch is cut from a later main
    # commit, so the merge-base name is not one of the golden directories.
    older = _commit(repo, "2026-01-01T00:00:00Z", "older")
    newer = _commit(repo, "2026-06-01T00:00:00Z", "newer")
    base = _commit(repo, "2026-08-01T00:00:00Z", "main moves on")
    _git(repo, "checkout", "-b", "topic")
    _commit(repo, "2026-09-01T00:00:00Z", "topic")
    assert base[:7] not in {older[:7], newer[:7]}
    _git(repo, "update-ref", "refs/remotes/origin/main", "main")

    golden = repo / "goldens"
    # The older commit's directory is written last, so its mtime is newer.
    (golden / newer[:7]).mkdir(parents=True)
    (golden / older[:7]).mkdir(parents=True)

    out = subprocess.run(
        [str(SCRIPT), "goldens"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    assert out.stdout.strip() == f"goldens/{newer[:7]}"
    assert "warning:" in out.stderr
    assert newer[:7] in out.stderr


def test_merge_base_golden_wins_when_it_is_present(tmp_path: Path) -> None:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-b", "main")
    base = _commit(repo, "2026-01-01T00:00:00Z", "base")
    _git(repo, "checkout", "-b", "topic")
    _commit(repo, "2026-02-01T00:00:00Z", "topic")
    _git(repo, "update-ref", "refs/remotes/origin/main", "main")
    (repo / "goldens" / base[:7]).mkdir(parents=True)

    out = subprocess.run(
        [str(SCRIPT), "goldens"],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    assert out.stdout.strip() == f"goldens/{base[:7]}"
    assert out.stderr == ""
