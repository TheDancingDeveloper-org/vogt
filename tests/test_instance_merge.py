"""Applying import: an export merged into a live instance.

The scenario these tests stand in for is the one the operation exists for:
prod is cloned into dev, both move on, and dev's work is carried back (or
prod's carried forward) without losing either side's changes. So the
fixture builds two real instances in two data directories, clones one from
the other, and diverges them deliberately:

- an item changed only on prod, one changed only on dev, one changed on
  both, one changed on neither;
- a new item, comment and relation on each side.

Every test then imports one side's export into the other and checks what
the policy promises: creates, one-side updates taken, target-side changes
kept, both-sides changes held as conflicts, and a re-import that changes
nothing.
"""

from __future__ import annotations

import dataclasses
import json
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BackupParams,
    CloneParams,
    CommentParams,
    CreateInitiativeParams,
    CreateLabelParams,
    CreateWorkParams,
    ExportParams,
    GetWorkParams,
    ImportParams,
    ImportResult,
    InitParams,
    RegisterProjectParams,
    RelateWorkParams,
    UpdateWorkParams,
)
from vogt.application.services import (
    backup,
    clone,
    comment_work,
    create_initiative,
    create_label,
    create_work,
    export_instance,
    get_work,
    import_instance,
    init_instance,
    register_project,
    relate_work,
    update_work,
)
from vogt.config import VogtConfig
from vogt.core.merge import decide
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import ProjectUpdate

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "applying import test"
T0 = datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)


class PrefixedIds(SequentialIds):
    """Ids that cannot collide with another instance's, as ULIDs do not."""

    def __init__(self, tag: str) -> None:
        super().__init__()
        self._tag = tag

    def __call__(self, prefix: str) -> str:
        return f"{self._tag}{super().__call__(prefix)}"


