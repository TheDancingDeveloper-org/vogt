"""Applying import: merge an export into a live instance.

`vogt import --source F` reads an export written by `vogt export` and merges
it into this instance. It is a **dry run by default**: the report below is
produced either way, and only `--apply --confirm` writes it. The write is
one `audited_write` (operation `import`, event `instance.imported`), so an
apply lands whole or not at all.

**Identity.** Entities are matched on what stays stable between instances,
never on what an instance allocates locally:

- projects by slug (a differing `root_path` is reported, never changed);
- work items by stable id, never by `WI-n` ref — two instances number
  independently, so a created item gets a fresh ref from this instance's
  counter and the report names both;
- initiatives by slug, labels by name, actors by `identity_ref`;
- comments by id (append-only), relations by (from, kind, to).

**Policy** (the decision itself is `core.merge.decide`). "Changed" means an
entity's `updated_at` is later than the *baseline*, the moment the two
instances last agreed:

- new on the incoming side → created;
- changed only in the export → the incoming version is taken;
- changed only here → this instance's version is kept (skipped);
- changed on both, or no baseline → a conflict: this instance's version is
  kept and, for a work item, the incoming version is attached as a comment
  (once — a re-import does not repeat it). `--strict` refuses the whole
  import instead, writing nothing.

The baseline is the clone stamp. When the export's instance is a clone of
this one (carrying dev back to prod), or this instance is a clone of the
export's (refreshing dev), the backup's as-of time is when they agreed. An
export of this same instance is measured against its own `exported_at`.
Anything else has no baseline, and every difference is a conflict.

**Additive only.** An import never deletes: a relation, label or comment
missing from the export stays. **Never imported:** tokens, password
logins, forge accounts, write-back settings (a created project starts at
`write_back = none` and unlinked), push subscriptions, sessions, the
instance id and the clone stamp. Items retired upstream (`superseded_by`)
and items in a project that is upstream-truth here are skipped: the forge
holds those.

A version-1 export (written before this format existed) carries no comments
and no clone stamp, so it can be reported on but not applied.
"""

from __future__ import annotations

import hashlib
import json
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any

from pydantic import ValidationError

from vogt.application.context import AppContext
from vogt.application.models import (
    ImportAction,
    ImportChange,
    ImportEntityKind,
    ImportParams,
    ImportResult,
    ImportTally,
)
from vogt.application.writes import WriteOutcome, audited_write, validate_reason
from vogt.core.clock import from_iso, to_iso
from vogt.core.entities import (
    Actor,
    Comment,
    Initiative,
    Label,
    Project,
    RelationKind,
    WorkItem,
)
from vogt.core.merge import MergeDecision, decide
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import ProjectUpdate, ReadView, WorkItemUpdate, WriteTxn

IMPORT_OPERATION = "import"
IMPORT_EVENT = "instance.imported"
#: The newest export format this build can apply.
APPLICABLE_FORMAT = 2
_ENTITIES: tuple[ImportEntityKind, ...] = (
    "actor",
    "label",
    "project",
    "initiative",
    "work_item",
    "relation",
    "comment",
)
_BIG = 100_000


# -- reading the file ------------------------------------------------------


@dataclass(frozen=True)
class _Export:
    format_version: int
    instance_id: str
    exported_at: datetime | None
    clone_source: str | None
    clone_backup_taken_at: datetime | None
    projects: list[Project]
    items: list[WorkItem]
    initiatives: list[Initiative]
    labels: list[Label]
    actors: list[Actor]
    comments: list[Comment]


