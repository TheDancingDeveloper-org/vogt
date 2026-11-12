"""The prod -> dev tracker snapshot importer (WI-4125)."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
from tools.vogt_snapshot_import import (
    Importer,
    ImportFailure,
    McpFailure,
    Plan,
    RefMap,
    Refused,
    State,
    build_plan,
    load_source,
    read_target,
    ref_number,
    slugify,
)

JSON = dict[str, Any]


# ---------------------------------------------------------------------------
# A small in-memory target that speaks the importer's subset of MCP tools.
# ---------------------------------------------------------------------------


class FakeTarget:
    def __init__(
        self,
        *,
        items: dict[str, JSON] | None = None,
        projects: dict[str, JSON] | None = None,
        write_back: str = "none",
        link_state: str = "unlinked",
        instance_id: str = "ins_dev",
    ) -> None:
        self.items: dict[str, JSON] = dict(items or {})
        self.projects: dict[str, JSON] = dict(projects or {})
        self.labels: set[str] = set()
        self.initiatives: set[str] = set()
        self.actors: dict[str, JSON] = {}
        self.comments: list[JSON] = []
        self.relations: set[tuple[str, str, str]] = set()
        self.calls: list[tuple[str, JSON]] = []
        self.mutations = 0
        self.fail_after: int | None = None
        self.initial_write_back = write_back
        self.initial_link_state = link_state
        self.instance_id = instance_id

    # -- transport ---------------------------------------------------------
    def tool(self, name: str, arguments: JSON | None = None) -> JSON:
        arguments = dict(arguments or {})
        self.calls.append((name, arguments))
        if name in _MUTATING:
            self.mutations += 1
            if self.fail_after is not None and self.mutations > self.fail_after:
                raise McpFailure(name, "unavailable", "injected target failure")
        result: object = getattr(self, "_" + name)(arguments)
        assert isinstance(result, dict)
        return result

    def _status(self, arguments: JSON) -> JSON:
        return {
            "instance_id": self.instance_id,
            "disk": {"path": "/srv/vogt/repos"},
            "counts": {"work_items": len(self.items)},
        }

    def _project_list(self, arguments: JSON) -> JSON:
        return {"projects": list(self.projects.values()), "total": len(self.projects)}

    def _label_list(self, arguments: JSON) -> JSON:
        rows = [{"name": name} for name in sorted(self.labels)]
        return {"labels": rows, "total": len(rows)}

    def _initiative_list(self, arguments: JSON) -> JSON:
        rows = [{"slug": slug} for slug in sorted(self.initiatives)]
        return {"initiatives": rows, "total": len(rows)}

    def _actor_list(self, arguments: JSON) -> JSON:
        rows = list(self.actors.values())
        return {"actors": rows, "total": len(rows)}

    def _work_list(self, arguments: JSON) -> JSON:
        rows = [
            {
                "ref": ref,
                "title": item["title"],
                "body": item.get("body", ""),
                "kind": item["kind"],
                "state": item["state"],
                "priority": item["priority"],
                "project_slug": item.get("project_slug"),
                "labels": item.get("labels", []),
            }
            for ref, item in sorted(
                self.items.items(), key=lambda pair: ref_number(pair[0]) or 0
            )
        ]
        return {"items": rows, "total": len(rows)}

    # -- writes ------------------------------------------------------------
    def _project_register(self, arguments: JSON) -> JSON:
        slug = slugify(arguments["name"])
        if slug in self.projects:
            raise McpFailure("project_register", "conflict", "already registered")
        self.projects[slug] = {
            "slug": slug,
            "name": arguments["name"],
            "root_path": arguments["root_path"],
            "repo_url": arguments.get("repo_url"),
            "lifecycle_state": arguments["lifecycle_state"],
            "write_back": "none",
            "link_state": "unlinked",
        }
        return {"project": self.projects[slug]}

    def _project_get(self, arguments: JSON) -> JSON:
        slug = arguments["slug"]
        if slug not in self.projects:
            raise McpFailure("project_get", "not_found", slug)
        return {"project": self.projects[slug]}

    def _label_create(self, arguments: JSON) -> JSON:
        self.labels.add(arguments["name"])
        return {"label": {"name": arguments["name"]}}

    def _initiative_create(self, arguments: JSON) -> JSON:
        slug = slugify(arguments["title"])
        self.initiatives.add(slug)
        return {"initiative": {"slug": slug}}

    def _actor_create(self, arguments: JSON) -> JSON:
        identity = arguments["identity_ref"]
        self.actors[identity] = {
            "identity_ref": identity,
            "display_name": arguments["display_name"],
            "kind": arguments["kind"],
        }
        return {"actor": self.actors[identity]}

    def _work_create(self, arguments: JSON) -> JSON:
        project = arguments.get("project")
        if project is not None and project not in self.projects:
            raise McpFailure("work_create", "not_found", f"project {project}")
        for label in arguments.get("labels") or []:
            if label not in self.labels:
                raise McpFailure("work_create", "not_found", f"label {label}")
        initiative = arguments.get("initiative")
        if initiative is not None and initiative not in self.initiatives:
            raise McpFailure("work_create", "not_found", f"initiative {initiative}")
        number = max((ref_number(ref) or 0 for ref in self.items), default=0) + 1
        ref = f"WI-{number}"
        self.items[ref] = {
            "ref": ref,
            "title": arguments["title"],
            "body": arguments["body"],
            "kind": arguments["kind"],
            "state": "open",
            "priority": arguments["priority"],
            "project_slug": project,
            "labels": list(arguments.get("labels") or []),
        }
        return {"item": {"ref": ref, "state": "open", "title": arguments["title"]}}

    def _work_update(self, arguments: JSON) -> JSON:
        item = self.items.get(arguments["ref"])
        if item is None:
            raise McpFailure("work_update", "not_found", arguments["ref"])
        item["title"] = arguments.get("title", item.get("title"))
        item["body"] = arguments.get("body", item.get("body", ""))
        item["priority"] = arguments.get("priority", item.get("priority"))
        if "project" in arguments:
            item["project_slug"] = arguments["project"]
        labels = set(item.get("labels", []))
        labels |= set(arguments.get("add_labels") or [])
        labels -= set(arguments.get("remove_labels") or [])
        item["labels"] = sorted(labels)
        return {"item": item}

    def _work_transition(self, arguments: JSON) -> JSON:
        item = self.items.get(arguments["ref"])
        if item is None:
            raise McpFailure("work_transition", "not_found", arguments["ref"])
        if item["state"] == arguments["to_state"]:
            raise McpFailure("work_transition", "conflict", "already in state")
        item["state"] = arguments["to_state"]
        return {"item": item}

    def _work_relate(self, arguments: JSON) -> JSON:
        key = (arguments["ref"], arguments["kind"], arguments["target"])
        if key in self.relations:
            raise McpFailure("work_relate", "conflict", "already related")
        self.relations.add(key)
        return {"item": self.items[arguments["ref"]]}

    def _work_comment(self, arguments: JSON) -> JSON:
        if arguments.get("kind") == "verdict" and arguments["ref"].endswith("2"):
            raise McpFailure(
                "work_comment", "person_required", "injected verdict refusal"
            )
        self.comments.append(
            {
                "ref": arguments["ref"],
                "body": arguments["body"],
                "kind": arguments["kind"],
            }
        )
        return {
            "comment": {"id": f"cmt_{len(self.comments)}", "kind": arguments["kind"]}
        }


_MUTATING = frozenset(
    {
        "project_register",
        "label_create",
        "initiative_create",
        "actor_create",
        "work_create",
        "work_update",
        "work_transition",
        "work_relate",
        "work_comment",
    }
)


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def source_payload(*, wi1_kind: str = "chore", superseded: bool = True) -> JSON:
    projects = [
        {
            "id": "prj_1",
            "slug": "alpha",
            "name": "alpha",
            "root_path": "/prod/alpha",
            "repo_url": "https://github.com/example/alpha",
            "lifecycle_state": "active",
            "exclusions": [".git/"],
            "write_back": "full",
            "link_state": "linked",
        },
        {
            "id": "prj_2",
            "slug": "beta",
            "name": "beta",
            "root_path": "/prod/beta",
            "repo_url": None,
            "lifecycle_state": "incubating",
            "exclusions": [".venv/"],
            "write_back": "none",
            "link_state": "unlinked",
        },
    ]
    items = [
        {
            "id": "wrk_1",
            "ref": "WI-1",
            "kind": wi1_kind,
            "title": "first prod item",
            "body": "one",
            "state": "open",
            "priority": "p2",
            "effort": None,
            "project_id": "prj_1",
            "project_slug": "alpha",
            "initiative_id": None,
            "origin": "created",
            "assignee_actor_id": None,
            "assignee_identity_ref": "agent:pod:prod",
            "labels": ["bug"],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "superseded_by": None,
            "relations": [
                {"kind": "relates_to", "related_id": "wrk_2", "related_ref": "WI-2"}
            ],
        },
        {
            "id": "wrk_2",
            "ref": "WI-2",
            "kind": "bug",
            "title": "second prod item",
            "body": "two",
            "state": "review",
            "priority": "p1",
            "effort": "m",
            "project_id": "prj_2",
            "project_slug": "beta",
            "initiative_id": "ini_1",
            "origin": "created",
            "assignee_actor_id": None,
            "assignee_identity_ref": None,
            "labels": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "superseded_by": None,
            "relations": [],
        },
        {
            "id": "wrk_3",
            "ref": "WI-3",
            "kind": "chore",
            "title": "third prod item",
            "body": "three",
            "state": "done",
            "priority": "p3",
            "effort": None,
            "project_id": "prj_1",
            "project_slug": "alpha",
            "initiative_id": None,
            "origin": "adopted",
            "assignee_actor_id": None,
            "assignee_identity_ref": None,
            "labels": [],
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "superseded_by": "gh:example/alpha#3" if superseded else None,
            "relations": [],
        },
    ]
    return {
        "export_format_version": 2,
        "instance_id": "ins_prod",
        "exported_at": "2026-11-12T00:00:00Z",
        "revision": 10,
        "scope": {"project": None},
        "clone_stamp": None,
        "projects": projects,
        "work_items": items,
        "initiatives": [
            {
                "id": "ini_1",
                "slug": "launch",
                "title": "launch",
                "body": "",
                "state": "open",
                "weight": 1,
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
            }
        ],
        "labels": [
            {
                "id": "lbl_1",
                "name": "bug",
                "color": "#d73a4a",
                "created_at": "2026-01-01T00:00:00Z",
            }
        ],
        "actors": [
            {
                "id": "act_1",
                "identity_ref": "agent:pod:prod",
                "display_name": "pod",
                "kind": "agent",
                "created_at": "2026-01-01T00:00:00Z",
                "disabled": False,
            },
        ],
        "comments": [
            {
                "id": "cmt_1",
                "work_item_id": "wrk_1",
                "actor_id": "act_1",
                "actor_display_name": "pod",
                "body": "a note",
                "created_at": "2026-01-03T00:00:00Z",
                "kind": "note",
                "handoff": None,
                "verdict": None,
                "verification": None,
            },
            {
                "id": "cmt_2",
                "work_item_id": "wrk_2",
                "actor_id": "act_1",
                "actor_display_name": "pod",
                "body": "a review",
                "created_at": "2026-01-04T00:00:00Z",
                "kind": "verdict",
                "handoff": None,
                "verdict": {
                    "head_sha": "a" * 40,
                    "base": "main",
                    "verdict": "no_blockers",
                    "blockers": [],
                    "deployment_paths_covered": [],
                    "reviewer_session": "ses_x",
                    "reviewer_started_by": "person",
                },
                "verification": None,
            },
        ],
    }


def write_source(tmp_path: Path, payload: JSON | None = None) -> Path:
    path = tmp_path / "export.json"
    path.write_text(json.dumps(payload or source_payload()), encoding="utf-8")
    return path


def test_read_target_refuses_the_snapshot_source_instance(tmp_path: Path) -> None:
    source = load_source(write_source(tmp_path))
    target = FakeTarget(instance_id=source.instance_id)
    with pytest.raises(Refused, match="instance"):
        read_target(target, source, url="http://127.0.0.1:8700")  # type: ignore[arg-type]


def test_the_not_prod_override_cannot_write_back_to_the_source_instance(
    tmp_path: Path,
) -> None:
    source = load_source(write_source(tmp_path))
    target = FakeTarget(instance_id=source.instance_id)
    with pytest.raises(Refused, match="instance"):
        read_target(
            target,  # type: ignore[arg-type]
            source,
            url="http://127.0.0.1:8700",
            allow_prod_hostname=True,
        )


def test_read_target_refuses_a_production_hostname(tmp_path: Path) -> None:
    source = load_source(write_source(tmp_path))
    for url in (
        "https://vogt.sprooty.com",
        "https://vogt-prod.sprooty.com",
        "https://vogt-prod.example.net:8443",
    ):
        with pytest.raises(Refused, match="production"):
            read_target(FakeTarget(), source, url=url)  # type: ignore[arg-type]


def test_the_not_prod_override_allows_a_foreign_production_hostname(
    tmp_path: Path,
) -> None:
    source = load_source(write_source(tmp_path))
    state = read_target(
        FakeTarget(instance_id="ins_other"),  # type: ignore[arg-type]
        source,
        url="https://vogt.sprooty.com",
        allow_prod_hostname=True,
    )
    assert state.instance_id == "ins_other"


def test_read_target_allows_a_normal_dev_target(tmp_path: Path) -> None:
    source = load_source(write_source(tmp_path))
    state = read_target(
        FakeTarget(instance_id="ins_dev"),  # type: ignore[arg-type]
        source,
        url="https://vogt-dev.sprooty.com",
    )
    assert state.instance_id == "ins_dev"


def target_with_wi1(*, kind: str = "chore") -> FakeTarget:
    return FakeTarget(
        items={
            "WI-1": {
                "ref": "WI-1",
                "title": "the dev smoke item",
                "kind": kind,
                "state": "open",
                "priority": "p2",
                "project_slug": "vogt",
                "labels": [],
            }
        },
        projects={
            "vogt": {
                "slug": "vogt",
                "name": "vogt",
                "root_path": "/srv/vogt/repos/vogt",
                "write_back": "none",
                "link_state": "unlinked",
            }
        },
    )


def plan_for(tmp_path: Path, target: FakeTarget, **kwargs: Any) -> Plan:
    ref_mode = kwargs.pop("ref_mode", "preserve")
    map_path = kwargs.pop("map_path", None)
    source = load_source(write_source(tmp_path, kwargs.pop("payload", None)))
    state = read_target(target, ref_mode=ref_mode)  # type: ignore[arg-type]
    state.url = "http://target"
    ref_map = None
    if ref_mode == "remap":
        ref_map = RefMap.open(
            map_path or tmp_path / "map.json", source=source, target_url="http://target"
        )
    return build_plan(
        source,
        state,
        project_root="/srv/vogt/repos",
        ref_policy=kwargs.pop("ref_policy", "sequence"),
        ref_mode=ref_mode,
        ref_map=ref_map,
        map_path=map_path or (tmp_path / "map.json" if ref_mode == "remap" else None),
        **kwargs,
    )


def apply_plan(
    tmp_path: Path, source_path: Path, target: FakeTarget, plan: Plan, **kwargs: Any
) -> Importer:
    ref_mode = kwargs.pop("ref_mode", plan.ref_mode)
    state = State.open(
        tmp_path / "state.json",
        source=plan.source,
        target_url="http://target",
        ref_mode=ref_mode,
    )
    ref_map = None
    if ref_mode == "remap":
        ref_map = RefMap.open(
            plan.map_path or tmp_path / "map.json",
            source=plan.source,
            target_url="http://target",
        )
    importer = Importer(
        target,  # type: ignore[arg-type]
        plan,
        state,
        reason="test",
        superseded_policy=kwargs.pop("superseded_policy", "wont_do"),
        comment_provenance=kwargs.pop("comment_provenance", True),
        strict_comments=kwargs.pop("strict_comments", False),
        project_root="/srv/vogt/repos",
        ref_mode=ref_mode,
        ref_map=ref_map,
        log=lambda _: None,
    )
    importer.run()
    return importer


# ---------------------------------------------------------------------------
# Source parsing
# ---------------------------------------------------------------------------


def test_source_round_trips_relations_and_comments(tmp_path: Path) -> None:
    source = load_source(write_source(tmp_path))
    assert [item.ref for item in source.items] == ["WI-1", "WI-2", "WI-3"]
    assert source.items[0].relations[0].target_ref == "WI-2"
    assert source.items[1].initiative_slug == "launch"
    assert source.comments[1].verdict is not None


def test_source_refuses_a_foreign_export_format(tmp_path: Path) -> None:
    payload = source_payload()
    payload["export_format_version"] = 1
    with pytest.raises(ImportFailure, match="export format"):
        load_source(write_source(tmp_path, payload))


def test_slugify_mirrors_the_core() -> None:
    assert slugify("Spot My Car!") == "spot-my-car"
    assert slugify("a__b") == "a-b"


# ---------------------------------------------------------------------------
# Planning
# ---------------------------------------------------------------------------


def test_plan_reports_and_forces_the_forge_safety_rewrite(tmp_path: Path) -> None:
    plan = plan_for(tmp_path, target_with_wi1())
    assert {project.slug for project in plan.projects_new} == {"alpha", "beta"}
    assert plan.write_back_forced == ["alpha"]
    assert plan.link_state_forced == ["alpha"]
    assert plan.replace_wi1 is True
    assert plan.first_new_ref == 2
    assert [item.ref for item in plan.items_new] == ["WI-2", "WI-3"]
    summary = plan.summary()
    assert summary["forge_safety"]["source_write_back_non_none"] == ["alpha"]
    assert summary["counts"]["comments"] == 2
    assert summary["refs"]["last_ref"] == 3


def test_plan_refuses_a_wi1_kind_the_target_cannot_update_in_place(
    tmp_path: Path,
) -> None:
    with pytest.raises(Refused, match="sequence preservation needs the same kind"):
        plan_for(
            tmp_path,
            target_with_wi1(kind="bug"),
            payload=source_payload(wi1_kind="chore"),
        )


def test_plan_refuses_a_target_project_that_is_still_linked(tmp_path: Path) -> None:
    target = target_with_wi1()
    target.projects["alpha"] = {
        "slug": "alpha",
        "name": "alpha",
        "root_path": "/srv/vogt/repos/alpha",
        "write_back": "full",
        "link_state": "linked",
    }
    with pytest.raises(Refused, match="write_back"):
        plan_for(tmp_path, target)


def test_plan_refuses_a_target_with_hidden_superseded_rows(tmp_path: Path) -> None:
    target = target_with_wi1()

    class Hidden(FakeTarget):
        def _status(self, arguments: JSON) -> JSON:
            return {"disk": {"path": "/srv/vogt/repos"}, "counts": {"work_items": 2}}

    hidden = Hidden(items=target.items, projects=target.projects)
    with pytest.raises(Refused, match="superseded"):
        plan_for(tmp_path, hidden)


def test_plan_refuses_a_non_contiguous_source(tmp_path: Path) -> None:
    payload = source_payload()
    payload["work_items"][2]["ref"] = "WI-9"
    with pytest.raises(ImportFailure, match="not contiguous"):
        plan_for(tmp_path, target_with_wi1(), payload=payload)


# ---------------------------------------------------------------------------
# Apply
# ---------------------------------------------------------------------------


def test_apply_creates_the_snapshot_in_ref_order_and_keeps_forge_off(
    tmp_path: Path,
) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()
    plan = plan_for(tmp_path, target)
    apply_plan(tmp_path, source_path, target, plan)

    assert list(target.items) == ["WI-1", "WI-2", "WI-3"]
    assert target.items["WI-1"]["title"] == "first prod item"
    assert target.items["WI-1"]["project_slug"] == "alpha"
    assert target.items["WI-2"]["state"] == "review"
    assert target.items["WI-3"]["state"] == "wont_do"
    assert "superseded by `gh:example/alpha#3`" in target.items["WI-3"]["body"]
    assert target.relations == {("WI-1", "relates_to", "WI-2")}
    assert [comment["kind"] for comment in target.comments] == ["note", "note"]
    assert all(project["write_back"] == "none" for project in target.projects.values())
    assert all(
        project["link_state"] == "unlinked" for project in target.projects.values()
    )


def test_apply_aborts_when_the_target_hands_back_a_different_ref(
    tmp_path: Path,
) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()

    original = target._work_create

    def skewed(arguments: JSON) -> JSON:
        result = original(arguments)
        target.items["WI-99"] = target.items.pop(result["item"]["ref"])
        target.items["WI-99"]["ref"] = "WI-99"
        result["item"]["ref"] = "WI-99"
        return result

    target._work_create = skewed  # type: ignore[method-assign]
    plan = plan_for(tmp_path, target)
    with pytest.raises(Refused, match="sequence preservation is broken"):
        apply_plan(tmp_path, source_path, target, plan)


def test_apply_resumes_after_a_failure_without_duplicating(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()
    plan = plan_for(tmp_path, target)
    target.fail_after = 4
    with pytest.raises(McpFailure):
        apply_plan(tmp_path, source_path, target, plan)
    assert len(target.items) < 3

    target.fail_after = None
    plan = plan_for(tmp_path, target)
    apply_plan(tmp_path, source_path, target, plan)
    assert list(target.items) == ["WI-1", "WI-2", "WI-3"]
    assert len(target.comments) == 2


def test_apply_falls_back_to_a_note_when_a_verdict_is_refused(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()
    plan = plan_for(tmp_path, target)
    apply_plan(tmp_path, source_path, target, plan)
    assert target.comments[1]["kind"] == "note"
    assert "verdict imported as a note" in target.comments[1]["body"]


def test_apply_never_writes_when_a_project_came_back_linked(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()
    plan = plan_for(tmp_path, target)
    original = target._project_register

    def sneaky(arguments: JSON) -> JSON:
        result = original(arguments)
        target.projects[result["project"]["slug"]]["write_back"] = "full"
        return result

    target._project_register = sneaky  # type: ignore[method-assign]
    with pytest.raises(Refused, match="refusing to write work"):
        apply_plan(tmp_path, source_path, target, plan)
    assert set(target.items) == {"WI-1"}


def test_limit_items_is_a_resumable_pilot(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1()
    pilot = plan_for(tmp_path, target, limit_items=1)
    assert [item.ref for item in pilot.items_new] == ["WI-2"]
    assert {item.ref for item in pilot.items_existing} == {"WI-1"}
    apply_plan(tmp_path, source_path, target, pilot)
    assert list(target.items) == ["WI-1", "WI-2"]

    rest = plan_for(tmp_path, target)
    apply_plan(tmp_path, source_path, target, rest)
    assert list(target.items) == ["WI-1", "WI-2", "WI-3"]
    assert [comment["body"].splitlines()[-1] for comment in target.comments] == [
        "a note",
        "a review",
    ]


def test_fresh_policy_never_touches_an_existing_wi1(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = target_with_wi1(kind="bug")
    plan = plan_for(
        tmp_path, target, ref_policy="fresh", payload=source_payload(wi1_kind="chore")
    )
    assert plan.ref_aligned is False
    apply_plan(tmp_path, source_path, target, plan)
    assert target.items["WI-1"]["title"] == "the dev smoke item"
    assert [ref for ref in target.items if ref != "WI-1"] == ["WI-2", "WI-3", "WI-4"]


# ---------------------------------------------------------------------------
# Remap mode (WI-4125, operator decision 2026-11-12)
# ---------------------------------------------------------------------------


def dev_target(count: int = 13) -> FakeTarget:
    items = {
        f"WI-{number}": {
            "ref": f"WI-{number}",
            "title": f"dev item {number}",
            "body": "",
            "kind": "chore",
            "state": "open",
            "priority": "p2",
            "project_slug": "vogt",
            "labels": [],
        }
        for number in range(1, count + 1)
    }
    return FakeTarget(
        items=items,
        projects={
            "vogt": {
                "slug": "vogt",
                "name": "vogt",
                "root_path": "/srv/vogt/repos/vogt",
                "write_back": "none",
                "link_state": "unlinked",
            }
        },
    )


def test_remap_assigns_new_numbers_after_the_target_max(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = dev_target()
    plan = plan_for(tmp_path, target, ref_mode="remap")

    assert plan.ref_mode == "remap"
    assert plan.ref_aligned is False
    assert plan.replace_wi1 is False
    assert [item.ref for item in plan.items_existing] == []
    assert [item.ref for item in plan.items_new] == ["WI-1", "WI-2", "WI-3"]
    assert plan.first_new_ref == 14
    assert plan.predicted_refs == {
        "WI-1": "WI-14",
        "WI-2": "WI-15",
        "WI-3": "WI-16",
    }

    apply_plan(tmp_path, source_path, target, plan)

    assert sorted(target.items, key=lambda ref: ref_number(ref) or 0) == [
        f"WI-{number}" for number in range(1, 17)
    ]
    assert target.items["WI-1"]["title"] == "dev item 1"
    assert target.items["WI-13"]["title"] == "dev item 13"
    assert target.items["WI-14"]["title"] == "first prod item"
    assert target.items["WI-14"]["body"].splitlines()[0] == "Imported from prod: WI-1"
    assert target.items["WI-15"]["body"].splitlines()[0] == "Imported from prod: WI-2"
    assert "prod-import" in target.items["WI-14"]["labels"]
    assert "prod-import" in target.labels
    assert target.items["WI-1"]["state"] == "open"
    assert target.items["WI-15"]["state"] == "review"
    assert target.items["WI-16"]["state"] == "wont_do"


def test_remap_relations_and_comments_follow_the_map(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = dev_target(2)
    plan = plan_for(tmp_path, target, ref_mode="remap")
    apply_plan(tmp_path, source_path, target, plan)

    assert target.relations == {("WI-3", "relates_to", "WI-4")}
    assert [comment["ref"] for comment in target.comments] == ["WI-3", "WI-4"]


def test_remap_markers_prevent_duplicates_when_resuming(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = dev_target(3)
    plan = plan_for(tmp_path, target, ref_mode="remap")
    apply_plan(tmp_path, source_path, target, plan)
    assert len(target.items) == 6

    (tmp_path / "map.json").unlink()
    resumed = plan_for(tmp_path, target, ref_mode="remap")
    assert [item.ref for item in resumed.items_new] == []
    assert {item.ref for item in resumed.items_existing} == {"WI-1", "WI-2", "WI-3"}

    apply_plan(tmp_path, source_path, target, resumed)
    assert len(target.items) == 6
    assert len(target.comments) == 2


def test_remap_suffixes_a_colliding_project_without_touching_devs(
    tmp_path: Path,
) -> None:
    source_path = write_source(tmp_path)
    target = dev_target(2)
    target.projects["alpha"] = {
        "slug": "alpha",
        "name": "alpha",
        "root_path": "/srv/vogt/repos/alpha",
        "write_back": "none",
        "link_state": "unlinked",
    }
    plan = plan_for(tmp_path, target, ref_mode="remap")

    assert plan.project_slug_map == {"alpha": "alpha-prod", "beta": "beta"}
    assert plan.project_collisions == [("alpha", "alpha-prod", "alpha prod")]
    apply_plan(tmp_path, source_path, target, plan)

    assert target.projects["alpha"]["name"] == "alpha"
    assert target.projects["alpha"]["root_path"] == "/srv/vogt/repos/alpha"
    assert target.projects["alpha-prod"]["name"] == "alpha prod"
    assert target.projects["alpha-prod"]["root_path"] == "/srv/vogt/repos/alpha-prod"
    assert target.items["WI-3"]["project_slug"] == "alpha-prod"
    assert target.items["WI-4"]["project_slug"] == "beta"


def test_remap_limit_items_is_a_resumable_pilot(tmp_path: Path) -> None:
    source_path = write_source(tmp_path)
    target = dev_target(2)
    pilot = plan_for(tmp_path, target, ref_mode="remap", limit_items=1)
    assert [item.ref for item in pilot.items_new] == ["WI-1"]
    apply_plan(tmp_path, source_path, target, pilot)
    assert [ref for ref in target.items if ref not in {"WI-1", "WI-2"}] == ["WI-3"]

    rest = plan_for(tmp_path, target, ref_mode="remap")
    assert [item.ref for item in rest.items_new] == ["WI-2", "WI-3"]
    apply_plan(tmp_path, source_path, target, rest)
    assert [ref for ref in target.items if ref not in {"WI-1", "WI-2"}] == [
        "WI-3",
        "WI-4",
        "WI-5",
    ]
    assert len(target.comments) == 2


def test_remap_still_refuses_the_source_instance_and_production_hosts(
    tmp_path: Path,
) -> None:
    source = load_source(write_source(tmp_path))
    with pytest.raises(Refused, match="instance"):
        read_target(
            FakeTarget(instance_id=source.instance_id),  # type: ignore[arg-type]
            source,
            url="http://127.0.0.1:8700",
            ref_mode="remap",
        )
    with pytest.raises(Refused, match="production"):
        read_target(
            FakeTarget(),  # type: ignore[arg-type]
            source,
            url="https://vogt-prod.example.net",
            ref_mode="remap",
        )


def test_strict_preserve_mode_still_refuses_a_diverged_target_item(
    tmp_path: Path,
) -> None:
    target = dev_target(2)
    target.items["WI-2"]["title"] = "something else entirely"
    with pytest.raises(Refused, match="refusing to overwrite unrelated work"):
        plan_for(tmp_path, target)