def _instance(tmp_path: Path, name: str, *, start: datetime) -> AppContext:
    context = build_context(
        config=VogtConfig(data_dir=tmp_path / name, sqlite_synchronous="off"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(start),
        id_factory=PrefixedIds(name),
    )
    init_instance(context, InitParams())
    return context


def _new(ctx: AppContext, title: str, **extra: object) -> str:
    return create_work(
        ctx,
        CreateWorkParams(
            kind="feature",
            title=title,
            project="shipped",
            local_only=True,
            reason=WHY,
            **extra,  # type: ignore[arg-type]
        ),
    ).item.ref


def _retitle(ctx: AppContext, ref: str, title: str) -> None:
    update_work(ctx, UpdateWorkParams(ref=ref, title=title, reason=WHY))


def _get(ctx: AppContext, ref: str) -> tuple[str, str, list[str]]:
    got = get_work(ctx, GetWorkParams(ref=ref, comment_limit=500))
    return got.item.title, got.item.priority, [c.body for c in got.comments]


def _revision(ctx: AppContext) -> int:
    with ctx.declared.read() as view:
        return view.current_revision()


def _ref_by_id(ctx: AppContext, item_id: str) -> str | None:
    with ctx.declared.read() as view:
        item = view.work_item_by_id(item_id)
    return None if item is None else item.ref


@dataclasses.dataclass
class Estate:
    prod: AppContext
    dev: AppContext
    ids: dict[str, str]  # name -> work item id (shared by both instances)
    tmp: Path

    def export(self, ctx: AppContext, name: str, **extra: object) -> str:
        path = self.tmp / f"{name}.json"
        export_instance(
            ctx,
            ExportParams(destination=str(path), reason=WHY, **extra),  # type: ignore[arg-type]
        )
        return str(path)


@pytest.fixture
def estate(tmp_path: Path) -> Estate:
    """prod cloned into dev, then both diverged."""
    prod = _instance(tmp_path, "prod", start=T0)
    register_project(
        prod,
        RegisterProjectParams(
            name="Shipped", root_path=str(tmp_path / "shipped"), reason=WHY
        ),
    )
    create_label(prod, CreateLabelParams(name="ux", reason=WHY))
    create_initiative(prod, CreateInitiativeParams(title="Launch", reason=WHY))
    refs = {
        "prod_only": _new(prod, "Changed on prod", initiative="launch"),
        "dev_only": _new(prod, "Changed on dev", labels=["ux"]),
        "both": _new(prod, "Changed on both"),
        "neither": _new(prod, "Changed nowhere"),
    }
    comment_work(prod, CommentParams(ref=refs["neither"], body="before", reason=WHY))
    taken = backup(prod, BackupParams(destination=str(tmp_path / "bk"), reason=WHY))

    # Dev's clock runs later than prod's backup, as a real clone's would.
    dev = _instance(tmp_path, "dev", start=T0 + timedelta(days=1))
    clone(dev, CloneParams(source=taken.path, confirm=True, reason=WHY))

    with prod.declared.read() as view:
        ids = {
            name: item.id
            for name, ref in refs.items()
            if (item := view.work_item_by_ref(ref)) is not None
        }

    # prod moves on after its backup.
    _retitle(prod, refs["prod_only"], "Prod's new title")
    update_work(prod, UpdateWorkParams(ref=refs["both"], priority="p0", reason=WHY))
    ids["prod_new"] = _id_of(prod, _new(prod, "Born on prod"))
    relate_work(
        prod,
        RelateWorkParams(
            ref=_ref(prod, ids["prod_new"]),
            kind="depends_on",
            target=refs["neither"],
            reason=WHY,
        ),
    )
    comment_work(
        prod, CommentParams(ref=_ref(prod, ids["prod_new"]), body="hi", reason=WHY)
    )
    comment_work(prod, CommentParams(ref=refs["neither"], body="prod", reason=WHY))

    # dev moves on too.
    _retitle(dev, refs["dev_only"], "Dev's new title")
    _retitle(dev, refs["both"], "Dev retitled it")
    ids["dev_new"] = _id_of(dev, _new(dev, "Born on dev"))
    comment_work(dev, CommentParams(ref=refs["neither"], body="dev", reason=WHY))
    return Estate(prod=prod, dev=dev, ids=ids, tmp=tmp_path)


def _id_of(ctx: AppContext, ref: str) -> str:
    with ctx.declared.read() as view:
        item = view.work_item_by_ref(ref)
    assert item is not None
    return item.id


def _ref(ctx: AppContext, item_id: str) -> str:
    ref = _ref_by_id(ctx, item_id)
    assert ref is not None
    return ref


def _item_actions(result: ImportResult) -> dict[str, str]:
    return {c.key: c.action for c in result.changes if c.entity == "work_item"}


def _import(ctx: AppContext, source: str, **extra: object) -> ImportResult:
    return import_instance(
        ctx,
        ImportParams(source=source, reason=WHY, **extra),  # type: ignore[arg-type]
    )


def _apply(ctx: AppContext, source: str, **extra: object) -> ImportResult:
    return _import(ctx, source, apply=True, confirm=True, **extra)


# -- the policy ------------------------------------------------------------


def test_the_policy_table() -> None:
    base = T0
    later = T0 + timedelta(hours=1)
    earlier = T0 - timedelta(hours=1)

    def run(target: datetime, incoming: datetime, *, equal: bool = False) -> str:
        return decide(
            equal=equal,
            base=base,
            target_updated_at=target,
            incoming_updated_at=incoming,
        )

    assert run(later, later, equal=True) == "unchanged"
    assert run(earlier, later) == "take_incoming"
    assert run(later, earlier) == "keep_target"
    assert run(later, later) == "conflict"
    assert run(earlier, earlier) == "conflict", "a difference nobody explains"
    assert (
        decide(
            equal=False, base=None, target_updated_at=later, incoming_updated_at=later
        )
        == "conflict"
    )


# -- export format 2 -------------------------------------------------------


def test_export_carries_what_an_import_needs(estate: Estate) -> None:
    payload = json.loads(Path(estate.export(estate.dev, "dev")).read_text("utf-8"))
    assert payload["export_format_version"] == 2
    assert payload["clone_stamp"]["source_instance_id"] == _instance_id(estate.prod)
    assert {c["body"] for c in payload["comments"]} >= {"before", "dev"}
    item = next(w for w in payload["work_items"] if w["id"] == estate.ids["dev_only"])
    assert item["labels"] == ["ux"]
    assert "updated_at" in item
    for secret in ("tokens", "password_credentials", "forge_accounts", "sessions"):
        assert secret not in payload


def test_a_project_export_holds_only_that_project(estate: Estate) -> None:
    register_project(
        estate.prod,
        RegisterProjectParams(
            name="Other", root_path=str(estate.tmp / "other"), reason=WHY
        ),
    )
    create_work(
        estate.prod,
        CreateWorkParams(
            kind="bug", title="Elsewhere", project="other", local_only=True, reason=WHY
        ),
    )
    path = estate.export(estate.prod, "scoped", project="shipped")
    payload = json.loads(Path(path).read_text("utf-8"))
    assert payload["scope"] == {"project": "shipped"}
    assert [p["slug"] for p in payload["projects"]] == ["shipped"]
    assert all(w["project_slug"] == "shipped" for w in payload["work_items"])
    assert [i["slug"] for i in payload["initiatives"]] == ["launch"]

    with pytest.raises(NotFound):
        estate.export(estate.prod, "nope", project="absent")


def _instance_id(ctx: AppContext) -> str:
    with ctx.declared.read() as view:
        return view.instance_id()


# -- prod's export merged into dev (dev is the clone) ----------------------


def test_the_dry_run_reports_the_plan_and_writes_nothing(estate: Estate) -> None:
    source = estate.export(estate.prod, "prod")
    before = _revision(estate.dev)
    result = _import(estate.dev, source)

    assert result.applied is False
    assert result.base is not None
    assert "clone of" in result.base_source
    actions = _item_actions(result)
    assert actions[estate.ids["prod_only"]] == "updated"
    assert actions[estate.ids["dev_only"]] == "skipped"
    assert actions[estate.ids["both"]] == "conflict"
    assert actions[estate.ids["prod_new"]] == "created"
    assert estate.ids["neither"] not in actions, "unchanged items are only counted"
    assert _revision(estate.dev) == before
    assert _ref_by_id(estate.dev, estate.ids["prod_new"]) is None


def test_apply_needs_confirmation(estate: Estate) -> None:
    source = estate.export(estate.prod, "prod")
    with pytest.raises(InvalidRequest, match="--confirm"):
        _import(estate.dev, source, apply=True)


def test_strict_refuses_a_conflict_and_leaves_the_target_untouched(
    estate: Estate,
) -> None:
    source = estate.export(estate.prod, "prod")
    before = _revision(estate.dev)
    with pytest.raises(Conflict, match="--strict"):
        _apply(estate.dev, source, strict=True)
    assert _revision(estate.dev) == before
    assert _ref_by_id(estate.dev, estate.ids["prod_new"]) is None
    assert _get(estate.dev, _ref(estate.dev, estate.ids["prod_only"]))[0] == (
        "Changed on prod"
    )


def test_prod_merged_into_dev(estate: Estate) -> None:
    source = estate.export(estate.prod, "prod")
    dev = estate.dev
    result = _apply(dev, source)
    assert result.applied is True

    # Changed only on prod: taken.
    assert _get(dev, _ref(dev, estate.ids["prod_only"]))[0] == "Prod's new title"
    # Changed only on dev: kept.
    assert _get(dev, _ref(dev, estate.ids["dev_only"]))[0] == "Dev's new title"
    # Changed on both: dev's version kept, prod's attached as a comment.
    title, priority, comments = _get(dev, _ref(dev, estate.ids["both"]))
    assert (title, priority) == ("Dev retitled it", "p2")
    assert len(comments) == 1
    assert comments[0].startswith("Import conflict")
    assert '"p0"' in comments[0]
    # New on prod: created, with a fresh ref from dev's counter.
    created = next(c for c in result.changes if c.key == estate.ids["prod_new"])
    new_ref = _ref(dev, estate.ids["prod_new"])
    assert created.ref == new_ref
    assert created.incoming_ref == _ref(estate.prod, estate.ids["prod_new"])
    assert new_ref not in {_ref(dev, estate.ids["dev_new"])}
    got = get_work(dev, GetWorkParams(ref=new_ref))
    assert [c.body for c in got.comments] == ["hi"]
    assert [(r.kind, r.related_id) for r in got.item.relations] == [
        ("depends_on", estate.ids["neither"])
    ]
    # Comments append, deduplicated by id, from both sides.
    # (ordered by when each was written, which was earlier on prod's clock).
    assert _get(dev, _ref(dev, estate.ids["neither"]))[2] == ["before", "prod", "dev"]
    # Dev's own new item is untouched.
    assert _get(dev, _ref(dev, estate.ids["dev_new"]))[0] == "Born on dev"

    assert result.by_entity["work_item"].created == 1
    assert result.by_entity["work_item"].updated == 1
    assert result.by_entity["work_item"].conflict == 1
    assert result.by_entity["comment"].created == 2
    assert result.by_entity["relation"].created == 1

    with dev.declared.read() as view:
        audit = view.list_audit(limit=1, offset=0)
        events = view.list_events(after=0, limit=10_000)
    assert audit[0].operation == "import"
    assert audit[0].reason == WHY
    assert [e.kind for e in events].count("instance.imported") == 1


def test_a_re_import_changes_nothing(estate: Estate) -> None:
    source = estate.export(estate.prod, "prod")
    _apply(estate.dev, source)
    before = _revision(estate.dev)
    both = _ref(estate.dev, estate.ids["both"])
    comments_before = _get(estate.dev, both)[2]

    again = _apply(estate.dev, source)
    assert (again.created, again.updated) == (0, 0)
    # The conflict is still a conflict — it was never resolved — but it is
    # recorded once, not once per import.
    assert again.conflicted == 1
    assert _get(estate.dev, both)[2] == comments_before
    # Only the import's own audit row moved the revision.
    assert _revision(estate.dev) == before + 1


# -- dev's export carried back to prod (the export is the clone) -----------


def test_dev_carried_back_to_prod(estate: Estate) -> None:
    source = estate.export(estate.dev, "dev")
    prod = estate.prod
    result = _apply(prod, source)
    assert "is a clone of this one" in result.base_source

    assert _get(prod, _ref(prod, estate.ids["dev_only"]))[0] == "Dev's new title"
    assert _get(prod, _ref(prod, estate.ids["prod_only"]))[0] == "Prod's new title"
    title, priority, comments = _get(prod, _ref(prod, estate.ids["both"]))
    assert (title, priority) == ("Changed on both", "p0")
    assert any("Dev retitled it" in body for body in comments)
    assert _get(prod, _ref(prod, estate.ids["dev_new"]))[0] == "Born on dev"
    assert _get(prod, _ref(prod, estate.ids["neither"]))[2] == [
        "before",
        "prod",
        "dev",
    ]


def test_write_back_and_identity_are_never_imported(estate: Estate) -> None:
    register_project(
        estate.dev,
        RegisterProjectParams(
            name="Fresh", root_path=str(estate.tmp / "fresh"), reason=WHY
        ),
    )
    with estate.dev.declared.write() as txn:
        project = txn.project_by_slug("fresh")
        assert project is not None
        txn.update_project(
            project.id, ProjectUpdate(write_back="full"), at=estate.dev.clock()
        )
    source = estate.export(estate.dev, "dev")
    prod_id = _instance_id(estate.prod)
    with estate.prod.declared.read() as view:
        tokens_before = view.list_tokens(include_revoked=True, limit=1000)
    _apply(estate.prod, source)

    with estate.prod.declared.read() as view:
        fresh = view.project_by_slug("fresh")
        assert view.instance_id() == prod_id
        assert view.clone_stamp() is None
        assert view.list_tokens(include_revoked=True, limit=1000) == tokens_before
    assert fresh is not None
    assert fresh.write_back == "none"
    assert fresh.link_state == "unlinked"


# -- scope and old files ---------------------------------------------------


def test_project_scope_merges_only_that_project(estate: Estate) -> None:
    register_project(
        estate.prod,
        RegisterProjectParams(
            name="Other", root_path=str(estate.tmp / "other"), reason=WHY
        ),
    )
    create_work(
        estate.prod,
        CreateWorkParams(
            kind="bug", title="Elsewhere", project="other", local_only=True, reason=WHY
        ),
    )
    source = estate.export(estate.prod, "prod")
    result = _apply(estate.dev, source, project="shipped")
    assert result.project == "shipped"
    with estate.dev.declared.read() as view:
        assert view.project_by_slug("other") is None
    assert _ref_by_id(estate.dev, estate.ids["prod_new"]) is not None

    with pytest.raises(NotFound, match="no project"):
        _import(estate.dev, source, project="absent")


def test_unrelated_instances_have_no_baseline(estate: Estate, tmp_path: Path) -> None:
    """Without a clone relationship nothing says which side changed."""
    stranger = _instance(tmp_path, "stranger", start=T0)
    source = estate.export(estate.prod, "prod")
    result = _import(stranger, source)
    assert result.base is None
    assert "no baseline" in result.base_source
    # Everything is new to the stranger, so nothing can conflict yet.
    assert result.conflicted == 0
    assert result.by_entity["work_item"].created == 5


def test_a_version_1_export_is_report_only(tmp_path: Path, estate: Estate) -> None:
    old = tmp_path / "v1.json"
    old.write_text(
        json.dumps(
            {
                "instance_id": "ins_old",
                "exported_at": "2026-01-01T00:00:00Z",
                "projects": [{}],
                "work_items": [{}, {}],
            }
        ),
        encoding="utf-8",
    )
    result = _import(estate.dev, str(old))
    assert result.applied is False
    assert result.export_format_version == 1
    assert result.work_items == 2
    assert "report-only" in result.detail
    with pytest.raises(InvalidRequest, match="not applied"):
        _apply(estate.dev, str(old))


def test_a_future_export_is_refused(tmp_path: Path, estate: Estate) -> None:
    future = tmp_path / "v9.json"
    future.write_text(json.dumps({"export_format_version": 9}), encoding="utf-8")
    with pytest.raises(InvalidRequest, match="newer build"):
        _import(estate.dev, str(future))
