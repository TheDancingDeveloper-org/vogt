"""Per-actor forge account linking (#179, design #178 decision 4).

Four properties, each with its own test:

- the stored token round-trips through encryption and is never hashed;
- link / status / unlink work and no surface ever hands the token back;
- with no key file the feature is off, and says so with a typed error;
- a linked actor's write goes upstream under their PAT, an unlinked one's
  under the instance file token.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from cryptography.fernet import Fernet

from vogt.adapters.forge.accounts import ForgeAccountCipher, load_cipher
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    CreateWorkParams,
    ForgeAccountLinkParams,
    ForgeAccountStatusParams,
    ForgeAccountUnlinkParams,
    InitParams,
    RegisterProjectParams,
    SetWriteBackParams,
)
from vogt.application.services import (
    create_work,
    init_instance,
    link_forge_account,
    register_project,
    set_write_back,
    status_forge_account,
    unlink_forge_account,
    writeback,
)
from vogt.config import VogtConfig
from vogt.core.entities import WriteBackRecord
from vogt.errors import ForgeAccountsNotConfigured, InvalidRequest

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "forge account test"
REPO = "https://github.com/TheDancingDeveloper-org/rustnzb"
SUBJECT = "gh:TheDancingDeveloper-org/rustnzb#12"


class RecordingForge:
    """A forge that answers `/user` and records the token every call carried."""

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
            return 200, json.dumps({"login": self.login}).encode("utf-8")
        if method != "GET":
            return 200, json.dumps(
                {"number": 12, "html_url": f"{REPO}/issues/12"}
            ).encode("utf-8")
        return 200, b"[]"

    @property
    def mutation_tokens(self) -> list[str | None]:
        return [auth for method, _url, auth in self.calls if method != "GET"]


def _key_file(tmp_path: Path) -> Path:
    path = tmp_path / "forge_account_key"
    path.write_bytes(Fernet.generate_key())
    return path


def _instance(
    tmp_path: Path,
    *,
    key_file: Path | None,
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


# -- encryption ------------------------------------------------------------


def test_the_cipher_round_trips_and_never_stores_plaintext() -> None:
    cipher = ForgeAccountCipher(Fernet(Fernet.generate_key()))
    ciphertext = cipher.encrypt("ghp_secret_value")
    assert ciphertext != "ghp_secret_value", "the stored form is not the token"
    assert "ghp_secret_value" not in ciphertext
    assert cipher.decrypt(ciphertext) == "ghp_secret_value"


def test_a_wrong_key_cannot_open_a_stored_token() -> None:
    stored = ForgeAccountCipher(Fernet(Fernet.generate_key())).encrypt("ghp_x")
    other = ForgeAccountCipher(Fernet(Fernet.generate_key()))
    with pytest.raises(ForgeAccountsNotConfigured):
        other.decrypt(stored)


# -- the feature is off without a key --------------------------------------


def test_linking_without_a_key_file_is_a_typed_not_configured_error(
    tmp_path: Path,
) -> None:
    ctx = _instance(tmp_path, key_file=None, forge_transport=RecordingForge())
    with pytest.raises(ForgeAccountsNotConfigured):
        link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_actor", reason=WHY))


def test_load_cipher_is_not_configured_when_the_key_file_is_missing(
    tmp_path: Path,
) -> None:
    config = VogtConfig(
        data_dir=tmp_path / "i", forge_account_key_file=tmp_path / "absent"
    )
    with pytest.raises(ForgeAccountsNotConfigured):
        load_cipher(config)


# -- link / status / unlink ------------------------------------------------


def test_link_then_status_then_unlink_never_reveals_the_token(
    tmp_path: Path,
) -> None:
    forge = RecordingForge(login="octo-actor")
    ctx = _instance(tmp_path, key_file=_key_file(tmp_path), forge_transport=forge)

    linked = link_forge_account(
        ctx, ForgeAccountLinkParams(token="ghp_actor_pat", reason=WHY)
    )
    assert linked.linked is True
    assert linked.login == "octo-actor"
    assert linked.host == "github.com"
    # Structurally the result has no token field, and no dumped form carries it.
    assert "ghp_actor_pat" not in json.dumps(linked.model_dump(mode="json"))

    status = status_forge_account(ctx, ForgeAccountStatusParams())
    assert [a.login for a in status.accounts] == ["octo-actor"]
    assert "ghp_actor_pat" not in json.dumps(status.model_dump(mode="json"))

    # The stored column is ciphertext, not the token and not a plain hash of it.
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert actor is not None
        secret = view.forge_account_secret(actor_id=actor.id, host="github.com")
    assert secret is not None and secret != "ghp_actor_pat"
    assert load_cipher(ctx.config).decrypt(secret) == "ghp_actor_pat"

    unlinked = unlink_forge_account(ctx, ForgeAccountUnlinkParams(reason=WHY))
    assert unlinked.linked is False
    assert status_forge_account(ctx, ForgeAccountStatusParams()).accounts == []


def test_an_invalid_token_is_refused_and_stores_nothing(tmp_path: Path) -> None:
    class Rejecting(RecordingForge):
        def __call__(
            self,
            url: str,
            headers: dict[str, str],
            body: bytes = b"",
            method: str = "GET",
        ) -> tuple[int, bytes]:
            if url.endswith("/user"):
                return 401, b""
            return 200, b"[]"

    ctx = _instance(tmp_path, key_file=_key_file(tmp_path), forge_transport=Rejecting())
    with pytest.raises(InvalidRequest):
        link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_bad", reason=WHY))
    assert status_forge_account(ctx, ForgeAccountStatusParams()).accounts == []


def test_only_github_is_supported_in_v1(tmp_path: Path) -> None:
    ctx = _instance(
        tmp_path, key_file=_key_file(tmp_path), forge_transport=RecordingForge()
    )
    with pytest.raises(InvalidRequest):
        link_forge_account(
            ctx,
            ForgeAccountLinkParams(host="gitlab.com", token="x", reason=WHY),
        )


# -- write-path attribution ------------------------------------------------


def _attempt_comment(ctx: AppContext) -> WriteBackRecord:
    register_project(
        ctx,
        RegisterProjectParams(
            name="rustnzb", root_path="/srv/rustnzb", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(
        ctx, SetWriteBackParams(project="rustnzb", policy="comment_only", reason=WHY)
    )
    work = create_work(
        ctx,
        CreateWorkParams(kind="bug", title="upstream", project="rustnzb", reason=WHY),
    )
    with ctx.declared.read() as view:
        project = view.project_by_slug("rustnzb")
        actor = view.actor_by_identity(TEST_PRINCIPAL.identity_ref)
    assert project is not None and actor is not None
    return writeback.attempt(
        ctx,
        actor=actor,
        action="comment",
        project=project,
        item=work.item,
        subject_key=SUBJECT,
        reason=WHY,
        body="posted",
    )


def test_a_linked_actor_writes_under_their_own_pat(tmp_path: Path) -> None:
    forge = RecordingForge(login="octo-actor")
    ctx = _instance(tmp_path, key_file=_key_file(tmp_path), forge_transport=forge)
    link_forge_account(ctx, ForgeAccountLinkParams(token="ghp_actor_pat", reason=WHY))

    record = _attempt_comment(ctx)
    assert record.outcome == "succeeded"
    assert forge.mutation_tokens == ["Bearer ghp_actor_pat"], (
        "the upstream comment is authored under the actor's linked PAT"
    )
    assert record.detail is not None and "octo-actor" in record.detail


def test_an_unlinked_actor_falls_back_to_the_file_token(tmp_path: Path) -> None:
    file_token = tmp_path / "github_token"
    file_token.write_text("ghp_file_token", encoding="utf-8")
    forge = RecordingForge()
    ctx = _instance(
        tmp_path,
        key_file=_key_file(tmp_path),
        github_token_file=file_token,
        forge_transport=forge,
    )
    # No link performed for the acting actor.
    record = _attempt_comment(ctx)
    assert record.outcome == "succeeded"
    assert forge.mutation_tokens == ["Bearer ghp_file_token"], (
        "an unlinked actor uses the instance file token"
    )
    assert record.detail is not None and "instance file token" in record.detail
