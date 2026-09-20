"""Adopting the operator-supplied brokered agent token at init (#726).

The session-side mirror of the core-token adoption: the same one-deploy shape,
with scopes taken from `agent_session_scopes` so both token paths share one
scope decision.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.application.services.auth import authenticate
from vogt.config import VogtConfig
from vogt.core.auth import hash_token
from vogt.errors import InvalidRequest

from .conftest import TEST_PRINCIPAL, SequentialIds, StepClock

SECRET = "vogt_bootstrap_agent_secret_that_is_long_enough_0123456789"


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
    scopes: str = "read,work.write,project.write,writeback",
    actor: str = "agent:vogt-sessions",
) -> VogtConfig:
    return VogtConfig(
        data_dir=data_dir,
        sqlite_synchronous="off",
        bootstrap_agent_token_file=token_file,
        agent_session_scopes=scopes,
        bootstrap_agent_token_actor=actor,
    )


def test_the_supplied_agent_token_authenticates_after_init(tmp_path: Path) -> None:
    """One deploy, and the brokered session token works — no admin mint."""
    token_file = tmp_path / "agent-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(_config(tmp_path / "instance", token_file))

    result = init_instance(ctx, InitParams())
    assert result.bootstrap_agent_token == "adopted"
    assert authenticate(ctx, bearer=SECRET).principal is not None


def test_a_second_boot_with_the_same_value_changes_nothing(tmp_path: Path) -> None:
    token_file = tmp_path / "agent-token"
    token_file.write_text(SECRET, encoding="utf-8")
    cfg = _config(tmp_path / "instance", token_file)

    first = init_instance(_context(cfg), InitParams())
    assert first.bootstrap_agent_token == "adopted"

    second = init_instance(_context(cfg), InitParams())
    assert second.bootstrap_agent_token == "already_present"


def test_unset_leaves_the_old_behaviour_exactly_as_it_was(tmp_path: Path) -> None:
    ctx = _context(_config(tmp_path / "instance", None))
    assert init_instance(ctx, InitParams()).bootstrap_agent_token == "not_configured"


def test_the_agent_token_carries_agent_session_scopes(tmp_path: Path) -> None:
    """The one knob: the adopted agent token holds exactly agent_session_scopes,
    bound to its own actor (attribution is not shared, only the scope set)."""
    token_file = tmp_path / "agent-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(
        _config(
            tmp_path / "instance",
            token_file,
            scopes="read,work.write",
            actor="agent:vogt-prod-sessions",
        )
    )
    init_instance(ctx, InitParams())

    principal = authenticate(ctx, bearer=SECRET).principal
    assert principal.identity_ref == "agent:vogt-prod-sessions"
    with ctx.declared.read() as view:
        token = view.token_by_hash(hash_token(SECRET))
    assert token is not None
    assert set(token.scopes) == {"read", "work.write"}


def test_an_unknown_scope_fails_startup_rather_than_being_ignored(
    tmp_path: Path,
) -> None:
    token_file = tmp_path / "agent-token"
    token_file.write_text(SECRET, encoding="utf-8")
    ctx = _context(
        _config(tmp_path / "instance", token_file, scopes="read,not-a-scope")
    )
    with pytest.raises(InvalidRequest, match="not-a-scope"):
        init_instance(ctx, InitParams())


def test_a_missing_file_does_not_stop_the_instance_coming_up(tmp_path: Path) -> None:
    ctx = _context(_config(tmp_path / "instance", tmp_path / "absent"))
    result = init_instance(ctx, InitParams())
    assert result.bootstrap_agent_token == "not_configured"
    assert result.instance_id


def test_core_and_agent_tokens_coexist_in_one_deploy(tmp_path: Path) -> None:
    """Both paths adopted at one init: distinct secrets, distinct actors."""
    agent_file = tmp_path / "agent-token"
    agent_file.write_text(SECRET, encoding="utf-8")
    core_secret = "vogt_bootstrap_core_secret_that_is_long_enough_0123456789"
    core_file = tmp_path / "core-token"
    core_file.write_text(core_secret, encoding="utf-8")
    cfg = VogtConfig(
        data_dir=tmp_path / "instance",
        sqlite_synchronous="off",
        bootstrap_core_token_file=core_file,
        bootstrap_agent_token_file=agent_file,
    )

    result = init_instance(_context(cfg), InitParams())
    assert result.bootstrap_core_token == "adopted"
    assert result.bootstrap_agent_token == "adopted"
