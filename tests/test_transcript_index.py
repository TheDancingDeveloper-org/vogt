"""The conversation-id index behind transcript lookup (WI-1413).

`session.list` asks for one transcript per live session. The index is built
from a single directory listing and reused, so N lookups scan the root once.
"""

from __future__ import annotations

import os
import time
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest

from vogt.adapters import transcripts


@pytest.fixture(autouse=True)
def _fresh_index() -> None:
    transcripts._indexes.clear()


def _claude_root(tmp_path: Path, sessions: int, decoys: int) -> Path:
    root = tmp_path / "claude"
    for n in range(decoys):
        (root / f"dir-{n:05d}").mkdir(parents=True)
    keep = root / "dir-keep"
    keep.mkdir()
    for n in range(sessions):
        (keep / f"conv-{n:05d}.jsonl").write_text("{}\n")
    return root


def _counting(
    scans: dict[str, int], root: Path
) -> Callable[[os.PathLike[str] | str], Iterator[os.DirEntry[str]]]:
    real = os.scandir

    def counting(path: os.PathLike[str] | str) -> Iterator[os.DirEntry[str]]:
        if Path(path) == root:
            scans["n"] += 1
        return real(path)

    return counting


def test_many_lookups_scan_the_root_once(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _claude_root(tmp_path, sessions=70, decoys=300)
    scans = {"n": 0}
    monkeypatch.setattr(os, "scandir", _counting(scans, root))
    transcripts._indexes.clear()
    for n in range(70):
        found = transcripts._by_id("claude", root, f"conv-{n:05d}")
        assert found is not None and found.name == f"conv-{n:05d}.jsonl"
    # One listing of the root builds the index. A second listing is the
    # freshness stamp, taken once for the whole batch, not once per session.
    assert scans["n"] <= 2


def test_a_new_file_is_found_once_the_index_expires(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _claude_root(tmp_path, sessions=1, decoys=2)
    transcripts._indexes.clear()
    assert transcripts._by_id("claude", root, "conv-00000") is not None
    (root / "dir-keep" / "conv-new.jsonl").write_text("{}\n")
    # Move the directory's mtime on explicitly. The filesystem's mtime is
    # coarse, so a file written in the same tick as the first scan leaves the
    # stamp unchanged and the old index would stand (WI-2242).
    keep = root / "dir-keep"
    later = keep.stat().st_mtime + 10
    os.utime(keep, (later, later))
    # Still inside the TTL: the index stands and the new file is invisible.
    assert transcripts._by_id("claude", root, "conv-new") is None
    monkeypatch.setattr(transcripts, "_clock", lambda: time.monotonic() + 100)
    found = transcripts._by_id("claude", root, "conv-new")
    assert found is not None and found.name == "conv-new.jsonl"


def test_a_missing_id_does_not_rescan_within_the_ttl(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _claude_root(tmp_path, sessions=1, decoys=50)
    scans = {"n": 0}
    monkeypatch.setattr(os, "scandir", _counting(scans, root))
    transcripts._indexes.clear()
    for _ in range(40):
        assert transcripts._by_id("claude", root, "does-not-exist") is None
    assert scans["n"] <= 2


def test_codex_ids_share_one_scan(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "codex"
    day = root / "2026" / "10" / "14"
    day.mkdir(parents=True)
    ident = "0f8fad5b-d9cb-469f-a165-70867728950e"
    (day / f"rollout-2026-10-14T00-00-00-{ident}.jsonl").write_text("{}\n")
    scans = {"n": 0}
    real = os.scandir

    def counting(path: os.PathLike[str] | str) -> Iterator[os.DirEntry[str]]:
        scans["n"] += 1
        return real(path)

    monkeypatch.setattr(os, "scandir", counting)
    transcripts._indexes.clear()
    for _ in range(10):
        found = transcripts._by_id("codex", root, ident)
        assert found is not None
    first = scans["n"]
    for _ in range(10):
        assert transcripts._by_id("codex", root, ident) is not None
    assert scans["n"] == first
    assert first > 0


def test_a_new_codex_file_in_todays_directory_is_found_after_the_ttl(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = tmp_path / "codex"
    day = root / "2026" / "10" / "14"
    day.mkdir(parents=True)
    first = "0f8fad5b-d9cb-469f-a165-70867728950e"
    (day / f"rollout-2026-10-14T00-00-00-{first}.jsonl").write_text("{}\n")
    transcripts._indexes.clear()
    assert transcripts._by_id("codex", root, first) is not None
    # Written into the same day directory: no new month or year appears.
    second = "11111111-2222-4333-8444-555555555555"
    (day / f"rollout-2026-10-14T01-00-00-{second}.jsonl").write_text("{}\n")
    # Coarse filesystem mtime: move the day directory's stamp on explicitly so
    # the expired index sees the change (WI-2242).
    later = day.stat().st_mtime + 10
    os.utime(day, (later, later))
    assert transcripts._by_id("codex", root, second) is None
    monkeypatch.setattr(transcripts, "_clock", lambda: time.monotonic() + 100)
    found = transcripts._by_id("codex", root, second)
    assert found is not None and found.parent == day


def test_a_non_uuid_codex_name_still_resolves(tmp_path: Path) -> None:
    root = tmp_path / "codex"
    day = root / "2026" / "10" / "14"
    day.mkdir(parents=True)
    (day / "rollout-session-7.jsonl").write_text("{}\n")
    found = transcripts._by_id("codex", root, "rollout-session-7")
    assert found is not None and found.name == "rollout-session-7.jsonl"


def test_a_symlinked_transcript_is_followed(tmp_path: Path) -> None:
    root = tmp_path / "claude"
    real = tmp_path / "elsewhere"
    real.mkdir()
    (real / "conv-1.jsonl").write_text("{}\n")
    link_dir = root / "dir-keep"
    link_dir.mkdir(parents=True)
    (link_dir / "conv-1.jsonl").symlink_to(real / "conv-1.jsonl")
    found = transcripts._by_id("claude", root, "conv-1")
    assert found is not None and found.name == "conv-1.jsonl"
