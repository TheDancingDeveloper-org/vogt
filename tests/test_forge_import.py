"""`forge.import` — turn a picker-listed repository into a project (#344).

`forge.repos` lists what a credential can see; `forge.import` is the verb that
turns one of those rows into a project — clone under the acting credential,
register, consolidate — so afterwards `forge.link`/`forge.writeback` operate on
it. The properties under test:

- it clones under the *actor's* linked credential (the one that could see the
  repository to list it), not the instance file token;
- it registers the project linked and consolidates its forge state, so the
  imported project arrives as upstream-truth;
- it composes with `project.import` (the clone/register/consolidate is shared);
- no credential, or a non-github host, is a typed refusal rather than a lie.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from cryptography.fernet import Fernet

from vogt.adapters.git import CloneOutcome, CloneRequest
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    ForgeAccountLinkParams,
    ForgeImportParams,
    InitParams,
    ListProjectsParams,
    SetWriteBackParams,
)
from vogt.application.services import (
    import_forge_repo,
    init_instance,
    link_forge_account,
    list_projects,
    set_write_back,
)
from vogt.config import VogtConfig
from vogt.errors import InvalidRequest

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "importing from the picker"


class ImportForge:
    """A forge that answers everything `forge.import` reads, recording tokens.

    `/user` and `/user/repos` for the picker's credential resolution, the bare
    repository read for `describe`, one issue for the consolidation sync, and
    empty lists for the other current-state reads.
    """

    def __init__(self, login: str = "octo-actor") -> None:
        self.login = login
        self.calls: list[tuple[str, str, str | None]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        self.calls.append((method, url, headers.get("Authorization")))
        if method == "GET" and url.endswith("/user"):
            return 200, json.dumps({"login": self.login}).encode()
        if method == "GET" and "/user/repos" in url:
            return 200, json.dumps(
                [
                    {
                        "name": "rustnzb",
                        "owner": {"login": self.login},
                        "default_branch": "main",
                        "private": True,
                        "html_url": f"https://github.com/{self.login}/rustnzb",
                    }
                ]
            ).encode()
        if method == "GET" and "/issues" in url and "/comments" not in url:
            return 200, json.dumps(
                [
                    {
                        "number": 7,
                        "title": "an existing issue",
                        "state": "open",
                        "labels": [],
                        "comments": 0,
                        "updated_at": "2026-08-01T00:00:07Z",
                        "html_url": (
                            f"https://github.com/{self.login}/rustnzb/issues/7"
                        ),
                    }
                ]
            ).encode()
        if method == "GET" and (
            "/pulls" in url
            or "/labels" in url
            or "/releases" in url
            or "/notifications" in url
        ):
            return 200, b"[]"
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode()
        if "/vulnerability-alerts" in url or "/automated-security-fixes" in url:
            return 404, b""
        if method == "GET" and "/repos/" in url:
            tail = url.split("/repos/", 1)[1].split("?", 1)[0]
            if tail.count("/") == 1:
                return 200, json.dumps({"default_branch": "main"}).encode()
        return 404, b""


class RecordingCloner:
    """A clone that records the request (its token especially) and writes a tree."""

    def __init__(self) -> None:
        self.requests: list[CloneRequest] = []

    def __call__(self, request: CloneRequest) -> CloneOutcome:
        self.requests.append(request)
        request.destination.mkdir(parents=True, exist_ok=True)
        (request.destination / "README.md").write_text("hi\n", encoding="utf-8")
        return CloneOutcome(
            destination=request.destination, revision="c" * 40, default_branch="main"
        )


def _key_file(tmp_path: Path) -> Path:
    path = tmp_path / "forge_account_key"
    path.write_bytes(Fernet.generate_key())
    return path


def _instance(
    tmp_path: Path,
    *,
    cloner: RecordingCloner,
    key_file: Path | None = None,
    github_token_file: Path | None = None,
    forge_transport: Any = None,
) -> AppContext:
    ctx = build_context(
        config=VogtConfig(
            data_dir=tmp_path / "instance",
            import_root=tmp_path / "estate",
            sqlite_synchronous="off",
            forge_account_key_file=key_file,
            github_token_file=github_token_file,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        cloner=cloner,
        forge_transport=forge_transport,
    )
    init_instance(ctx, InitParams())
    return ctx


def _file_token(tmp_path: Path) -> Path:
    path = tmp_path / "github_token"
    path.write_text("ghp_file_token", encoding="utf-8")
    return path


# -- the happy path --------------------------------------------------------


def test_forge_import_clones_registers_and_consolidates(tmp_path: Path) -> None:
    """The picker pick: named, cloned, registered linked, forge state read."""
    forge = ImportForge()
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        key_file=_key_file(tmp_path),
        github_token_file=_file_token(tmp_path),
        forge_transport=forge,
    )
    link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_actor_pat", reason=WHY))

    result = import_forge_repo(
        ctx, ForgeImportParams(owner="octo-actor", name="rustnzb", reason=WHY)
    )

    assert result.project.slug == "rustnzb"
    assert result.project.repo_url == "https://github.com/octo-actor/rustnzb"
    assert Path(result.root_path).is_dir()
    assert result.cloned is True
    # Registered as upstream-truth: linked, and the consolidation read the
    # repository's existing issue (FR-B3), so it does not arrive looking empty.
    assert result.project.link_state == "linked"
    assert result.consolidated is not None and result.consolidated.issues == 1

    listed = list_projects(ctx, ListProjectsParams())
    assert [p.slug for p in listed.projects] == ["rustnzb"]


def test_forge_import_clones_under_the_actors_credential(tmp_path: Path) -> None:
    """The clone runs under the linked PAT, not the instance file token (#344).

    The credential that could *see* the repository to list it is the one the
    clone must authenticate with — otherwise a private repository the actor's
    PAT reaches, but the file token does not, would fail to clone.
    """
    forge = ImportForge()
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        key_file=_key_file(tmp_path),
        github_token_file=_file_token(tmp_path),
        forge_transport=forge,
    )
    link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_actor_pat", reason=WHY))

    import_forge_repo(
        ctx, ForgeImportParams(owner="octo-actor", name="rustnzb", reason=WHY)
    )

    assert [r.token for r in cloner.requests] == ["ghp_actor_pat"], (
        "the clone authenticated with the actor's linked PAT, not the file token"
    )


def test_forge_import_falls_back_to_the_file_token(tmp_path: Path) -> None:
    """With no linked account, the instance file token clones (FR-S7)."""
    forge = ImportForge()
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        github_token_file=_file_token(tmp_path),
        forge_transport=forge,
    )

    result = import_forge_repo(
        ctx, ForgeImportParams(owner="octo-actor", name="rustnzb", reason=WHY)
    )

    assert result.project.link_state == "linked"
    assert [r.token for r in cloner.requests] == ["ghp_file_token"]


def test_forge_import_takes_a_display_name_override(tmp_path: Path) -> None:
    forge = ImportForge()
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        github_token_file=_file_token(tmp_path),
        forge_transport=forge,
    )

    result = import_forge_repo(
        ctx,
        ForgeImportParams(
            owner="octo-actor",
            name="rustnzb",
            display_name="My RustNZB",
            reason=WHY,
        ),
    )

    assert result.project.name == "My RustNZB"
    assert result.project.slug == "my-rustnzb"


def test_forge_import_leaves_it_ready_for_writeback(tmp_path: Path) -> None:
    """The composition the issue asks for: after import, `forge writeback`
    operates on the project, because it came back linked (#344)."""
    forge = ImportForge()
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        github_token_file=_file_token(tmp_path),
        forge_transport=forge,
    )
    imported = import_forge_repo(
        ctx, ForgeImportParams(owner="octo-actor", name="rustnzb", reason=WHY)
    )

    armed = set_write_back(
        ctx,
        SetWriteBackParams(project=imported.project.slug, policy="full", reason=WHY),
    )
    assert armed.project.write_back == "full"  # type: ignore[attr-defined]


# -- the refusals ----------------------------------------------------------


def test_forge_import_without_a_credential_is_refused(tmp_path: Path) -> None:
    """No linked account and no file token: a typed refusal, not a lie."""
    cloner = RecordingCloner()
    ctx = _instance(tmp_path, cloner=cloner, forge_transport=ImportForge())

    with pytest.raises(InvalidRequest, match="no forge credential"):
        import_forge_repo(
            ctx, ForgeImportParams(owner="octo-actor", name="rustnzb", reason=WHY)
        )
    assert cloner.requests == [], "nothing was cloned when there was no credential"


def test_forge_import_rejects_a_non_github_host(tmp_path: Path) -> None:
    cloner = RecordingCloner()
    ctx = _instance(
        tmp_path,
        cloner=cloner,
        github_token_file=_file_token(tmp_path),
        forge_transport=ImportForge(),
    )

    with pytest.raises(InvalidRequest, match="only in v1"):
        import_forge_repo(
            ctx,
            ForgeImportParams(
                owner="octo-actor", name="rustnzb", host="gitlab.com", reason=WHY
            ),
        )
