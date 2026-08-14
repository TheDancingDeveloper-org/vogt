"""Backup, restore, export, import (FR-L1, FR-L2).

A backup snapshots **both** stores consistently and writes a manifest naming
the schema version of each. Restore verifies that manifest *before touching
anything* — because the failure this prevents is restoring a backup taken
under an older schema onto a newer binary and discovering it half way
through, with the live data already gone.

Backups use SQLite's own backup API rather than copying files: a copy taken
while a write is in flight is a torn database, and WAL mode makes that more
likely rather than less.
"""

from __future__ import annotations

import json
import shutil
import sqlite3
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    BackupParams,
    BackupResult,
    ExportParams,
    ExportResult,
    ImportParams,
    ImportResult,
    RestoreParams,
    RestoreResult,
)
from vogt.core.clock import from_iso, to_iso
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import WorkFilter

MANIFEST_NAME = "manifest.json"
MANIFEST_VERSION = 2

#: An export covers everything, finished work included: it is a record,
#: not a to-do list.
_ALL_WORK = WorkFilter(limit=10_000, exclude_terminal=False)

BACKUP_EVENT = "instance.backed_up"
RESTORE_EVENT = "instance.restored"


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
    #: (NFR-I6). A backup that silently covers less than the product is the
    #: failure the requirement names: the work items come back and the
    #: terminals' history, push subscriptions and agent tasks do not.
    engine_state: str = "not configured"
    #: Where imported projects lived when this was taken. A restore that
    #: re-establishes the stores without the tree leaves every project
    #: pointing at a path that no longer exists (FR-E3, §6.3), and the
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
    including the failures: NFR-I6 asks for the whole product in one act, and
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
    """Snapshot both stores plus a manifest (FR-L2)."""
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


def restore(ctx: AppContext, params: RestoreParams) -> RestoreResult:
    """Verify the manifest, then replace the data directory's stores.

    Verification happens **before anything is touched**. A restore that
    discovers a problem half way through has already destroyed the thing you
    would have wanted to keep.
    """
    source = Path(params.source).expanduser()
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
            "build rather than restoring backwards (NFR-I3)."
        )
        raise InvalidRequest(msg)

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


def export_instance(ctx: AppContext, params: ExportParams) -> ExportResult:
    """Write the declared entities as JSON, for reading and for moving.

    Deliberately declared-only: observations are evidence a collector can
    reproduce, and shipping a million of them around is not what anybody
    means by "export my backlog".
    """
    with ctx.declared.read() as view:
        payload = {
            "instance_id": view.instance_id(),
            "exported_at": to_iso(ctx.clock()),
            "revision": view.current_revision(),
            "projects": [
                p.model_dump(mode="json")
                for p in view.list_projects(limit=10_000, offset=0)
            ],
            "work_items": [
                item.model_dump(mode="json") for item in view.list_work_items(_ALL_WORK)
            ],
            "initiatives": [
                initiative.model_dump(mode="json")
                for initiative in view.list_initiatives(limit=10_000, offset=0)
            ],
            "labels": [
                label.model_dump(mode="json")
                for label in view.list_labels(limit=10_000, offset=0)
            ],
            "actors": [
                actor.model_dump(mode="json")
                for actor in view.list_actors(limit=10_000, offset=0)
            ],
        }

    destination = Path(params.destination).expanduser()
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
    return ExportResult(
        path=str(destination),
        projects=len(payload["projects"]),  # type: ignore[arg-type]
        work_items=len(payload["work_items"]),  # type: ignore[arg-type]
    )


def import_instance(ctx: AppContext, params: ImportParams) -> ImportResult:
    """Report what an export contains, without writing anything.

    A real merge needs an identity-conflict policy — same slug, different
    project; same ref, different item — and inventing one silently is how
    an import destroys the thing it was meant to restore. Until that policy
    is designed, this reads the file and tells you what is in it, and
    `restore` remains the supported way to move an instance.
    """
    source = Path(params.source).expanduser()
    if not source.is_file():
        msg = f"no such export: {source}"
        raise NotFound(msg)
    payload = json.loads(source.read_text(encoding="utf-8"))
    return ImportResult(
        source=str(source),
        instance_id=str(payload.get("instance_id", "")),
        projects=len(payload.get("projects", [])),
        work_items=len(payload.get("work_items", [])),
        applied=False,
        detail=(
            "Import is read-only in v1: merging two instances needs a "
            "documented conflict policy, and guessing one silently is how an "
            "import destroys what it was meant to restore. Use `restore` to "
            "move an instance."
        ),
    )
