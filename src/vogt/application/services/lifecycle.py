"""Backup, restore, clone, export, import.

A backup snapshots **both** stores consistently and writes a manifest naming
the schema version of each. Restore verifies that manifest *before touching
anything* — because the failure this prevents is restoring a backup taken
under an older schema onto a newer binary and discovering it half way
through, with the live data already gone.

Backups use SQLite's own backup API rather than copying files: a copy taken
while a write is in flight is a torn database, and WAL mode makes that more
likely rather than less.

`clone` is `restore` for a backup of *another* instance: the data comes
across, the target's identity and credentials stay, and the copy is made
unable to act as its source (no source tokens, no armed forge write-back, no
source push subscriptions). See `clone` for the full list.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from vogt.application.context import AppContext, with_stores_at
from vogt.application.models import (
    BackupParams,
    BackupResult,
    CloneParams,
    CloneResult,
    ExportParams,
    ExportResult,
    ImportParams,
    ImportResult,
    RestoreParams,
    RestoreResult,
)
from vogt.application.services.instance_merge import merge_export
from vogt.application.writes import WriteOutcome, audited_write, validate_reason
from vogt.core.clock import from_iso, to_iso
from vogt.core.entities import Actor
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import (
    CarryReport,
    CloneStamp,
    ProjectUpdate,
    WorkFilter,
    WriteTxn,
)

MANIFEST_NAME = "manifest.json"
MANIFEST_VERSION = 2

#: An export covers everything, finished work included: it is a record,
#: not a to-do list.
# Export carries retired (superseded) rows too: a portability dump that
# quietly dropped the rows anchoring comments and ledger history would not be
# a dump of the instance.
_ALL_WORK = WorkFilter(limit=10_000, exclude_terminal=False, include_superseded=True)

BACKUP_EVENT = "instance.backed_up"
RESTORE_EVENT = "instance.restored"
CLONE_EVENT = "instance.cloned"
CLONE_OPERATION = "clone"

#: The two store files, in the order they are swapped into place.
_STORE_FILES = ("declared.sqlite3", "observed.sqlite3")

#: What `clone --include-engine-state` copies from the backup's engine state:
#: session history only. Everything else in that directory is left behind —
#: `push.json` above all (the source's phones would receive this instance's
#: notifications), and `agent-tasks.json` / `agent-task-prompts/` (the
#: source's scheduled agent work would start running here too). An allowlist
#: rather than a denylist, so a file a later engine adds is left behind until
#: somebody decides it belongs to a copy.
_CLONED_ENGINE_STATE = ("history.db", "assistant-log.db", "session-logs")


@dataclass(frozen=True)
class Manifest:
    """What a backup says about itself."""

    manifest_version: int
    instance_id: str
    vogt_version: str
    declared_schema_version: int
    observed_schema_version: int
    taken_at: datetime
    #: What this backup covers besides the two stores, and what it does not
    #: A backup that silently covers less than the product is the
    #: failure the requirement names: the work items come back and the
    #: terminals' history, push subscriptions and agent tasks do not.
    engine_state: str = "not configured"
    #: Where imported projects lived when this was taken. A restore that
    #: re-establishes the stores without the tree leaves every project
    #: pointing at a path that no longer exists, and the
    #: restore says so rather than letting it be discovered by a session
    #: that will not open.
    import_root: str | None = None

    def to_json(self) -> dict[str, object]:
        return {
            "manifest_version": self.manifest_version,
            "instance_id": self.instance_id,
            "vogt_version": self.vogt_version,
            "declared_schema_version": self.declared_schema_version,
            "observed_schema_version": self.observed_schema_version,
            "taken_at": to_iso(self.taken_at),
            "engine_state": self.engine_state,
            "import_root": self.import_root,
        }

    @classmethod
    def from_json(cls, raw: dict[str, object]) -> Manifest:
        return cls(
            manifest_version=int(str(raw.get("manifest_version", 0))),
            instance_id=str(raw.get("instance_id", "")),
            vogt_version=str(raw.get("vogt_version", "")),
            declared_schema_version=int(str(raw.get("declared_schema_version", 0))),
            observed_schema_version=int(str(raw.get("observed_schema_version", 0))),
            taken_at=from_iso(str(raw.get("taken_at"))),
            # Absent in a version-1 manifest, which covered the two stores and
            # said nothing about the rest. Read as "unknown" rather than as
            # "nothing", because an old backup did not fail to copy the engine
            # state — it was taken before there was any.
            engine_state=str(raw.get("engine_state", "unknown (manifest v1)")),
            import_root=(
                None if raw.get("import_root") is None else str(raw["import_root"])
            ),
        )


#: Where the engine's state lands inside a backup directory.
ENGINE_STATE_DIR = "engine-state"


def _copy_engine_state(source: Path | None, target: Path) -> str:
    """Copy the engine's state directory, and say what happened either way.

    Returns the sentence the manifest carries. Every branch returns one,
    including the failures: a backup covers the whole product in one act, and
    a backup that quietly covered two thirds of it would be indistinguishable
    from one that covered all three until somebody restored it.

    Not fatal. A backup of the stores is worth having even when the engine's
    directory is unreadable — refusing to take one would trade a partial
    backup for none, which is the wrong way round.
    """
    if source is None:
        return "not configured"
    resolved = Path(source).expanduser()
    if not resolved.is_dir():
        return f"configured at {resolved}, which does not exist"
    try:
        shutil.copytree(resolved, target, symlinks=True, dirs_exist_ok=True)
    except OSError as exc:
        return f"copy of {resolved} failed: {exc}"
    return f"copied from {resolved}"


def _restore_engine_state(source: Path, target: Path | None) -> str:
    """Put the engine's state back, and say what happened either way.

    Never fatal, and never silent. The stores are already in place by the
    time this runs — refusing here would leave a half-restored instance,
    which is the state `restore` verifies its manifest up-front to avoid.
    """
    if not source.is_dir():
        return "not in this backup"
    if target is None:
        return (
            f"in the backup at {source}, but no engine_state_dir is "
            "configured here, so it was not restored"
        )
    resolved = Path(target).expanduser()
    try:
        resolved.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(source, resolved, symlinks=True, dirs_exist_ok=True)
    except OSError as exc:
        return f"restoring into {resolved} failed: {exc}"
    return f"restored into {resolved}"


def _snapshot(source: Path, target: Path) -> None:
    """Copy one SQLite database consistently, using its own backup API."""
    target.parent.mkdir(parents=True, exist_ok=True)
    origin = sqlite3.connect(source)
    try:
        destination = sqlite3.connect(target)
        try:
            origin.backup(destination)
        finally:
            destination.close()
    finally:
        origin.close()


def backup(ctx: AppContext, params: BackupParams) -> BackupResult:
    """Snapshot both stores plus a manifest."""
    from vogt import __version__

    with ctx.declared.read() as view:
        instance_id = view.instance_id()

    taken_at = ctx.clock()
    label = params.label or taken_at.strftime("%Y%m%dT%H%M%SZ")
    destination = (
        Path(params.destination).expanduser()
        if params.destination
        else ctx.config.backups_dir / label
    )
    if destination.exists() and any(destination.iterdir()):
        msg = f"{destination} already exists and is not empty"
        raise Conflict(msg)
    destination.mkdir(parents=True, exist_ok=True)

    _snapshot(ctx.config.declared_db_path, destination / "declared.sqlite3")
    _snapshot(ctx.config.observed_db_path, destination / "observed.sqlite3")
    engine_state = _copy_engine_state(
        ctx.config.engine_state_dir, destination / ENGINE_STATE_DIR
    )

    manifest = Manifest(
        manifest_version=MANIFEST_VERSION,
        instance_id=instance_id,
        vogt_version=__version__,
        declared_schema_version=ctx.declared.schema_version(),
        observed_schema_version=ctx.observed.schema_version(),
        taken_at=taken_at,
        engine_state=engine_state,
        import_root=str(ctx.config.resolved_import_root),
    )
    (destination / MANIFEST_NAME).write_text(
        json.dumps(manifest.to_json(), indent=2) + "\n", encoding="utf-8"
    )

    ctx.declared.publish_event(
        kind=BACKUP_EVENT,
        entity_kind="instance",
        entity_id=instance_id,
        summary={"path": str(destination), "reason": params.reason},
        at=ctx.clock(),
    )
    return BackupResult(
        path=str(destination),
        instance_id=instance_id,
        engine_state=manifest.engine_state,
        import_root=manifest.import_root,
        declared_schema_version=manifest.declared_schema_version,
        observed_schema_version=manifest.observed_schema_version,
        taken_at=taken_at,
    )


def _verified_manifest(ctx: AppContext, source: Path) -> Manifest:
    """Read a backup's manifest and refuse anything this build cannot take.

    Shared by `restore` and `clone`, which have to refuse the same things for
    the same reasons: an unreadable or future manifest, a missing store, a
    schema ahead of this build.
    """
    manifest_path = source / MANIFEST_NAME
    if not manifest_path.is_file():
        msg = f"{source} has no {MANIFEST_NAME}; it is not a Vogt backup"
        raise NotFound(msg)
    manifest = Manifest.from_json(json.loads(manifest_path.read_text("utf-8")))

    if manifest.manifest_version > MANIFEST_VERSION:
        msg = (
            f"backup manifest version {manifest.manifest_version} is newer "
            f"than {MANIFEST_VERSION}; run a newer build rather than guessing "
            "what it contains"
        )
        raise InvalidRequest(msg)
    if manifest.manifest_version < 1:
        msg = f"backup manifest version {manifest.manifest_version} is not readable"
        raise InvalidRequest(msg)
    for name in ("declared.sqlite3", "observed.sqlite3"):
        if not (source / name).is_file():
            msg = f"{source} is missing {name}"
            raise NotFound(msg)

    current_declared = ctx.declared.schema_version()
    if manifest.declared_schema_version > current_declared:
        msg = (
            f"the backup was taken at declared schema "
            f"{manifest.declared_schema_version} and this build is at "
            f"{current_declared}. Migrations are forward-only: run a newer "
            "build rather than restoring backwards."
        )
        raise InvalidRequest(msg)
    return manifest


def restore(ctx: AppContext, params: RestoreParams) -> RestoreResult:
    """Verify the manifest, then replace the data directory's stores.

    Verification happens **before anything is touched**. A restore that
    discovers a problem half way through has already destroyed the thing you
    would have wanted to keep.
    """
    source = Path(params.source).expanduser()
    manifest = _verified_manifest(ctx, source)

    if not params.confirm:
        msg = (
            f"this replaces the stores in {ctx.config.resolved_data_dir} with "
            f"the backup taken at {to_iso(manifest.taken_at)}. Pass --confirm."
        )
        raise InvalidRequest(msg)

    data_dir = ctx.config.resolved_data_dir
    data_dir.mkdir(parents=True, exist_ok=True)
    for name in ("declared.sqlite3", "observed.sqlite3"):
        shutil.copy2(source / name, data_dir / name)
        # WAL and shm files belong to the replaced database, not the new one.
        for suffix in ("-wal", "-shm"):
            stale = data_dir / f"{name}{suffix}"
            if stale.exists():
                stale.unlink()

    engine_state = _restore_engine_state(
        source / ENGINE_STATE_DIR, ctx.config.engine_state_dir
    )

    migrated = ctx.declared.migrate()
    ctx.observed.migrate()
    return RestoreResult(
        source=str(source),
        instance_id=manifest.instance_id,
        restored_from=manifest.taken_at,
        migrations_applied=list(migrated.applied),
        declared_schema_version=ctx.declared.schema_version(),
        engine_state=engine_state,
        import_root_then=manifest.import_root,
        import_root_now=str(ctx.config.resolved_import_root),
    )


def _clone_engine_state(source: Path, target: Path | None, *, include: bool) -> str:
    """Copy the allowlisted session history, or say why nothing was copied.

    Never fatal, like `_restore_engine_state`: by the time this runs the
    stores are already in place.
    """
    left_behind = (
        "push subscriptions and agent tasks are never copied by a clone; "
        "this engine keeps its own"
    )
    if not include:
        return f"not copied (pass include_engine_state to copy history); {left_behind}"
    if not source.is_dir():
        return f"not in this backup; {left_behind}"
    if target is None:
        return (
            f"in the backup at {source}, but no engine_state_dir is configured "
            f"here, so it was not copied; {left_behind}"
        )
    resolved = Path(target).expanduser()
    copied: list[str] = []
    try:
        resolved.mkdir(parents=True, exist_ok=True)
        for entry in sorted(source.iterdir()):
            # `history.db-wal` travels with `history.db`: the backup copied the
            # pair as they stood, and one without the other is a torn database.
            if not any(
                entry.name == name or entry.name.startswith(f"{name}-")
                for name in _CLONED_ENGINE_STATE
            ):
                continue
            destination = resolved / entry.name
            if entry.is_dir():
                shutil.copytree(entry, destination, symlinks=True, dirs_exist_ok=True)
            else:
                shutil.copy2(entry, destination)
            copied.append(entry.name)
    except OSError as exc:
        return f"copying session history into {resolved} failed: {exc}"
    if not copied:
        return f"no session history in the backup; {left_behind}"
    return f"copied {', '.join(copied)} into {resolved}; {left_behind}"


@dataclass(frozen=True)
class _Sanitised:
    carried: CarryReport
    write_back_reset: list[str]
    sessions_closed: int


def clone(ctx: AppContext, params: CloneParams) -> CloneResult:
    """Restore another instance's backup here as a copy, not as that instance.

    A plain `restore` of another instance's backup makes this instance *be*
    that one: its tokens work here, this instance's own tokens are gone, its
    forge write-back sends this copy's edits upstream as if from the source,
    and the two carry one instance id. A clone, in order:

    1. verifies the backup exactly as `restore` does, before touching
       anything, and refuses a backup of this same instance (that is a
       `restore`);
    2. reads this instance's own credentials — every token, password login
       and linked forge account, with the actors they belong to;
    3. copies the backup's stores into a staging directory beside the live
       ones and migrates them forward there;
    4. sanitises the staged copy in one audited write: the source's live
       tokens are revoked, its password logins and forge accounts dropped,
       this instance's credentials carried in (actors matched by
       identity_ref); every project's write-back set to `none`; sessions
       the source recorded as running marked stopped (they are processes on
       the source's engine); the instance id set back to this instance's and
       the clone stamp recorded; the audit row and `instance.cloned` event
       land in the same transaction;
    5. only then swaps the staged stores over the live ones.

    A failure before step 5 leaves the live stores untouched. The source
    engine's push subscriptions and agent tasks are never copied; its session
    history is copied only when asked for (`include_engine_state`).
    """
    source = Path(params.source).expanduser()
    manifest = _verified_manifest(ctx, source)
    current_observed = ctx.observed.schema_version()
    if manifest.observed_schema_version > current_observed:
        msg = (
            f"the backup was taken at observed schema "
            f"{manifest.observed_schema_version} and this build is at "
            f"{current_observed}. Run a newer build rather than cloning backwards."
        )
        raise InvalidRequest(msg)
    reason = validate_reason(params.reason)

    with ctx.declared.read() as view:
        instance_id = view.instance_id()
    if manifest.instance_id == instance_id:
        msg = (
            f"{source} is a backup of this instance ({instance_id}); a clone "
            "copies another instance. Use `restore` to put this one back."
        )
        raise InvalidRequest(msg)

    if not params.confirm:
        msg = (
            f"this replaces the stores in {ctx.config.resolved_data_dir} with a "
            f"copy of instance {manifest.instance_id} as it was at "
            f"{to_iso(manifest.taken_at)}, keeping this instance's id and "
            "credentials. Pass --confirm."
        )
        raise InvalidRequest(msg)

    carried = ctx.declared.credentials()
    cloned_at = ctx.clock()
    stamp = CloneStamp(
        source_instance_id=manifest.instance_id,
        cloned_at=cloned_at,
        backup_taken_at=manifest.taken_at,
    )
    revoke_reason = (
        f"cloned from instance {manifest.instance_id}: the source's credentials "
        "are not valid on a copy"
    )

    data_dir = ctx.config.resolved_data_dir
    data_dir.mkdir(parents=True, exist_ok=True)
    # Beside the live stores, so the final swap is a same-filesystem rename.
    staging = data_dir / f".clone-staging-{cloned_at.strftime('%Y%m%dT%H%M%S%f')}"
    staging.mkdir()
    try:
        for name in _STORE_FILES:
            shutil.copy2(source / name, staging / name)
        staged = with_stores_at(ctx, staging)
        migrated = staged.declared.migrate()
        staged.observed.migrate()

        def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[_Sanitised]:
            del actor
            carry = txn.carry_credentials(carried, reason=revoke_reason, at=cloned_at)
            reset: list[str] = []
            for project in txn.list_projects(limit=10_000, offset=0):
                if project.write_back != "none":
                    txn.update_project(
                        project.id, ProjectUpdate(write_back="none"), at=cloned_at
                    )
                    reset.append(project.slug)
            running = txn.list_sessions(include_stopped=False, limit=10_000, offset=0)
            for session in running:
                txn.mark_session_stopped(session.id, at=cloned_at)
            txn.set_instance_identity(instance_id, stamp)
            summary: dict[str, object] = {
                "source_instance_id": manifest.instance_id,
                "backup_taken_at": to_iso(manifest.taken_at),
                "source": str(source),
                "tokens_kept": carry.tokens_kept,
                "source_tokens_revoked": carry.source_tokens_revoked,
                "write_back_reset": reset,
                "sessions_closed": len(running),
            }
            return WriteOutcome(
                result=_Sanitised(
                    carried=carry, write_back_reset=reset, sessions_closed=len(running)
                ),
                entity_kind="instance",
                entity_id=instance_id,
                payload=summary,
                event_kind=CLONE_EVENT,
                summary=summary,
            )

        sanitised = audited_write(
            staged, operation=CLONE_OPERATION, reason=reason, body=body
        )
        staged.observed.rebind_instance(instance_id)

        for name in _STORE_FILES:
            # WAL and shm files belong to the replaced database, not the new one.
            for suffix in ("-wal", "-shm"):
                stale = data_dir / f"{name}{suffix}"
                if stale.exists():
                    stale.unlink()
            (staging / name).replace(data_dir / name)
            # Every staged connection is closed, so SQLite has normally folded
            # its WAL back already; if one is left, it travels with its file.
            leftover = staging / f"{name}-wal"
            if leftover.exists():
                leftover.replace(data_dir / f"{name}-wal")
    finally:
        shutil.rmtree(staging, ignore_errors=True)

    engine_state = _clone_engine_state(
        source / ENGINE_STATE_DIR,
        ctx.config.engine_state_dir,
        include=params.include_engine_state,
    )
    carry = sanitised.carried
    return CloneResult(
        source=str(source),
        instance_id=instance_id,
        source_instance_id=manifest.instance_id,
        restored_from=manifest.taken_at,
        cloned_at=cloned_at,
        migrations_applied=list(migrated.applied),
        declared_schema_version=ctx.declared.schema_version(),
        tokens_kept=carry.tokens_kept,
        source_tokens_revoked=carry.source_tokens_revoked,
        password_logins_kept=carry.password_logins_kept,
        source_password_logins_dropped=carry.source_password_logins_dropped,
        forge_accounts_kept=carry.forge_accounts_kept,
        source_forge_accounts_dropped=carry.source_forge_accounts_dropped,
        write_back_reset=sanitised.write_back_reset,
        sessions_closed=sanitised.sessions_closed,
        engine_state=engine_state,
        import_root_then=manifest.import_root,
        import_root_now=str(ctx.config.resolved_import_root),
    )


#: The export file format. 1 (no version key) carried projects, work items,
#: initiatives, labels and actors; 2 adds the format key, the scope, the
#: source's clone stamp and every comment — what an applying `import` needs.
#: Relations, labels and the initiative link travel inside each work item.
EXPORT_FORMAT_VERSION = 2


def export_instance(ctx: AppContext, params: ExportParams) -> ExportResult:
    """Write the declared entities as JSON, for reading and for moving.

    Deliberately declared-only: observations are evidence a collector can
    reproduce, and shipping a million of them around is not what anybody
    means by "export my backlog". Never exported: tokens, password logins,
    forge accounts, auth decisions, sessions — nothing that would let the
    file act as the instance it came from.

    With `project`, the export is one project: its work items (comments and
    relations with them) and only the initiatives, labels and actors those
    reference. A relation to an item in another project still names it by
    id; an import links it if the target already holds that item.
    """
    with ctx.declared.read() as view:
        stamp = view.clone_stamp()
        projects = view.list_projects(limit=10_000, offset=0)
        items = view.list_work_items(_ALL_WORK)
        initiatives = view.list_initiatives(limit=10_000, offset=0)
        labels = view.list_labels(limit=10_000, offset=0)
        actors = view.list_actors(limit=10_000, offset=0)
        if params.project is not None:
            scoped = view.project_by_slug(params.project)
            if scoped is None:
                msg = f"no project with slug {params.project!r}"
                raise NotFound(msg)
            projects = [scoped]
            items = [item for item in items if item.project_id == scoped.id]
        comments = [
            comment
            for item in items
            for comment in view.comments_for(item.id, limit=100_000)
        ]
        if params.project is not None:
            initiative_ids = {i.initiative_id for i in items if i.initiative_id}
            initiatives = [i for i in initiatives if i.id in initiative_ids]
            label_names = {name for item in items for name in item.labels}
            labels = [label for label in labels if label.name in label_names]
            actor_ids = {i.assignee_actor_id for i in items if i.assignee_actor_id}
            actor_ids |= {comment.actor_id for comment in comments}
            actors = [actor for actor in actors if actor.id in actor_ids]
        payload: dict[str, object] = {
            "export_format_version": EXPORT_FORMAT_VERSION,
            "instance_id": view.instance_id(),
            "exported_at": to_iso(ctx.clock()),
            "revision": view.current_revision(),
            "scope": {"project": params.project},
            "clone_stamp": None
            if stamp is None
            else {
                "source_instance_id": stamp.source_instance_id,
                "cloned_at": to_iso(stamp.cloned_at),
                "backup_taken_at": to_iso(stamp.backup_taken_at),
            },
            "projects": [p.model_dump(mode="json") for p in projects],
            "work_items": [item.model_dump(mode="json") for item in items],
            "initiatives": [i.model_dump(mode="json") for i in initiatives],
            "labels": [label.model_dump(mode="json") for label in labels],
            "actors": [actor.model_dump(mode="json") for actor in actors],
            "comments": [comment.model_dump(mode="json") for comment in comments],
        }

    destination = Path(params.destination).expanduser()
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return ExportResult(
        path=str(destination),
        export_format_version=EXPORT_FORMAT_VERSION,
        project=params.project,
        projects=len(projects),
        work_items=len(items),
        comments=len(comments),
    )


def import_instance(ctx: AppContext, params: ImportParams) -> ImportResult:
    """Merge an export into this instance under the documented policy.

    A dry run unless `apply` (with `confirm`): the same report either way, so
    what an apply will do is always readable first. The policy, the identity
    matching and what is never imported are in `instance_merge`.
    """
    return merge_export(ctx, params)
