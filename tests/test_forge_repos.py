"""The import repo picker (#180, design #178 decision 5).

Enumerate the repositories a credential can see so a person can pick which to
import. Four properties, each with its own test:

- the provider lists what `GET /user/repos` returns, provider-agnostically;
- the op uses the acting actor's linked PAT (#179) when they have one, and the
  instance file token otherwise;
- `already_registered` is computed against the declared project list;
- an unconfigured host lists nothing and says why, rather than lying "empty".
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from cryptography.fernet import Fernet

from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.github.client import GitHubClient
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    ForgeAccountLinkParams,
    ForgeReposParams,
    InitParams,
    RegisterProjectParams,
)
from vogt.application.services import (
    init_instance,
    link_forge_account,
    list_forge_repos,
    register_project,
)
from vogt.config import VogtConfig
from vogt.errors import InvalidRequest

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "repo picker test"

_REPOS_PAYLOAD = [
    {
        "name": "rustnzb",
        "owner": {"login": "octo-actor"},
        "default_branch": "trunk",
        "private": False,
        "html_url": "https://github.com/octo-actor/rustnzb",
    },
    {
        "name": "secret-lab",
        "owner": {"login": "octo-actor"},
        "default_branch": "main",
        "private": True,
        "html_url": "https://github.com/octo-actor/secret-lab",
    },
]


class ReposForge:
    """A forge that answers `/user` and `/user/repos`, recording every token."""

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
        if method == "GET" and "/user/repos" in url:
            return 200, json.dumps(_REPOS_PAYLOAD).encode("utf-8")
        if method == "GET" and url.endswith("/user"):
            return 200, json.dumps({"login": self.login}).encode("utf-8")
        return 404, b""

    @property
    def repo_tokens(self) -> list[str | None]:
        return [auth for method, url, auth in self.calls if "/user/repos" in url]


def _key_file(tmp_path: Path) -> Path:
    path = tmp_path / "forge_account_key"
    path.write_bytes(Fernet.generate_key())
    return path


def _instance(
    tmp_path: Path,
    *,
    key_file: Path | None = None,
    github_token_file: Path | None = None,
    forge_transport: Any = None,
) -> AppContext:
    config = VogtConfig(
        data_dir=tmp_path / "instance",
        sqlite_synchronous="off",
        forge_account_key_file=key_file,
        github_token_file=github_token_file,
    )
    ctx = build_context(
        config=config,
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        forge_transport=forge_transport,
    )
    init_instance(ctx, InitParams())
    return ctx


# -- the provider ----------------------------------------------------------


def test_the_provider_lists_repos_provider_agnostically() -> None:
    """`list_repos` normalises the GitHub payload into the shared shape."""
    forge = ReposForge()
    provider = GitHubProvider(GitHubClient(token="ghp_x", transport=forge))

    repos = list(provider.list_repos())

    assert [r.slug for r in repos] == ["octo-actor/rustnzb", "octo-actor/secret-lab"]
    assert repos[0].default_branch == "trunk"
    assert repos[0].visibility == "public"
    assert repos[1].visibility == "private"
    assert repos[1].web_url == "https://github.com/octo-actor/secret-lab"


# -- the operation ---------------------------------------------------------


def test_enumeration_uses_the_actors_linked_pat(tmp_path: Path) -> None:
    """A linked actor sees the repositories *their* PAT reaches (#179)."""
    forge = ReposForge(login="octo-actor")
    ctx = _instance(tmp_path, key_file=_key_file(tmp_path), forge_transport=forge)
    link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_actor_pat", reason=WHY))

    result = list_forge_repos(ctx, ForgeReposParams())

    assert [(r.owner, r.name) for r in result.repos] == [
        ("octo-actor", "rustnzb"),
        ("octo-actor", "secret-lab"),
    ]
    assert result.login == "octo-actor"
    assert forge.repo_tokens == ["Bearer ghp_actor_pat"], (
        "enumeration listed under the actor's linked PAT"
    )


def test_enumeration_falls_back_to_the_file_token(tmp_path: Path) -> None:
    """With no linked account, the instance file token enumerates (FR-S7)."""
    file_token = tmp_path / "github_token"
    file_token.write_text("ghp_file_token", encoding="utf-8")
    forge = ReposForge()
    ctx = _instance(
        tmp_path,
        key_file=_key_file(tmp_path),
        github_token_file=file_token,
        forge_transport=forge,
    )

    result = list_forge_repos(ctx, ForgeReposParams())

    assert [r.name for r in result.repos] == ["rustnzb", "secret-lab"]
    assert result.login is None
    assert forge.repo_tokens == ["Bearer ghp_file_token"]


def test_already_registered_is_computed_against_declared_projects(
    tmp_path: Path,
) -> None:
    """A repo this instance already has is flagged, so select-all can skip it.

    Matching is on the parsed identity, so an http URL registered against the
    `git@`/`.git` spelling still resolves to the same repository.
    """
    file_token = tmp_path / "github_token"
    file_token.write_text("ghp_file_token", encoding="utf-8")
    ctx = _instance(
        tmp_path, github_token_file=file_token, forge_transport=ReposForge()
    )
    register_project(
        ctx,
        RegisterProjectParams(
            name="rustnzb",
            root_path="/srv/rustnzb",
            repo_url="git@github.com:octo-actor/rustnzb.git",
            reason=WHY,
        ),
    )

    result = list_forge_repos(ctx, ForgeReposParams())

    flagged = {r.name: r.already_registered for r in result.repos}
    assert flagged == {"rustnzb": True, "secret-lab": False}


def test_no_credential_lists_nothing_and_says_why(tmp_path: Path) -> None:
    """An empty picker reads as 'not collected', never 'you have no repos'."""
    ctx = _instance(tmp_path)  # no key, no file token
    result = list_forge_repos(ctx, ForgeReposParams())
    assert result.repos == []
    assert result.detail is not None and "not collected" in result.detail


def test_unknown_forge_host_is_rejected(tmp_path: Path) -> None:
    ctx = _instance(tmp_path, forge_transport=ReposForge())
    with pytest.raises(InvalidRequest):
        list_forge_repos(ctx, ForgeReposParams(host="gitlab.com"))
