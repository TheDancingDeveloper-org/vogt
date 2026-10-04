"""`clone`: another instance's backup restored here as a copy, not as it.

The scenario these tests stand in for is the one the operation exists for:
take a backup of production, clone it into development, and keep working on
development without development ever acting as production. So every test
builds two real instances in two data directories — a *source* and a
*target* — and checks the four things a plain `restore` would get wrong:
whose credentials work, whether forge write-back is armed, whose push
subscriptions come along, and which instance the result says it is.
"""

from __future__ import annotations

import dataclasses
import json
from collections.abc import Callable
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BackupParams,
    CloneParams,
    CreateActorParams,
    CreateUserParams,
    CreateWorkParams,
    InitParams,
    IssueTokenParams,
    ListWorkParams,
    LoginParams,
    RegisterProjectParams,
    RestoreParams,
    StatusParams,
)
from vogt.application.services import (
    backup,
    clone,
    create_actor,
    create_user,
    create_work,
    init_instance,
    issue_token,
    list_work,
    login,
    register_project,
    restore,
    status,
)
from vogt.application.services.auth import Unauthenticated, authenticate
from vogt.application.services.lifecycle import MANIFEST_NAME
from vogt.config import VogtConfig
from vogt.core.auth import hash_token
from vogt.core.entities import CodingSession, Token
from vogt.errors import InvalidRequest
from vogt.storage.interface import ProjectUpdate
from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.declared import MIGRATIONS_DIR as DECLARED_MIGRATIONS
from vogt.storage.sqlite.declared import SqliteWriteTxn
from vogt.storage.sqlite.migrator import Migrator, load_migrations

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "clone test"


class PrefixedIds(SequentialIds):
    """Ids that cannot collide with another instance's, as ULIDs do not."""

    def __init__(self, tag: str) -> None:
        super().__init__()
        self._tag = tag

    def __call__(self, prefix: str) -> str:
        return f"{self._tag}{super().__call__(prefix)}"


class CollidingIds(SequentialIds):
    """Two fresh instances whose actor and token ids collide.

    Those are the rows a clone carries across, so this is the worst case for
    it: each carried actor and token id already names something else in the
    copy. Every other id is tagged per instance, as independent ULIDs are.
    """

    def __init__(self, tag: str) -> None:
        super().__init__()
        self._tag = tag

    def __call__(self, prefix: str) -> str:
        raw = super().__call__(prefix)
        return raw if prefix in {"act", "tok"} else f"{self._tag}{raw}"


