"""Backup, restore, export, import (FR-L1, FR-L2)."""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BackupParams,
    CreateWorkParams,
    ExportParams,
    ImportParams,
    InitParams,
    ListWorkParams,
    RegisterProjectParams,
    RestoreParams,
)
from vogt.application.services import (
    backup,
    create_work,
    export_instance,
    import_instance,
    init_instance,
    list_work,
    register_project,
    restore,
)
from vogt.application.services.lifecycle import MANIFEST_NAME
from vogt.config import VogtConfig
from vogt.errors import Conflict, InvalidRequest, NotFound

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "lifecycle test"


@pytest.fixture
def populated(instance: AppContext, tmp_path: Path) -> AppContext:
    register_project(
        instance,
        RegisterProjectParams(
            name="Kept", root_path=str(tmp_path / "kept"), reason=WHY
        ),
    )
    create_work(
        instance,
        CreateWorkParams(kind="bug", title="Worth keeping", reason=WHY),
    )
    return instance


# -- backup ----------------------------------------------------------------


def test_a_backup_carries_a_manifest(populated: AppContext, tmp_path: Path) -> None:
    result = backup(
        populated,
        BackupParams(destination=str(tmp_path / "snap"), reason="before an upgrade"),
    )
    snapshot = Path(result.path)
    assert (snapshot / "declared.sqlite3").is_file()
    assert (snapshot / "observed.sqlite3").is_file()

    manifest = json.loads((snapshot / MANIFEST_NAME).read_text(encoding="utf-8"))
    assert manifest["instance_id"] == result.instance_id
    assert manifest["declared_schema_version"] > 0
    assert manifest["vogt_version"]


def test_a_backup_publishes_an_event(populated: AppContext, tmp_path: Path) -> None:
    backup(populated, BackupParams(destination=str(tmp_path / "s"), reason=WHY))
    with populated.declared.read() as view:
        kinds = [event.kind for event in view.list_events(after=0, limit=50)]
    assert "instance.backed_up" in kinds


def test_backing_up_over_something_is_refused(
    populated: AppContext, tmp_path: Path
) -> None:
    destination = tmp_path / "occupied"
    destination.mkdir()
    (destination / "something.txt").write_text("mine", encoding="utf-8")
    with pytest.raises(Conflict, match="not empty"):
        backup(populated, BackupParams(destination=str(destination), reason=WHY))


def test_the_default_destination_is_inside_the_data_dir(
    populated: AppContext,
) -> None:
    result = backup(populated, BackupParams(reason=WHY))
    assert Path(result.path).parent == populated.config.backups_dir


# -- restore ---------------------------------------------------------------


def _fresh(tmp_path: Path, name: str) -> AppContext:
    context = build_context(
        config=VogtConfig(data_dir=tmp_path / name),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )
    init_instance(context, InitParams())
    return context


def test_a_backup_restores_into_a_different_instance(
    populated: AppContext, tmp_path: Path
) -> None:
    taken = backup(
        populated, BackupParams(destination=str(tmp_path / "snap"), reason=WHY)
    )
    target = _fresh(tmp_path, "target")
    assert list_work(target, ListWorkParams()).items == []

    result = restore(
        target, RestoreParams(source=taken.path, confirm=True, reason="disaster")
    )
    assert result.instance_id == taken.instance_id
    restored = list_work(target, ListWorkParams()).items
    assert [item.title for item in restored] == ["Worth keeping"]


def test_restore_refuses_without_confirmation(
    populated: AppContext, tmp_path: Path
) -> None:
    """It replaces live data; a typo should not be enough."""
    taken = backup(
        populated, BackupParams(destination=str(tmp_path / "snap"), reason=WHY)
    )
    target = _fresh(tmp_path, "target")
    with pytest.raises(InvalidRequest, match="--confirm"):
        restore(target, RestoreParams(source=taken.path, reason=WHY))
    assert list_work(target, ListWorkParams()).items == [], "nothing was touched"


