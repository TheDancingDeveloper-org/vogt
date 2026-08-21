"""Adopting an operator-supplied core token at init (#199).

The point of the feature is that a fronted deployment reaches a working
`/api/vogt` in one deploy. These pin the three things that has to mean:
the secret works afterwards, a second boot is a no-op, and nothing about
the old mint-then-configure path changed.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.application.services.auth import authenticate
from vogt.config import VogtConfig
from vogt.core.auth import MIN_ADOPTED_SECRET_LEN, adopt, hash_token, issue
from vogt.errors import InvalidRequest

from .conftest import TEST_PRINCIPAL, SequentialIds, StepClock

SECRET = "vogt_bootstrap_secret_that_is_long_enough_0123456789"


def _context(cfg: VogtConfig) -> AppContext:
    return build_context(
        config=cfg,
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )


def _config(
    data_dir: Path,
    token_file: Path | None,
    *,
    scopes: str = "read,work.write,project.write",
    actor: str = "agent:vogt-engine",
) -> VogtConfig:
    return VogtConfig(
        data_dir=data_dir,
        sqlite_synchronous="off",
        bootstrap_core_token_file=token_file,
        bootstrap_core_token_scopes=scopes,
        bootstrap_core_token_actor=actor,
    )


def test_adopt_hashes_a_supplied_secret_rather_than_minting_one() -> None:
    credential = adopt(SECRET, ("read",))
    assert credential.secret == SECRET
    assert credential.token_hash == hash_token(SECRET)


def test_issue_still_mints_its_own() -> None:
    """The old path is untouched: `issue` invents the secret, as it always did."""
    a, b = issue(("read",)), issue(("read",))
    assert a.secret != b.secret
    assert a.secret.startswith("vogt_")


def test_a_short_secret_is_refused_rather_than_becoming_a_credential() -> None:
    with pytest.raises(ValueError, match="at least"):
        adopt("changeme", ("read",))
    assert len("changeme") < MIN_ADOPTED_SECRET_LEN


def test_the_supplied_token_authenticates_after_init(tmp_path: Path) -> None:
    """The whole point: one deploy, and the front door's token works."""
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))

    result = init_instance(ctx, InitParams())
    assert result.bootstrap_core_token == "adopted"

    assert authenticate(ctx, bearer=SECRET).principal is not None


def test_a_second_boot_with_the_same_value_changes_nothing(tmp_path: Path) -> None:
    """`init` runs on every container start, not only the first."""
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET, encoding="utf-8")
    cfg = _config(tmp_path / "instance", token_file)

    first = init_instance(_context(cfg), InitParams())
    assert first.bootstrap_core_token == "adopted"

    second = init_instance(_context(cfg), InitParams())
    assert second.bootstrap_core_token == "already_present"
    assert second.created is False


def test_trailing_whitespace_does_not_produce_a_different_token(
    tmp_path: Path,
) -> None:
    """A heredoc or `echo` leaves a newline; that must not change the secret."""
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET + "\n", encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))

    assert init_instance(ctx, InitParams()).bootstrap_core_token == "adopted"
    assert authenticate(ctx, bearer=SECRET).principal is not None


def test_unset_leaves_the_old_behaviour_exactly_as_it_was(tmp_path: Path) -> None:
    ctx = _context(_config(tmp_path / "instance", None))
    assert init_instance(ctx, InitParams()).bootstrap_core_token == "not_configured"


@pytest.mark.parametrize("content", ["", "   \n"])
def test_an_empty_file_is_not_configured_rather_than_an_error(
    tmp_path: Path, content: str
) -> None:
    """The pre_deploy hook writes an empty file when the value is unset."""
    token_file = tmp_path / "core-token"
    token_file.write_text(content, encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))
    assert init_instance(ctx, InitParams()).bootstrap_core_token == "not_configured"


def test_a_missing_file_does_not_stop_the_instance_coming_up(
    tmp_path: Path,
) -> None:
    """The instance is the thing that must boot; a credential is not worth
    taking the core down over."""
    ctx = _context(_config(tmp_path / "instance", tmp_path / "absent"))
    result = init_instance(ctx, InitParams())
    assert result.bootstrap_core_token == "not_configured"
    assert result.instance_id


def test_the_adopted_token_carries_the_configured_scopes(tmp_path: Path) -> None:
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(
        _config(
            tmp_path / "instance",
            token_file,
            scopes="read",
            actor="agent:front-door",
        )
    )
    init_instance(ctx, InitParams())

    principal = authenticate(ctx, bearer=SECRET).principal
    assert principal.identity_ref == "agent:front-door"
    with ctx.declared.read() as view:
        token = view.token_by_hash(hash_token(SECRET))
    assert token is not None
    assert token.scopes == ["read"]


def test_an_unknown_scope_fails_startup_rather_than_being_ignored(
    tmp_path: Path,
) -> None:
    """A misconfigured scope must not degrade to "no credential, quietly":
    that is a deployment that believes it is secured and is not."""
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(
        _config(
            tmp_path / "instance",
            token_file,
            scopes="read,not-a-scope",
        )
    )
    with pytest.raises(InvalidRequest, match="not-a-scope"):
        init_instance(ctx, InitParams())


def test_a_too_short_secret_fails_startup(tmp_path: Path) -> None:
    token_file = tmp_path / "core-token"
    token_file.write_text("changeme", encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))
    with pytest.raises(InvalidRequest, match="at least"):
        init_instance(ctx, InitParams())


def test_the_secret_never_reaches_the_audit_row(tmp_path: Path) -> None:
    """An audit row holding the credential is a leak with a timestamp."""
    token_file = tmp_path / "core-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))
    init_instance(ctx, InitParams())

    with ctx.declared.read() as view:
        rows = view.list_audit(limit=100)
    assert rows
    assert not any(SECRET in str(row.model_dump()) for row in rows)
