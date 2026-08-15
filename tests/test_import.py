"""Importing a repository from GitHub (FR-P6, FR-P7, FR-S8).

Includes the M7 import demo. From `ROADMAP.md`:

    Import a repository that exists only on GitHub, with a reason. It lands
    on disk at the configured root, appears in the project list with its
    issues and PRs already consolidated.

The clone itself is exercised twice over: through an injected cloner, which
is how every use-case test runs offline, and directly against a real local
`git` for the parts that are about git's behaviour rather than Vogt's.
"""

from __future__ import annotations

import json
import subprocess
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.git import (
    CloneOutcome,
    CloneRequest,
    GitUnavailable,
    clone_repository,
)
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    ImportProjectParams,
    InitParams,
    ListAuditParams,
    ListEventsParams,
    ListProjectsParams,
)
from vogt.application.services import (
    import_project,
    init_instance,
    list_audit,
    list_events,
    list_projects,
)
from vogt.config import VogtConfig
from vogt.errors import Conflict, InvalidRequest, NotFound

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "onboarding the estate"
REPO = "TheDancingDeveloper-org/rustnzb"


class FakeCloner:
    """A clone that records what it was asked for and writes a tree."""

    def __init__(self) -> None:
        self.requests: list[CloneRequest] = []

    def __call__(self, request: CloneRequest) -> CloneOutcome:
        self.requests.append(request)
        request.destination.mkdir(parents=True, exist_ok=True)
        (request.destination / "README.md").write_text("hi\n", encoding="utf-8")
        return CloneOutcome(
            destination=request.destination,
            revision="a" * 40,
            default_branch="main",
        )


@pytest.fixture
def cloner() -> FakeCloner:
    return FakeCloner()