def test_restoring_a_directory_that_is_not_a_backup_says_so(
    instance: AppContext, tmp_path: Path
) -> None:
    with pytest.raises(NotFound, match="not a Vogt backup"):
        restore(
            instance,
            RestoreParams(source=str(tmp_path), confirm=True, reason=WHY),
        )


def test_a_manifest_from_the_future_is_refused_before_anything_is_touched(
    populated: AppContext, tmp_path: Path
) -> None:
    """FR-L2: verification happens before the live data is replaced.

    Migrations are forward-only, so a backup taken under a newer schema
    cannot be restored onto an older build — and discovering that half way
    through would already have destroyed what you wanted to keep.
    """
    taken = backup(
        populated, BackupParams(destination=str(tmp_path / "snap"), reason=WHY)
    )
    manifest_path = Path(taken.path) / MANIFEST_NAME
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["declared_schema_version"] = 999
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")

    target = _fresh(tmp_path, "target")
    create_work(target, CreateWorkParams(kind="chore", title="Precious", reason=WHY))

    with pytest.raises(InvalidRequest, match="forward-only"):
        restore(target, RestoreParams(source=taken.path, confirm=True, reason=WHY))

    survivors = [item.title for item in list_work(target, ListWorkParams()).items]
    assert survivors == ["Precious"], "the live data is untouched"


def test_an_incomplete_backup_is_refused(populated: AppContext, tmp_path: Path) -> None:
    taken = backup(
        populated, BackupParams(destination=str(tmp_path / "snap"), reason=WHY)
    )
    (Path(taken.path) / "observed.sqlite3").unlink()
    with pytest.raises(NotFound, match=r"missing observed\.sqlite3"):
        restore(
            populated,
            RestoreParams(source=taken.path, confirm=True, reason=WHY),
        )


def test_restore_leaves_no_stale_wal_behind(
    populated: AppContext, tmp_path: Path
) -> None:
    """A WAL from the replaced database would corrupt the restored one."""
    taken = backup(
        populated, BackupParams(destination=str(tmp_path / "snap"), reason=WHY)
    )
    target = _fresh(tmp_path, "target")
    create_work(target, CreateWorkParams(kind="chore", title="Doomed", reason=WHY))

    restore(target, RestoreParams(source=taken.path, confirm=True, reason=WHY))
    titles = [item.title for item in list_work(target, ListWorkParams()).items]
    assert titles == ["Worth keeping"]


# -- export and import -----------------------------------------------------


def test_export_writes_the_declared_entities(
    populated: AppContext, tmp_path: Path
) -> None:
    destination = tmp_path / "export.json"
    result = export_instance(
        populated, ExportParams(destination=str(destination), reason=WHY)
    )
    payload = json.loads(destination.read_text(encoding="utf-8"))
    assert result.projects == 1
    assert result.work_items == 1
    assert payload["work_items"][0]["title"] == "Worth keeping"
    assert "observations" not in payload, "evidence is reproducible; it is not export"


def test_import_reports_without_applying(populated: AppContext, tmp_path: Path) -> None:
    """Read-only in v1, and it says so rather than half-merging."""
    destination = tmp_path / "export.json"
    export_instance(populated, ExportParams(destination=str(destination), reason=WHY))

    target = _fresh(tmp_path, "target")
    result = import_instance(target, ImportParams(source=str(destination), reason=WHY))
    assert result.applied is False
    assert result.work_items == 1
    assert "conflict policy" in result.detail
    assert list_work(target, ListWorkParams()).items == []


def test_importing_something_absent_says_so(instance: AppContext) -> None:
    with pytest.raises(NotFound, match="no such export"):
        import_instance(instance, ImportParams(source="/nope.json", reason=WHY))


# -- NFR-I6: one act covers the whole product ------------------------------