def _read(source: Path) -> dict[str, Any]:
    if not source.is_file():
        msg = f"no such export: {source}"
        raise NotFound(msg)
    try:
        raw = json.loads(source.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        msg = f"{source} is not a readable export: {exc}"
        raise InvalidRequest(msg) from exc
    if not isinstance(raw, dict):
        msg = f"{source} is not a Vogt export"
        raise InvalidRequest(msg)
    return raw


def _format_version(raw: dict[str, Any]) -> int:
    value = raw.get("export_format_version", 1)
    try:
        return int(value)
    except (TypeError, ValueError) as exc:
        msg = f"export_format_version {value!r} is not a number"
        raise InvalidRequest(msg) from exc


def _parse(raw: dict[str, Any], source: Path) -> _Export:
    stamp = raw.get("clone_stamp") or {}
    exported_at = raw.get("exported_at")
    try:
        return _Export(
            format_version=_format_version(raw),
            instance_id=str(raw.get("instance_id", "")),
            exported_at=None if exported_at is None else from_iso(str(exported_at)),
            clone_source=stamp.get("source_instance_id"),
            clone_backup_taken_at=(
                None
                if stamp.get("backup_taken_at") is None
                else from_iso(str(stamp["backup_taken_at"]))
            ),
            projects=[Project.model_validate(p) for p in raw.get("projects", [])],
            items=[WorkItem.model_validate(w) for w in raw.get("work_items", [])],
            initiatives=[
                Initiative.model_validate(i) for i in raw.get("initiatives", [])
            ],
            labels=[Label.model_validate(label) for label in raw.get("labels", [])],
            actors=[Actor.model_validate(a) for a in raw.get("actors", [])],
            comments=[Comment.model_validate(c) for c in raw.get("comments", [])],
        )
    except (ValidationError, ValueError, AttributeError) as exc:
        msg = f"{source} does not hold a well-formed export: {exc}"
        raise InvalidRequest(msg) from exc


def _baseline(
    export: _Export, *, instance_id: str, view: ReadView
) -> tuple[datetime | None, str]:
    """When the two instances last agreed, and how that is known."""
    if export.instance_id == instance_id:
        if export.exported_at is None:
            return None, "an export of this instance with no exported_at"
        return (
            export.exported_at,
            "an export of this same instance: its exported_at",
        )
    if export.clone_source == instance_id and export.clone_backup_taken_at:
        return (
            export.clone_backup_taken_at,
            f"instance {export.instance_id} is a clone of this one; the "
            "cloned backup's as-of time",
        )
    stamp = view.clone_stamp()
    if stamp is not None and stamp.source_instance_id == export.instance_id:
        return (
            stamp.backup_taken_at,
            f"this instance is a clone of {export.instance_id}; the cloned "
            "backup's as-of time",
        )
    return (
        None,
        f"no clone relationship between this instance and {export.instance_id}: "
        "no baseline, so every difference is a conflict",
    )


# -- the plan --------------------------------------------------------------


@dataclass(frozen=True)
class _ConflictNote:
    work_item_id: str
    comment_id: str
    body: str


@dataclass
class _Plan:
    actors: list[Actor] = field(default_factory=list)
    labels: list[Label] = field(default_factory=list)
    projects: list[Project] = field(default_factory=list)
    project_updates: list[tuple[str, ProjectUpdate]] = field(default_factory=list)
    initiatives: list[Initiative] = field(default_factory=list)
    initiative_updates: list[Initiative] = field(default_factory=list)
    items: list[tuple[WorkItem, ImportChange]] = field(default_factory=list)
    item_updates: list[tuple[str, WorkItemUpdate]] = field(default_factory=list)
    relations: list[tuple[str, str, RelationKind]] = field(default_factory=list)
    comments: list[Comment] = field(default_factory=list)
    conflict_notes: list[_ConflictNote] = field(default_factory=list)
    changes: list[ImportChange] = field(default_factory=list)
    unchanged: dict[str, int] = field(default_factory=dict)

    def record(
        self,
        entity: ImportEntityKind,
        key: str,
        action: ImportAction,
        *,
        detail: str = "",
        fields: list[str] | None = None,
        ref: str | None = None,
        incoming_ref: str | None = None,
    ) -> ImportChange:
        change = ImportChange(
            entity=entity,
            key=key,
            action=action,
            ref=ref,
            incoming_ref=incoming_ref,
            fields=fields or [],
            detail=detail,
        )
        self.changes.append(change)
        return change

    def same(self, entity: ImportEntityKind) -> None:
        self.unchanged[entity] = self.unchanged.get(entity, 0) + 1

    @property
    def conflicts(self) -> list[ImportChange]:
        return [c for c in self.changes if c.action == "conflict"]


_DECISION_DETAIL: dict[MergeDecision, str] = {
    "unchanged": "",
    "take_incoming": "changed only in the export since the baseline; taken",
    "keep_target": "changed only here since the baseline; this version kept",
    "conflict": "changed on both sides (or no baseline); this version kept",
}


class _Planner:
    """Decide, entity by entity, what an import would do. Writes nothing."""

    def __init__(
        self,
        view: ReadView,
        export: _Export,
        *,
        scope: str | None,
        base: datetime | None,
        ids: Callable[[str], str],
        now: datetime,
    ) -> None:
        self.view = view
        self.now = now
        self.export = export
        self.base = base
        self.ids = ids
        self.plan = _Plan()
        if scope is not None and not any(p.slug == scope for p in export.projects):
            msg = f"the export holds no project with slug {scope!r}"
            raise NotFound(msg)
        self.items = [
            item for item in export.items if scope is None or item.project_slug == scope
        ]
        item_ids = {item.id for item in self.items}
        self.comments = [c for c in export.comments if c.work_item_id in item_ids]
        self.projects = [p for p in export.projects if scope is None or p.slug == scope]
        self.initiatives = export.initiatives
        self.labels = export.labels
        self.actors = export.actors
        if scope is not None:
            used_initiatives = {i.initiative_id for i in self.items if i.initiative_id}
            used_labels = {name for item in self.items for name in item.labels}
            used_actors = {i.assignee_identity_ref for i in self.items}
            authors = {c.actor_id for c in self.comments}
            self.initiatives = [i for i in self.initiatives if i.id in used_initiatives]
            self.labels = [label for label in self.labels if label.name in used_labels]
            self.actors = [
                a
                for a in self.actors
                if a.identity_ref in used_actors or a.id in authors
            ]
        self.incoming_initiative_slug = {i.id: i.slug for i in export.initiatives}
        self.incoming_actor_ref = {a.id: a.identity_ref for a in export.actors}
        # Target-side identities, including what this plan will create.
        self.actor_ids: dict[str, str] = {}  # identity_ref -> target actor id
        existing_labels = view.list_labels(limit=_BIG, offset=0)
        self.label_names: set[str] = {label.name for label in existing_labels}
        self.label_ids: set[str] = {label.id for label in existing_labels}
        self.project_ids: dict[str, str] = {}  # slug -> target project id
        self.initiative_ids: dict[str, str] = {}  # incoming id -> target id
        self.present_items: set[str] = set()  # item ids here, after the plan
        self.target_comments: dict[str, set[str]] = {}

    # -- helpers -----------------------------------------------------------

    def _decide(
        self, *, equal: bool, target_at: datetime, incoming_at: datetime
    ) -> MergeDecision:
        return decide(
            equal=equal,
            base=self.base,
            target_updated_at=target_at,
            incoming_updated_at=incoming_at,
        )

    def _fresh_id(self, wanted: str, prefix: str, taken: bool) -> str:
        return self.ids(prefix) if taken else wanted

    def _actor_id(self, identity_ref: str | None) -> str | None:
        if identity_ref is None:
            return None
        if identity_ref in self.actor_ids:
            return self.actor_ids[identity_ref]
        actor = self.view.actor_by_identity(identity_ref)
        if actor is None:
            return None
        self.actor_ids[identity_ref] = actor.id
        return actor.id

    def _project_id(self, slug: str) -> tuple[str | None, Project | None]:
        if slug in self.project_ids:
            return self.project_ids[slug], self.view.project_by_slug(slug)
        project = self.view.project_by_slug(slug)
        return (None, None) if project is None else (project.id, project)

    def _ensure_label(self, name: str) -> None:
        if name in self.label_names:
            return
        label_id = self.ids("lbl")
        self.plan.labels.append(
            Label(id=label_id, name=name, color=None, created_at=self.now)
        )
        self.label_names.add(name)
        self.label_ids.add(label_id)
        self.plan.record("label", name, "created", detail="referenced by an item")

    def _comment_ids(self, work_item_id: str) -> set[str]:
        if work_item_id not in self.target_comments:
            self.target_comments[work_item_id] = {
                c.id for c in self.view.comments_for(work_item_id, limit=_BIG)
            }
        return self.target_comments[work_item_id]

    # -- the passes, in dependency order -----------------------------------

    def run(self) -> _Plan:
        self._actors()
        self._labels()
        self._projects()
        self._initiatives()
        self._work_items()
        self._relations()
        self._comments()
        return self.plan

    def _actors(self) -> None:
        for incoming in self.actors:
            existing = self.view.actor_by_identity(incoming.identity_ref)
            if existing is not None:
                self.actor_ids[incoming.identity_ref] = existing.id
                self.plan.same("actor")
                continue
            actor_id = self._fresh_id(
                incoming.id, "act", self.view.actor_by_id(incoming.id) is not None
            )
            self.plan.actors.append(incoming.model_copy(update={"id": actor_id}))
            self.actor_ids[incoming.identity_ref] = actor_id
            self.plan.record("actor", incoming.identity_ref, "created")

    def _labels(self) -> None:
        for incoming in self.labels:
            if incoming.name in self.label_names:
                self.plan.same("label")
                continue
            label_id = self._fresh_id(incoming.id, "lbl", incoming.id in self.label_ids)
            self.plan.labels.append(incoming.model_copy(update={"id": label_id}))
            self.label_names.add(incoming.name)
            self.label_ids.add(label_id)
            self.plan.record("label", incoming.name, "created")

    def _projects(self) -> None:
        for incoming in self.projects:
            target = self.view.project_by_slug(incoming.slug)
            if target is None:
                self._create_project(incoming)
                continue
            self.project_ids[incoming.slug] = target.id
            notes: list[str] = []
            if target.root_path != incoming.root_path:
                notes.append(
                    f"root_path differs ({incoming.root_path} in the export); "
                    f"kept {target.root_path}"
                )
            diff: dict[str, object] = {}
            for name in ("lifecycle_state", "repo_url", "current_version"):
                theirs = getattr(incoming, name)
                if theirs != getattr(target, name):
                    diff[name] = theirs
            if sorted(incoming.exclusions) != sorted(target.exclusions):
                diff["exclusions"] = tuple(incoming.exclusions)
            decision = self._decide(
                equal=not diff,
                target_at=target.updated_at,
                incoming_at=incoming.updated_at,
            )
            if decision == "unchanged":
                if notes:
                    self.plan.record(
                        "project", incoming.slug, "skipped", detail="; ".join(notes)
                    )
                else:
                    self.plan.same("project")
                continue
            fields = sorted(diff)
            if decision == "take_incoming":
                cleared = [k for k, v in diff.items() if v is None]
                for name in cleared:
                    del diff[name]
                    notes.append(f"{name} cannot be cleared by an import; kept")
                if diff:
                    self.plan.project_updates.append(
                        (target.id, ProjectUpdate(**diff))  # type: ignore[arg-type]
                    )
                action: ImportAction = "updated" if diff else "skipped"
            else:
                action = "conflict" if decision == "conflict" else "skipped"
            self.plan.record(
                "project",
                incoming.slug,
                action,
                fields=fields,
                detail="; ".join([_DECISION_DETAIL[decision], *notes]),
            )

    def _create_project(self, incoming: Project) -> None:
        project_id = self._fresh_id(
            incoming.id, "prj", self.view.project_by_id(incoming.id) is not None
        )
        notes = [f"root_path {incoming.root_path} taken as-is"]
        if incoming.write_back != "none":
            notes.append(f"write_back {incoming.write_back} not imported (none)")
        if incoming.link_state != "unlinked":
            notes.append(f"link_state {incoming.link_state} not imported (unlinked)")
        self.plan.projects.append(
            incoming.model_copy(
                update={
                    "id": project_id,
                    "write_back": "none",
                    "link_state": "unlinked",
                    "compliance_status": "not_checked",
                    "compliance_checked_at": None,
                    "trust_state": "unverified",
                }
            )
        )
        self.project_ids[incoming.slug] = project_id
        self.plan.record("project", incoming.slug, "created", detail="; ".join(notes))

    def _initiatives(self) -> None:
        for incoming in self.initiatives:
            target = self.view.initiative_by_slug(incoming.slug)
            if target is None:
                initiative_id = self._fresh_id(
                    incoming.id,
                    "ini",
                    self.view.initiative_by_id(incoming.id) is not None,
                )
                self.plan.initiatives.append(
                    incoming.model_copy(update={"id": initiative_id})
                )
                self.initiative_ids[incoming.id] = initiative_id
                self.plan.record("initiative", incoming.slug, "created")
                continue
            self.initiative_ids[incoming.id] = target.id
            fields = [
                name
                for name in ("title", "body", "state", "weight")
                if getattr(incoming, name) != getattr(target, name)
            ]
            decision = self._decide(
                equal=not fields,
                target_at=target.updated_at,
                incoming_at=incoming.updated_at,
            )
            if decision == "unchanged":
                self.plan.same("initiative")
                continue
            if decision == "take_incoming":
                self.plan.initiative_updates.append(
                    target.model_copy(
                        update={
                            "title": incoming.title,
                            "body": incoming.body,
                            "state": incoming.state,
                            "weight": incoming.weight,
                            "updated_at": self.now,
                        }
                    )
                )
            self.plan.record(
                "initiative",
                incoming.slug,
                _action(decision),
                fields=fields,
                detail=_DECISION_DETAIL[decision],
            )

    def _incoming_view(self, item: WorkItem) -> dict[str, object]:
        return {
            "kind": item.kind,
            "title": item.title,
            "body": item.body,
            "state": item.state,
            "priority": item.priority,
            "effort": item.effort,
            "project": item.project_slug,
            "initiative": (
                None
                if item.initiative_id is None
                else self.incoming_initiative_slug.get(item.initiative_id)
            ),
            "assignee": item.assignee_identity_ref,
            "labels": sorted(item.labels),
        }

    def _target_view(self, item: WorkItem) -> dict[str, object]:
        initiative = (
            None
            if item.initiative_id is None
            else self.view.initiative_by_id(item.initiative_id)
        )
        return {
            "kind": item.kind,
            "title": item.title,
            "body": item.body,
            "state": item.state,
            "priority": item.priority,
            "effort": item.effort,
            "project": item.project_slug,
            "initiative": None if initiative is None else initiative.slug,
            "assignee": item.assignee_identity_ref,
            "labels": sorted(item.labels),
        }

    def _work_items(self) -> None:
        for incoming in self.items:
            self._work_item(incoming)

    def _work_item(self, incoming: WorkItem) -> None:
        key, ref = incoming.id, incoming.ref
        if incoming.superseded_by is not None:
            self.plan.record(
                "work_item",
                key,
                "skipped",
                incoming_ref=ref,
                detail=f"retired upstream in the export ({incoming.superseded_by})",
            )
            return
        target = self.view.work_item_by_id(incoming.id)
        if target is not None and target.superseded_by is not None:
            self.present_items.add(target.id)
            self.plan.record(
                "work_item",
                key,
                "skipped",
                ref=target.ref,
                incoming_ref=ref,
                detail=f"retired upstream here ({target.superseded_by})",
            )
            return

        project_id: str | None = None
        if incoming.project_slug is not None:
            project_id, project = self._project_id(incoming.project_slug)
            if project_id is None:
                self.plan.record(
                    "work_item",
                    key,
                    "skipped",
                    ref=None if target is None else target.ref,
                    incoming_ref=ref,
                    detail=(
                        f"its project {incoming.project_slug} is neither here "
                        "nor in the export"
                    ),
                )
                if target is not None:
                    self.present_items.add(target.id)
                return
            if project is not None and project.link_state == "linked":
                if target is not None:
                    self.present_items.add(target.id)
                self.plan.record(
                    "work_item",
                    key,
                    "skipped",
                    ref=None if target is None else target.ref,
                    incoming_ref=ref,
                    detail=(
                        f"project {incoming.project_slug} is upstream-truth "
                        "here; its items live on the forge"
                    ),
                )
                return
        if incoming.state not in self.view.workflow_for(incoming.kind).states:
            if target is not None:
                self.present_items.add(target.id)
            self.plan.record(
                "work_item",
                key,
                "skipped",
                ref=None if target is None else target.ref,
                incoming_ref=ref,
                detail=(
                    f"state {incoming.state!r} is not in this instance's "
                    f"{incoming.kind} workflow"
                ),
            )
            return

        initiative_id = (
            None
            if incoming.initiative_id is None
            else self.initiative_ids.get(incoming.initiative_id)
        )
        assignee_id = self._actor_id(incoming.assignee_identity_ref)

        if target is None:
            for name in incoming.labels:
                self._ensure_label(name)
            created = incoming.model_copy(
                update={
                    "ref": "",
                    "project_id": project_id,
                    "initiative_id": initiative_id,
                    "assignee_actor_id": assignee_id,
                    "relations": [],
                }
            )
            change = self.plan.record(
                "work_item",
                key,
                "created",
                incoming_ref=ref,
                detail="a fresh ref is assigned from this instance's counter",
            )
            self.plan.items.append((created, change))
            self.present_items.add(incoming.id)
            return

        self.present_items.add(target.id)
        theirs = self._incoming_view(incoming)
        ours = self._target_view(target)
        fields = [name for name in theirs if theirs[name] != ours[name]]
        decision = self._decide(
            equal=not fields,
            target_at=target.updated_at,
            incoming_at=incoming.updated_at,
        )
        if decision == "take_incoming" and "kind" in fields:
            decision = "conflict"
        if decision == "unchanged":
            self.plan.same("work_item")
            return
        detail = _DECISION_DETAIL[decision]
        if decision == "take_incoming":
            if "labels" in fields:
                for name in incoming.labels:
                    self._ensure_label(name)
            self.plan.item_updates.append(
                (
                    target.id,
                    _item_update(
                        fields,
                        incoming,
                        target,
                        project_id=project_id,
                        initiative_id=initiative_id,
                        assignee_id=assignee_id,
                    ),
                )
            )
        elif decision == "conflict":
            note = _conflict_note(
                target, incoming, fields, theirs, instance_id=self.export.instance_id
            )
            if note.comment_id in self._comment_ids(target.id):
                detail += "; the incoming version is already recorded as a comment"
            else:
                self.plan.conflict_notes.append(note)
                detail += "; the incoming version is attached as a comment"
        self.plan.record(
            "work_item",
            key,
            _action(decision),
            ref=target.ref,
            incoming_ref=ref,
            fields=fields,
            detail=detail,
        )

    def _relations(self) -> None:
        for incoming in self.items:
            if incoming.id not in self.present_items:
                continue
            target = self.view.work_item_by_id(incoming.id)
            existing = (
                set()
                if target is None
                else {(r.kind, r.related_id) for r in target.relations}
            )
            for relation in incoming.relations:
                key = f"{incoming.ref} -{relation.kind}-> {relation.related_ref}"
                if (relation.kind, relation.related_id) in existing:
                    self.plan.same("relation")
                    continue
                if relation.related_id not in self.present_items and (
                    self.view.work_item_by_id(relation.related_id) is None
                ):
                    self.plan.record(
                        "relation",
                        key,
                        "skipped",
                        detail=f"{relation.related_ref} is not here",
                    )
                    continue
                self.plan.relations.append(
                    (incoming.id, relation.related_id, relation.kind)
                )
                self.plan.record("relation", key, "created")

    def _comments(self) -> None:
        for incoming in self.comments:
            if incoming.work_item_id not in self.present_items:
                self.plan.record(
                    "comment",
                    incoming.id,
                    "skipped",
                    detail="its work item was not imported",
                )
                continue
            if incoming.id in self._comment_ids(incoming.work_item_id):
                self.plan.same("comment")
                continue
            author_ref = self.incoming_actor_ref.get(incoming.actor_id)
            actor_id = self._actor_id(author_ref)
            if actor_id is None:
                self.plan.record(
                    "comment",
                    incoming.id,
                    "skipped",
                    detail="its author is neither here nor in the export",
                )
                continue
            self.plan.comments.append(
                incoming.model_copy(update={"actor_id": actor_id})
            )
            self._comment_ids(incoming.work_item_id).add(incoming.id)
            self.plan.record("comment", incoming.id, "created")


def _action(decision: MergeDecision) -> ImportAction:
    if decision == "take_incoming":
        return "updated"
    if decision == "conflict":
        return "conflict"
    return "skipped"


def _item_update(
    fields: list[str],
    incoming: WorkItem,
    target: WorkItem,
    *,
    project_id: str | None,
    initiative_id: str | None,
    assignee_id: str | None,
) -> WorkItemUpdate:
    changes: dict[str, Any] = {}
    for name in ("title", "body", "state", "priority"):
        if name in fields:
            changes[name] = getattr(incoming, name)
    if "effort" in fields:
        if incoming.effort is None:
            changes["clear_effort"] = True
        else:
            changes["effort"] = incoming.effort
    if "project" in fields and project_id is not None:
        changes["project_id"] = project_id
    if "initiative" in fields:
        if initiative_id is None:
            changes["clear_initiative"] = True
        else:
            changes["initiative_id"] = initiative_id
    if "assignee" in fields:
        if assignee_id is None:
            changes["clear_assignee"] = True
        else:
            changes["assignee_actor_id"] = assignee_id
    if "labels" in fields:
        changes["add_labels"] = tuple(sorted(set(incoming.labels) - set(target.labels)))
        changes["remove_labels"] = tuple(
            sorted(set(target.labels) - set(incoming.labels))
        )
    return WorkItemUpdate(**changes)


def _conflict_note(
    target: WorkItem,
    incoming: WorkItem,
    fields: list[str],
    theirs: dict[str, object],
    *,
    instance_id: str,
) -> _ConflictNote:
    """The incoming version, kept beside the target's as a comment.

    Its id is derived from the item and the incoming version, so importing
    the same export twice records the conflict once.
    """
    digest = hashlib.sha256(
        json.dumps(
            [incoming.id, to_iso(incoming.updated_at), theirs],
            sort_keys=True,
            default=str,
        ).encode()
    ).hexdigest()[:26]
    lines = [
        f"Import conflict: {target.ref} changed here and in instance {instance_id} "
        f"(as {incoming.ref}, updated {to_iso(incoming.updated_at)}). This "
        "instance's version was kept. The incoming version of each differing "
        "field:",
        "",
    ]
    for name in fields:
        lines.append(f"- {name}: {json.dumps(theirs[name], default=str)}")
    return _ConflictNote(
        work_item_id=target.id, comment_id=f"cmt_import_{digest}", body="\n".join(lines)
    )


# -- applying it -----------------------------------------------------------


@dataclass(frozen=True)
class _Applied:
    plan: _Plan
    base: datetime | None
    base_source: str


def _apply(txn: WriteTxn, plan: _Plan, actor: Actor, now: datetime) -> None:
    for new_actor in plan.actors:
        txn.insert_actor(new_actor)
    for label in plan.labels:
        txn.insert_label(label)
    for project in plan.projects:
        txn.insert_project(project)
    for project_id, update in plan.project_updates:
        txn.update_project(project_id, update, at=now)
    for initiative in plan.initiatives:
        txn.insert_initiative(initiative)
    for initiative in plan.initiative_updates:
        txn.update_initiative(initiative)
    for item, change in plan.items:
        ref = txn.next_work_ref()
        txn.insert_work_item(item.model_copy(update={"ref": ref}))
        change.ref = ref
    for item_id, item_update in plan.item_updates:
        txn.update_work_item(item_id, item_update, at=now)
    for from_id, to_id, kind in plan.relations:
        txn.insert_relation(work_item_id=from_id, related_id=to_id, kind=kind, at=now)
    for comment in plan.comments:
        txn.insert_comment(comment)
    for note in plan.conflict_notes:
        txn.insert_comment(
            Comment(
                id=note.comment_id,
                work_item_id=note.work_item_id,
                actor_id=actor.id,
                actor_display_name=actor.display_name,
                body=note.body,
                created_at=now,
            )
        )


def _tallies(plan: _Plan) -> dict[str, ImportTally]:
    counts: dict[str, dict[str, int]] = {name: {} for name in _ENTITIES}
    for change in plan.changes:
        bucket = counts[change.entity]
        bucket[change.action] = bucket.get(change.action, 0) + 1
    for name, n in plan.unchanged.items():
        counts[name]["unchanged"] = n
    return {
        name: ImportTally.model_validate(bucket)
        for name, bucket in counts.items()
        if bucket
    }


def _strict_refusal(plan: _Plan) -> Conflict:
    named = ", ".join(f"{c.entity} {c.ref or c.key}" for c in plan.conflicts[:20])
    more = "" if len(plan.conflicts) <= 20 else f" and {len(plan.conflicts) - 20} more"
    return Conflict(
        f"--strict: {len(plan.conflicts)} entities changed on both sides "
        f"({named}{more}); nothing was imported"
    )


def merge_export(ctx: AppContext, params: ImportParams) -> ImportResult:
    """Plan the merge, and write it only when asked to (see the module)."""
    reason = validate_reason(params.reason)
    source = Path(params.source).expanduser()
    raw = _read(source)
    version = _format_version(raw)
    if version > APPLICABLE_FORMAT:
        msg = (
            f"export format {version} is newer than {APPLICABLE_FORMAT}; run a "
            "newer build rather than guessing what it contains"
        )
        raise InvalidRequest(msg)
    if version < APPLICABLE_FORMAT:
        if params.apply:
            msg = (
                f"{source} is a format-{version} export: it carries no comments "
                "or clone stamp, so it can be reported on but not applied. "
                "Export again from the source with this build."
            )
            raise InvalidRequest(msg)
        return ImportResult(
            source=str(source),
            instance_id=str(raw.get("instance_id", "")),
            export_format_version=version,
            projects=len(raw.get("projects", [])),
            work_items=len(raw.get("work_items", [])),
            applied=False,
            detail=(
                f"A format-{version} export is report-only: it predates "
                "applying import and carries no comments or clone stamp. "
                "Export again with this build to merge it."
            ),
        )
    export = _parse(raw, source)
    if params.apply and not params.confirm:
        msg = (
            f"this merges {source} (instance {export.instance_id}) into the live "
            "store. Run it without --apply to read the plan, then pass --confirm."
        )
        raise InvalidRequest(msg)

    def plan_on(view: ReadView, now: datetime) -> tuple[_Plan, datetime | None, str]:
        base, base_source = _baseline(export, instance_id=view.instance_id(), view=view)
        planner = _Planner(
            view,
            export,
            scope=params.project,
            base=base,
            ids=ctx.id_factory,
            now=now,
        )
        return planner.run(), base, base_source

    now = ctx.clock()
    if not params.apply:
        with ctx.declared.read() as view:
            plan, base, base_source = plan_on(view, now)
        detail = "Dry run: nothing was written. Pass --apply --confirm to merge."
        if params.strict and plan.conflicts:
            detail += f" --strict would refuse it: {len(plan.conflicts)} conflicts."
        return _result(source, export, params, plan, base, base_source, False, detail)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[_Applied]:
        plan, base, base_source = plan_on(txn, now)
        if params.strict and plan.conflicts:
            raise _strict_refusal(plan)
        _apply(txn, plan, actor, now)
        tallies = _tallies(plan)
        summary: dict[str, object] = {
            "source": str(source),
            "source_instance_id": export.instance_id,
            "project": params.project,
            "base": None if base is None else to_iso(base),
            "counts": {k: v.model_dump() for k, v in tallies.items()},
        }
        return WriteOutcome(
            result=_Applied(plan=plan, base=base, base_source=base_source),
            entity_kind="instance",
            entity_id=txn.instance_id(),
            payload=summary,
            event_kind=IMPORT_EVENT,
            summary=summary,
        )

    applied = audited_write(ctx, operation=IMPORT_OPERATION, reason=reason, body=body)
    return _result(
        source,
        export,
        params,
        applied.plan,
        applied.base,
        applied.base_source,
        True,
        "Applied in one audited write (operation import).",
    )


def _result(
    source: Path,
    export: _Export,
    params: ImportParams,
    plan: _Plan,
    base: datetime | None,
    base_source: str,
    applied: bool,
    detail: str,
) -> ImportResult:
    def total(action: str) -> int:
        return sum(1 for c in plan.changes if c.action == action)

    return ImportResult(
        source=str(source),
        instance_id=export.instance_id,
        export_format_version=export.format_version,
        projects=len(export.projects),
        work_items=len(export.items),
        applied=applied,
        detail=detail,
        project=params.project,
        base=base,
        base_source=base_source,
        created=total("created"),
        updated=total("updated"),
        conflicted=total("conflict"),
        skipped=total("skipped"),
        unchanged=sum(plan.unchanged.values()),
        by_entity=_tallies(plan),
        changes=plan.changes,
    )