@pytest.fixture
def importing(
    data_dir: Path, tmp_path: Path, cloner: FakeCloner
) -> tuple[AppContext, Path]:
    """An initialised instance whose import root is a known directory."""
    root = tmp_path / "estate"
    context = build_context(
        config=VogtConfig(
            data_dir=data_dir, import_root=root, sqlite_synchronous="off"
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        cloner=cloner,
    )
    init_instance(context, InitParams())
    return context, root


# -- the operation ---------------------------------------------------------


def test_import_clones_registers_and_lands_at_the_default_root(
    importing: tuple[AppContext, Path], cloner: FakeCloner
) -> None:
    """The demo, minus the forge: named, cloned, registered, on disk."""
    context, root = importing
    result = import_project(
        context, ImportProjectParams(repo=REPO, consolidate=False, reason=WHY)
    )

    assert result.project.slug == "rustnzb"
    assert result.root_path == str(root / "rustnzb")
    assert Path(result.root_path).is_dir()
    assert result.revision == "a" * 40
    assert result.cloned is True

    listed = list_projects(context, ListProjectsParams())
    assert [project.slug for project in listed.projects] == ["rustnzb"]
    # The remote is stored so collectors can find it — and stored without a
    # credential in it (FR-S8).
    assert listed.projects[0].repo_url == f"https://github.com/{REPO}"
    assert "@" not in (listed.projects[0].repo_url or "")


def test_an_explicit_root_path_wins_over_the_import_root(
    importing: tuple[AppContext, Path], tmp_path: Path
) -> None:
    context, _ = importing
    elsewhere = tmp_path / "somewhere-else"
    result = import_project(
        context,
        ImportProjectParams(
            repo=REPO, root_path=str(elsewhere), consolidate=False, reason=WHY
        ),
    )
    assert result.root_path == str(elsewhere)


def test_the_import_is_audited_as_an_import(
    importing: tuple[AppContext, Path],
) -> None:
    """ "Registered" and "imported from GitHub" are different answers.

    Collapsing them would lose the only record that a clone happened, which
    is the one part of this operation that touched the world outside the
    database (FR-S1).
    """
    context, _ = importing
    import_project(
        context, ImportProjectParams(repo=REPO, consolidate=False, reason=WHY)
    )

    operations = {
        record.operation for record in list_audit(context, ListAuditParams()).records
    }
    assert "project.import" in operations
    assert "project.register" not in operations

    kinds = {event.kind for event in list_events(context, ListEventsParams()).events}
    assert "project.imported" in kinds

    reasons = {
        record.reason
        for record in list_audit(context, ListAuditParams()).records
        if record.operation == "project.import"
    }
    assert reasons == {WHY}


def test_a_duplicate_slug_is_refused_before_anything_is_cloned(
    importing: tuple[AppContext, Path], cloner: FakeCloner
) -> None:
    """Cloning a repository only to fail on a name is a minute wasted."""
    context, _ = importing
    import_project(
        context, ImportProjectParams(repo=REPO, consolidate=False, reason=WHY)
    )
    assert len(cloner.requests) == 1

    with pytest.raises(Conflict):
        import_project(
            context, ImportProjectParams(repo=REPO, consolidate=False, reason=WHY)
        )
    assert len(cloner.requests) == 1, "the second import cloned before checking"


@pytest.mark.parametrize(
    ("reference", "expected"),
    [
        ("owner/name", "name"),
        ("https://github.com/owner/name", "name"),
        ("https://github.com/owner/name.git", "name"),
        ("git@github.com:owner/name.git", "name"),
        ("  owner/name  ", "name"),
    ],
)
def test_the_ways_a_person_names_a_repository(
    importing: tuple[AppContext, Path], reference: str, expected: str
) -> None:
    context, _ = importing
    result = import_project(
        context,
        ImportProjectParams(repo=reference, consolidate=False, reason=WHY),
    )
    assert result.project.slug == expected


@pytest.mark.parametrize("reference", ["", "   ", "just-a-name", "https://example.com"])
def test_something_that_is_not_a_repository_is_refused(
    importing: tuple[AppContext, Path], reference: str
) -> None:
    """A reference this instance cannot address is a request to go looking.

    Nothing here looks (FR-G15), so it is an error rather than a search.
    """
    context, _ = importing
    with pytest.raises((InvalidRequest, ValueError)):
        import_project(
            context,
            ImportProjectParams(repo=reference, consolidate=False, reason=WHY),
        )


def test_a_reason_is_required(importing: tuple[AppContext, Path]) -> None:
    with pytest.raises(ValueError):
        ImportProjectParams(repo=REPO, consolidate=False)  # type: ignore[call-arg]


# -- with the forge configured ---------------------------------------------


class Forge:
    """A fake GitHub recording every request, method included."""

    def __init__(self, *, exists: bool = True) -> None:
        self.exists = exists
        self.requests: list[tuple[str, str]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        del headers, body
        self.requests.append((method, url))
        if url.endswith(f"/repos/{REPO}"):
            if not self.exists:
                return 404, b""
            return 200, json.dumps(
                {"name": "rustnzb", "default_branch": "trunk"}
            ).encode("utf-8")
        if "/issues" in url:
            return 200, json.dumps(
                [
                    {
                        "number": 12,
                        "title": "Segment fetch retries forever",
                        "state": "open",
                        "labels": [],
                        "html_url": f"https://github.com/{REPO}/issues/12",
                    }
                ]
            ).encode("utf-8")
        if "/labels" in url:
            return 200, json.dumps([{"name": "bug", "color": "d73a4a"}]).encode("utf-8")
        return 200, b"[]"

    @property
    def mutations(self) -> list[tuple[str, str]]:
        return [(m, u) for m, u in self.requests if m != "GET"]


@pytest.fixture
def forge(monkeypatch: pytest.MonkeyPatch) -> Forge:
    fake = Forge()
    from vogt.adapters.github.client import GitHubClient

    def configured(cls: Any, path: Any, *, transport: Any = None) -> GitHubClient:
        del cls, path, transport
        return GitHubClient(token="ghp_fake", transport=fake)

    monkeypatch.setattr(GitHubClient, "from_token_file", classmethod(configured))
    return fake


def test_import_consolidates_and_changes_nothing_upstream(
    importing: tuple[AppContext, Path], forge: Forge
) -> None:
    """The M7 demo. Import reads history; it never writes it (FR-B3).

    Asserted by recording every request the adapter made and failing on any
    method that is not a GET — the same check M5 uses, because an import that
    quietly opened an issue would be the worst possible first impression.
    """
    context, _ = importing
    result = import_project(context, ImportProjectParams(repo=REPO, reason=WHY))

    assert result.consolidated is not None
    assert result.consolidated.issues == 1
    assert result.consolidated.mutations == 0
    assert forge.mutations == [], f"import mutated upstream: {forge.mutations}"


def test_a_repository_the_token_cannot_see_is_not_imported(
    importing: tuple[AppContext, Path], forge: Forge
) -> None:
    """404 is "no such repository, to us" — checked before anything clones."""
    context, _ = importing
    forge.exists = False
    with pytest.raises(NotFound):
        import_project(context, ImportProjectParams(repo=REPO, reason=WHY))
    assert list_projects(context, ListProjectsParams()).total == 0


def test_without_the_adapter_the_import_still_works_and_says_so(
    importing: tuple[AppContext, Path],
) -> None:
    """The core never requires the optional adapter (NFR-PO1).

    A public repository clones perfectly well without a token; what is
    missing is the consolidation, and the result says that rather than
    pretending the repository had no issues.
    """
    context, _ = importing
    result = import_project(context, ImportProjectParams(repo=REPO, reason=WHY))
    assert result.consolidated is None
    assert result.detail is not None
    assert "not collected" in result.detail


# -- the cloner itself, against a real git ---------------------------------


def _git(cwd: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


@pytest.fixture
def origin(tmp_path: Path) -> Path:
    """A real repository on disk, so `clone_repository` faces a real git."""
    source = tmp_path / "origin"
    source.mkdir()
    _git(source, "init", "--initial-branch", "main")
    (source / "README.md").write_text("origin\n", encoding="utf-8")
    _git(source, "add", "README.md")
    _git(source, "-c", "user.email=t@t", "-c", "user.name=t", "commit", "-m", "first")
    return source


def test_clone_puts_a_working_tree_where_it_was_told(
    origin: Path, tmp_path: Path
) -> None:
    destination = tmp_path / "cloned" / "here"
    outcome = clone_repository(
        CloneRequest(remote=str(origin), destination=destination)
    )

    assert (destination / "README.md").read_text(encoding="utf-8") == "origin\n"
    assert outcome.revision is not None and len(outcome.revision) == 40
    assert outcome.default_branch == "main"
    assert outcome.reused is False


def test_a_second_import_of_the_same_remote_reuses_the_checkout(
    origin: Path, tmp_path: Path
) -> None:
    """FR-P7: re-importing registers what is there rather than re-cloning."""
    destination = tmp_path / "cloned"
    first = clone_repository(CloneRequest(remote=str(origin), destination=destination))
    (destination / "local-work.txt").write_text("mine\n", encoding="utf-8")

    second = clone_repository(CloneRequest(remote=str(origin), destination=destination))

    assert second.reused is True
    assert second.revision == first.revision
    assert (destination / "local-work.txt").exists(), "the import ate local work"


def test_a_destination_holding_a_different_repository_is_refused(
    origin: Path, tmp_path: Path
) -> None:
    destination = tmp_path / "cloned"
    clone_repository(CloneRequest(remote=str(origin), destination=destination))

    with pytest.raises(Conflict):
        clone_repository(
            CloneRequest(
                remote="https://github.com/someone/else.git", destination=destination
            )
        )
    assert (destination / "README.md").exists(), "a refused import touched the tree"


def test_an_instance_without_git_says_so_instead_of_blaming_the_checkout(
    origin: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#21: the guard has to run before anything asks git a question.

    Production shipped an image with no git. Importing into a checkout of
    exactly the right remote failed as `conflict: ... is a clone of an
    unknown remote`, because reading the origin URL came back empty and
    empty was indistinguishable from "this checkout has no origin". The
    message sent the reader to inspect a correct working tree.
    """
    destination = tmp_path / "cloned"
    clone_repository(CloneRequest(remote=str(origin), destination=destination))

    monkeypatch.setattr("vogt.adapters.git.clone.shutil.which", lambda _: None)

    with pytest.raises(GitUnavailable, match="git is not installed"):
        clone_repository(CloneRequest(remote=str(origin), destination=destination))


def test_a_git_that_cannot_be_run_is_not_read_as_an_empty_answer(
    origin: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """#21, the other half: `_read` tolerated far more than it meant to.

    Its contract is "a question the checkout has no answer to" — no commits,
    no origin. git failing to execute is not that, and returning `None` for
    it manufactures the same empty answer a real one produces. Modelled with
    the error a missing binary actually raises, past the `which` guard, so
    the tolerance is tested rather than the guard.
    """
    destination = tmp_path / "cloned"
    clone_repository(CloneRequest(remote=str(origin), destination=destination))

    def _no_binary(*_args: object, **_kwargs: object) -> None:
        raise FileNotFoundError(2, "No such file or directory: 'git'")

    monkeypatch.setattr("vogt.adapters.git.clone.subprocess.run", _no_binary)

    with pytest.raises(GitUnavailable):
        clone_repository(CloneRequest(remote=str(origin), destination=destination))


def test_a_checkout_with_no_origin_is_still_a_conflict(
    origin: Path, tmp_path: Path
) -> None:
    """The tolerance #21 narrowed is still there for the case it was for.

    Narrowing `_read` must not turn "git answered, and the answer is that
    there is no origin" into an error. That answer is a real one, and it
    still means the destination is not this remote's clone.
    """
    destination = tmp_path / "cloned"
    clone_repository(CloneRequest(remote=str(origin), destination=destination))
    _git(destination, "remote", "remove", "origin")

    with pytest.raises(Conflict, match="unknown remote"):
        clone_repository(CloneRequest(remote=str(origin), destination=destination))


def test_a_destination_holding_unrelated_files_is_refused(tmp_path: Path) -> None:
    """The single most destructive thing an import could do, prevented."""
    destination = tmp_path / "not-a-repo"
    destination.mkdir()
    (destination / "important.txt").write_text("do not lose me\n", encoding="utf-8")

    with pytest.raises(Conflict):
        clone_repository(
            CloneRequest(remote="https://github.com/o/n.git", destination=destination)
        )
    assert (destination / "important.txt").exists()


def test_the_token_never_reaches_the_command_line_or_the_config(
    origin: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """FR-S8, asserted at the two places a credential is normally left.

    Every `git` invocation's argv is recorded, and the resulting clone's own
    configuration is read back. A token in either is a token in a process
    listing or in a file that outlives the import.
    """
    recorded: list[list[str]] = []
    real_run = subprocess.run

    def recording(args: Any, **kwargs: Any) -> Any:
        recorded.append(list(args))
        return real_run(args, **kwargs)

    monkeypatch.setattr(subprocess, "run", recording)

    destination = tmp_path / "cloned"
    clone_repository(
        CloneRequest(
            remote=str(origin), destination=destination, token="ghp_supersecret"
        )
    )

    flat = " ".join(" ".join(argv) for argv in recorded)
    assert "ghp_supersecret" not in flat, "the token reached a command line"
    assert "ghp_supersecret" not in (destination / ".git" / "config").read_text(
        encoding="utf-8"
    ), "the token was written into the clone's configuration"
    assert any("GIT_ASKPASS" not in part for argv in recorded for part in argv)


def test_the_askpass_helper_does_not_outlive_the_clone(
    origin: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A credential helper left on disk is a credential left on disk."""
    seen: list[str] = []
    real_run = subprocess.run

    def recording(args: Any, **kwargs: Any) -> Any:
        env = kwargs.get("env") or {}
        if "GIT_ASKPASS" in env:
            seen.append(env["GIT_ASKPASS"])
        return real_run(args, **kwargs)

    monkeypatch.setattr(subprocess, "run", recording)
    clone_repository(
        CloneRequest(remote=str(origin), destination=tmp_path / "cloned", token="ghp_x")
    )

    assert seen, "no askpass helper was used, so the token went somewhere else"
    for path in seen:
        assert not Path(path).exists(), f"{path} survived the clone"


def test_a_failure_message_does_not_repeat_a_credential(tmp_path: Path) -> None:
    """An error message is a place secrets escape to logs.

    Vogt's own remotes never carry a credential, but a caller can pass a URL
    that does, and git echoes the remote back in most failures.
    """
    from vogt.adapters.git.clone import GitUnavailable

    with pytest.raises(GitUnavailable) as raised:
        clone_repository(
            CloneRequest(
                remote="https://user:hunter2@example.invalid/o/n.git",
                destination=tmp_path / "nope",
            )
        )
    assert "hunter2" not in str(raised.value)


def test_an_empty_destination_is_cloned_into(origin: Path, tmp_path: Path) -> None:
    """Empty is not occupied — refusing it would fail every second attempt."""
    destination = tmp_path / "empty"
    destination.mkdir()

    outcome = clone_repository(
        CloneRequest(remote=str(origin), destination=destination)
    )
    assert outcome.reused is False
    assert (destination / "README.md").exists()