def test_a_backup_carries_the_engines_state_and_says_so(
    instance: AppContext, tmp_path: Path
) -> None:
    """The requirement is "as one act", and the manifest is where it is checked.

    Half a restore is the failure: the work items come back and the
    terminals' history, push subscriptions and agent tasks do not.
    """
    engine_state = tmp_path / "engine-state"
    (engine_state / "agent-task-prompts").mkdir(parents=True)
    (engine_state / "agent-task-prompts" / "one.md").write_text("a brief", "utf-8")
    (engine_state / "push.json").write_text('{"subscriptions": []}', "utf-8")

    ctx = dataclasses.replace(
        instance,
        config=instance.config.model_copy(update={"engine_state_dir": engine_state}),
    )
    result = backup(ctx, BackupParams(reason="nfr-i6"))

    copied = Path(result.path) / "engine-state"
    assert (copied / "push.json").read_text("utf-8") == '{"subscriptions": []}'
    assert (copied / "agent-task-prompts" / "one.md").read_text("utf-8") == "a brief"
    assert "copied from" in result.engine_state
    assert result.import_root, "a restore elsewhere needs to know where projects were"


def test_a_backup_without_the_engine_says_which(instance: AppContext) -> None:
    """Not an error, and not silence.

    A core-only deployment has no engine state, and a backup of one is
    complete. What it must not do is look identical to a merged backup that
    missed half the product.
    """
    result = backup(instance, BackupParams(reason="core only"))
    assert result.engine_state == "not configured"
    assert not (Path(result.path) / "engine-state").exists()


def test_a_backup_survives_an_unreadable_engine_directory(
    instance: AppContext, tmp_path: Path
) -> None:
    """A partial backup beats no backup, and says which part it is."""
    ctx = dataclasses.replace(
        instance,
        config=instance.config.model_copy(
            update={"engine_state_dir": tmp_path / "not-there"}
        ),
    )
    result = backup(ctx, BackupParams(reason="engine gone"))
    assert "does not exist" in result.engine_state
    assert (Path(result.path) / "declared.sqlite3").is_file()


def test_a_restore_puts_the_engine_state_back(
    instance: AppContext, tmp_path: Path
) -> None:
    engine_state = tmp_path / "engine-state"
    engine_state.mkdir()
    (engine_state / "push.json").write_text("original", "utf-8")

    ctx = dataclasses.replace(
        instance,
        config=instance.config.model_copy(update={"engine_state_dir": engine_state}),
    )
    taken = backup(ctx, BackupParams(reason="before"))
    (engine_state / "push.json").write_text("clobbered", "utf-8")

    restored = restore(
        ctx, RestoreParams(source=taken.path, confirm=True, reason="nfr-i6 test")
    )

    assert (engine_state / "push.json").read_text("utf-8") == "original"
    assert "restored into" in restored.engine_state


def test_a_restore_reports_an_estate_that_moved(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-E3's path agreement, at the moment it is most likely to break.

    A restore does not rewrite the paths in the store — they are declared
    state, and rewriting them would be inventing a decision. So it says
    where projects were and where they will be looked for, and lets somebody
    who moved the estate see it here rather than in a session that will not
    open.
    """
    taken = backup(instance, BackupParams(reason="before the move"))
    moved = dataclasses.replace(
        instance,
        config=instance.config.model_copy(update={"import_root": tmp_path / "moved"}),
    )
    restored = restore(
        moved, RestoreParams(source=taken.path, confirm=True, reason="nfr-i6 test")
    )

    assert restored.import_root_then != restored.import_root_now
    assert restored.import_root_now is not None
    assert "moved" in restored.import_root_now


def test_an_older_manifest_still_restores(instance: AppContext, tmp_path: Path) -> None:
    """A version bump must not strand the backups taken before it.

    Manifest v1 covered the two stores and said nothing about the rest — not
    because it failed to copy the engine's state, but because there was none
    to copy. It reads as unknown, which is the honest word for it.
    """
    taken = backup(instance, BackupParams(reason="v1 era"))
    manifest_path = Path(taken.path) / "manifest.json"
    raw = json.loads(manifest_path.read_text("utf-8"))
    raw["manifest_version"] = 1
    del raw["engine_state"]
    del raw["import_root"]
    manifest_path.write_text(json.dumps(raw), "utf-8")

    restored = restore(
        instance,
        RestoreParams(source=taken.path, confirm=True, reason="nfr-i6 test"),
    )
    assert restored.instance_id == taken.instance_id