def _instance(
    tmp_path: Path,
    name: str,
    *,
    ids: Callable[[str], str] | None = None,
    engine_state_dir: Path | None = None,
) -> AppContext:
    context = build_context(
        config=VogtConfig(
            data_dir=tmp_path / name,
            sqlite_synchronous="off",
            engine_state_dir=engine_state_dir,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=ids or CollidingIds(name),
    )
    init_instance(context, InitParams())
    return context


def _issue(ctx: AppContext, identity_ref: str, scopes: str = "work.write") -> str:
    """Create the agent actor, then mint a token for it; return the secret."""
    with ctx.declared.read() as view:
        exists = view.actor_by_identity(identity_ref) is not None
    if not exists:
        create_actor(
            ctx,
            CreateActorParams(
                identity_ref=identity_ref, display_name=identity_ref, reason=WHY
            ),
        )
    return issue_token(
        ctx,
        IssueTokenParams(actor=identity_ref, name="token", scopes=scopes, reason=WHY),
    ).secret


def _arm_write_back(ctx: AppContext, slug: str) -> None:
    with ctx.declared.write() as txn:
        project = txn.project_by_slug(slug)
        assert project is not None
        txn.update_project(project.id, ProjectUpdate(write_back="full"), at=ctx.clock())


def _link_forge_account(ctx: AppContext, identity_ref: str, login_name: str) -> None:
    with ctx.declared.write() as txn:
        actor = txn.actor_by_identity(identity_ref)
        assert actor is not None
        txn.upsert_forge_account(
            actor_id=actor.id,
            host="github.com",
            login=login_name,
            scopes="repo",
            encrypted_token=f"ciphertext-of-{login_name}",
            at=ctx.clock(),
        )


def _open_session(ctx: AppContext, slug: str) -> None:
    with ctx.declared.write() as txn:
        project = txn.project_by_slug(slug)
        actor = txn.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert project is not None
        assert actor is not None
        txn.insert_session(
            CodingSession(
                id="ses_source_1",
                engine_session_id="engine-on-the-source",
                project_id=project.id,
                actor_id=actor.id,
                cwd=project.root_path,
                reason=WHY,
                started_at=ctx.clock(),
            )
        )


@dataclasses.dataclass
class Pair:
    source: AppContext
    target: AppContext
    backup_path: str
    source_secret: str
    target_secret: str
    source_instance_id: str
    target_instance_id: str


def _pair(
    tmp_path: Path,
    *,
    source_ids: Callable[[str], str] | None = None,
    target_ids: Callable[[str], str] | None = None,
    target_engine_state: Path | None = None,
    source_engine_state: Path | None = None,
) -> Pair:
    source = _instance(
        tmp_path, "source", ids=source_ids, engine_state_dir=source_engine_state
    )
    register_project(
        source,
        RegisterProjectParams(
            name="Shipped", root_path=str(tmp_path / "shipped"), reason=WHY
        ),
    )
    _arm_write_back(source, "shipped")
    create_work(
        source,
        CreateWorkParams(
            kind="bug",
            title="Found in production",
            project="shipped",
            local_only=True,
            reason=WHY,
        ),
    )
    source_secret = _issue(source, "agent:prod-bot")
    create_user(
        source,
        CreateUserParams(username="prodlogin", password="prod-password", reason=WHY),
    )
    _link_forge_account(source, TEST_PRINCIPAL.identity_ref, "prod-github")
    _open_session(source, "shipped")
    taken = backup(
        source,
        BackupParams(destination=str(tmp_path / "prod-backup"), reason="before clone"),
    )

    target = _instance(
        tmp_path, "target", ids=target_ids, engine_state_dir=target_engine_state
    )
    target_secret = _issue(target, "agent:dev-bot")
    create_user(
        target,
        CreateUserParams(username="devlogin", password="dev-password", reason=WHY),
    )
    _link_forge_account(target, TEST_PRINCIPAL.identity_ref, "dev-github")

    with source.declared.read() as view:
        source_instance_id = view.instance_id()
    with target.declared.read() as view:
        target_instance_id = view.instance_id()
    assert source_instance_id != target_instance_id
    return Pair(
        source=source,
        target=target,
        backup_path=taken.path,
        source_secret=source_secret,
        target_secret=target_secret,
        source_instance_id=source_instance_id,
        target_instance_id=target_instance_id,
    )


def _clone(pair: Pair, **extra: object) -> object:
    return clone(
        pair.target,
        CloneParams(source=pair.backup_path, confirm=True, reason=WHY, **extra),  # type: ignore[arg-type]
    )


# -- the end-to-end promise --------------------------------------------------


@pytest.mark.parametrize(
    ("source_ids", "target_ids"),
    [
        # Independent ids, as ULIDs are in production.
        (PrefixedIds("p"), PrefixedIds("d")),
        # Colliding ids — two fresh instances counting from one — so the
        # re-keying of a carried row whose id the copy already uses is proved.
        (CollidingIds("source"), CollidingIds("target")),
    ],
    ids=["independent-ids", "colliding-ids"],
)
def test_a_clone_brings_the_data_and_keeps_the_targets_credentials(
    tmp_path: Path,
    source_ids: Callable[[str], str],
    target_ids: Callable[[str], str],
) -> None:
    pair = _pair(tmp_path, source_ids=source_ids, target_ids=target_ids)
    result = clone(
        pair.target, CloneParams(source=pair.backup_path, confirm=True, reason=WHY)
    )
    target = pair.target

    # The data came across.
    titles = [item.title for item in list_work(target, ListWorkParams()).items]
    assert titles == ["Found in production"]

    # The target's own token still works, as the target's actor.
    who = authenticate(target, bearer=pair.target_secret)
    assert who.principal.identity_ref == "agent:dev-bot"
    # The source's token does not.
    with pytest.raises(Unauthenticated):
        authenticate(target, bearer=pair.source_secret)

    # Password logins: the target's works, the source's is gone.
    signed_in = login(target, LoginParams(username="devlogin", password="dev-password"))
    assert signed_in.actor.identity_ref == "human:devlogin"
    with pytest.raises(Unauthenticated):
        login(target, LoginParams(username="prodlogin", password="prod-password"))

    # Linked forge accounts: the target's, not the source's.
    with target.declared.read() as view:
        actor = view.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert actor is not None
        logins = [a.login for a in view.forge_accounts_for_actor(actor.id)]
        shipped = view.project_by_slug("shipped")
        live_sessions = view.list_sessions(limit=10, offset=0)
    assert logins == ["dev-github"]

    # Forge write-back is disarmed.
    assert shipped is not None
    assert shipped.write_back == "none"
    assert result.write_back_reset == ["shipped"]

    # The source's running session names a process on the source's engine.
    assert live_sessions == []
    assert result.sessions_closed == 1

    # The target is still the target, and says what it is a copy of.
    assert result.instance_id == pair.target_instance_id
    assert result.source_instance_id == pair.source_instance_id
    assert result.tokens_kept >= 1
    assert result.source_tokens_revoked >= 1
    assert result.source_password_logins_dropped == 1
    assert result.password_logins_kept == 1
    assert result.source_forge_accounts_dropped == 1
    assert result.forge_accounts_kept == 1
    reported = status(target, StatusParams())
    assert reported.instance_id == pair.target_instance_id
    assert reported.clone is not None
    assert reported.clone.source_instance_id == pair.source_instance_id
    assert target.observed.instance_id() == pair.target_instance_id


def test_a_clone_is_audited_and_evented(tmp_path: Path) -> None:
    pair = _pair(tmp_path)
    _clone(pair)
    with pair.target.declared.read() as view:
        audit = view.list_audit(limit=5, offset=0)
        events = view.list_events(after=0, limit=10_000)
    assert audit[0].operation == "clone"
    assert audit[0].entity_kind == "instance"
    assert audit[0].entity_id == pair.target_instance_id
    assert audit[0].reason == WHY
    cloned = [e for e in events if e.kind == "instance.cloned"]
    assert len(cloned) == 1
    assert cloned[0].summary["source_instance_id"] == pair.source_instance_id


def test_the_source_tokens_are_revoked_not_deleted(tmp_path: Path) -> None:
    """`auth_decisions` names token ids, so the rows stay, dead."""
    pair = _pair(tmp_path)
    _clone(pair)
    with pair.target.declared.read() as view:
        row = view.token_by_hash(hash_token(pair.source_secret))
    assert row is not None
    assert row.revoked_at is not None
    assert row.revoked_reason is not None
    assert pair.source_instance_id in row.revoked_reason


def test_a_secret_both_instances_hold_stays_live_as_the_targets(
    tmp_path: Path,
) -> None:
    """One stack secret on both sides is the same token on both sides.

    The copy keeps the source's row (its id is what the history names) and
    makes it say what the target's row said: live, and the target's actor.
    """
    pair = _pair(tmp_path, source_ids=PrefixedIds("p"), target_ids=PrefixedIds("d"))
    shared = "shared-stack-secret-0123456789abcdef"
    for ctx, owner in ((pair.source, "agent:prod-core"), (pair.target, "agent:core")):
        _issue(ctx, owner, "read")
        with ctx.declared.write() as txn:
            actor = txn.actor_by_identity(owner)
            assert actor is not None
            txn.insert_token(
                Token(
                    id=ctx.id_factory("tok"),
                    actor_id=actor.id,
                    name="stack secret",
                    scopes=["read"],
                    created_at=ctx.clock(),
                ),
                token_hash=hash_token(shared),
            )
    taken = backup(
        pair.source,
        BackupParams(destination=str(tmp_path / "second"), reason="again"),
    )
    clone(pair.target, CloneParams(source=taken.path, confirm=True, reason=WHY))
    who = authenticate(pair.target, bearer=shared)
    assert who.principal.identity_ref == "agent:core"


# -- what a clone refuses, and what it leaves alone ---------------------------


def test_a_clone_refuses_without_confirmation_and_touches_nothing(
    tmp_path: Path,
) -> None:
    pair = _pair(tmp_path)
    with pytest.raises(InvalidRequest, match="--confirm"):
        clone(pair.target, CloneParams(source=pair.backup_path, reason=WHY))
    assert list_work(pair.target, ListWorkParams()).items == []
    assert status(pair.target, StatusParams()).clone is None


def test_a_backup_of_this_instance_is_a_restore_not_a_clone(tmp_path: Path) -> None:
    pair = _pair(tmp_path)
    with pytest.raises(InvalidRequest, match="Use `restore`"):
        clone(
            pair.source,
            CloneParams(source=pair.backup_path, confirm=True, reason=WHY),
        )


def test_a_clone_refuses_a_backup_from_the_future(tmp_path: Path) -> None:
    pair = _pair(tmp_path)
    manifest_path = Path(pair.backup_path) / MANIFEST_NAME
    manifest = json.loads(manifest_path.read_text("utf-8"))
    manifest["declared_schema_version"] = 10_000
    manifest_path.write_text(json.dumps(manifest), "utf-8")
    with pytest.raises(InvalidRequest, match="forward-only"):
        _clone(pair)
    assert list_work(pair.target, ListWorkParams()).items == []


def test_a_failure_while_sanitising_leaves_the_live_stores_untouched(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The copy is sanitised in staging; the swap happens only after."""
    pair = _pair(tmp_path)

    def explode(*_: object, **__: object) -> None:
        raise RuntimeError("disk on fire")

    monkeypatch.setattr(SqliteWriteTxn, "set_instance_identity", explode)
    with pytest.raises(RuntimeError, match="disk on fire"):
        _clone(pair)

    assert list_work(pair.target, ListWorkParams()).items == []
    who = authenticate(pair.target, bearer=pair.target_secret)
    assert who.principal.identity_ref == "agent:dev-bot"
    data_dir = pair.target.config.resolved_data_dir
    assert not list(data_dir.glob(".clone-staging-*")), "staging is cleaned up"


# -- engine state ---------------------------------------------------------------


def _engine_states(tmp_path: Path) -> tuple[Path, Path]:
    source_state = tmp_path / "source-engine"
    (source_state / "session-logs").mkdir(parents=True)
    (source_state / "session-logs" / "one.log").write_text("prod output", "utf-8")
    (source_state / "history.db").write_text("prod history", "utf-8")
    (source_state / "push.json").write_text("prod phones", "utf-8")
    (source_state / "agent-tasks.json").write_text("prod schedule", "utf-8")
    target_state = tmp_path / "target-engine"
    target_state.mkdir()
    (target_state / "push.json").write_text("dev phones", "utf-8")
    return source_state, target_state


def test_a_clone_leaves_the_engine_state_alone_by_default(tmp_path: Path) -> None:
    source_state, target_state = _engine_states(tmp_path)
    pair = _pair(
        tmp_path,
        source_engine_state=source_state,
        target_engine_state=target_state,
    )
    result = clone(
        pair.target, CloneParams(source=pair.backup_path, confirm=True, reason=WHY)
    )
    assert "not copied" in result.engine_state
    assert sorted(p.name for p in target_state.iterdir()) == ["push.json"]


def test_a_clone_copies_session_history_but_never_push_or_tasks(
    tmp_path: Path,
) -> None:
    source_state, target_state = _engine_states(tmp_path)
    pair = _pair(
        tmp_path,
        source_engine_state=source_state,
        target_engine_state=target_state,
    )
    result = clone(
        pair.target,
        CloneParams(
            source=pair.backup_path,
            confirm=True,
            include_engine_state=True,
            reason=WHY,
        ),
    )
    assert (target_state / "history.db").read_text("utf-8") == "prod history"
    assert (target_state / "session-logs" / "one.log").read_text("utf-8")
    assert (target_state / "push.json").read_text("utf-8") == "dev phones"
    assert not (target_state / "agent-tasks.json").exists()
    assert "push subscriptions" in result.engine_state


# -- schema ---------------------------------------------------------------------


def test_a_backup_from_an_older_schema_is_migrated_in_staging(
    tmp_path: Path,
) -> None:
    """Same rule as restore: older comes forward, and the sanitising runs on
    the migrated copy — here across 0017, which adds the password table the
    carry writes into."""
    shipped = load_migrations(DECLARED_MIGRATIONS)
    old_dir = tmp_path / "old-build"
    old_dir.mkdir()
    for migration in shipped:
        if migration.number <= 16:
            (old_dir / f"{migration.id}.sql").write_text(migration.sql, "utf-8")

    old_backup = tmp_path / "old-backup"
    old_backup.mkdir()
    conn = connect(old_backup / "declared.sqlite3", create=True)
    Migrator(store="declared", directory=old_dir, holder="old/1").migrate(
        conn, now=StepClock()()
    )
    for key, value in (
        ("instance_id", "ins_old_prod"),
        ("revision", "0"),
        ("work_ref_seq", "0"),
        ("created_at", "2026-01-01T00:00:00Z"),
    ):
        conn.execute("INSERT INTO meta (key, value) VALUES (?, ?)", (key, value))
    conn.execute(
        "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, "
        "created_at) VALUES ('act_old', 'agent', 'old bot', 'agent:old', 0, "
        "'2026-01-01T00:00:00Z')"
    )
    conn.execute(
        "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, created_at) "
        "VALUES ('tok_old', 'act_old', 'old', ?, '[\"read\"]', "
        "'2026-01-01T00:00:00Z')",
        (hash_token("old-prod-secret-0123456789"),),
    )
    conn.commit()
    conn.close()

    # The observed store of a fresh instance stands in for the old one's.
    scratch = _instance(tmp_path, "scratch")
    taken = backup(
        scratch, BackupParams(destination=str(tmp_path / "scratch-b"), reason=WHY)
    )
    (old_backup / "observed.sqlite3").write_bytes(
        (Path(taken.path) / "observed.sqlite3").read_bytes()
    )
    manifest = json.loads((Path(taken.path) / MANIFEST_NAME).read_text("utf-8"))
    manifest.update({"instance_id": "ins_old_prod", "declared_schema_version": 16})
    (old_backup / MANIFEST_NAME).write_text(json.dumps(manifest), "utf-8")

    target = _instance(tmp_path, "target", ids=PrefixedIds("d"))
    secret = _issue(target, "agent:dev", "read")
    result = clone(
        target, CloneParams(source=str(old_backup), confirm=True, reason=WHY)
    )
    assert any("0017" in name for name in result.migrations_applied)
    assert authenticate(target, bearer=secret).principal.identity_ref == "agent:dev"
    with pytest.raises(Unauthenticated):
        authenticate(target, bearer="old-prod-secret-0123456789")
    assert status(target, StatusParams()).clone is not None


def test_a_plain_restore_still_takes_the_sources_identity(tmp_path: Path) -> None:
    """The contrast that makes `clone` necessary, pinned so it stays visible."""
    pair = _pair(tmp_path)
    result = restore(
        pair.target,
        RestoreParams(source=pair.backup_path, confirm=True, reason=WHY),
    )
    assert result.instance_id == pair.source_instance_id
    assert status(pair.target, StatusParams()).clone is None
