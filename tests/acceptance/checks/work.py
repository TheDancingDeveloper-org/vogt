"""Group 4 — work tracking on a throwaway local-only project.

Every check here writes, so the group is skipped unless ``--allow-writes`` was
given. The project is registered by W1, used by W2-W9, and archived by the
run's cleanup (W10 asserts the cleanup semantics when it is reached).
"""

from __future__ import annotations

from typing import Any

from ..harness import (
    Case,
    Check,
    Context,
    ToolError,
    failed,
    on_run_cleanup,
    passed,
    skipped,
)


def _state(ctx: Context, key: str) -> Any:
    if key not in ctx.state:
        raise AssertionError(f"depends on an earlier case that did not run: {key}")
    return ctx.state[key]


def _item(payload: dict[str, Any]) -> dict[str, Any]:
    """The WorkItem from a `WorkResult`, which names it `item`.

    `work_item` is kept as a fallback so a shape that speaks the older name is
    still read rather than producing a false failure.
    """
    for key in ("item", "work_item"):
        value = payload.get(key)
        if isinstance(value, dict):
            return value
    return payload


def _cleanup_project(ctx: Context, slug: str, refs: list[str]) -> None:
    for ref in refs:
        try:
            ctx.client.tool(
                "work_transition",
                {"ref": ref, "to_state": "wont_do", "reason": f"{ctx.tag}: cleanup"},
            )
        except Exception:
            continue
    try:
        ctx.client.tool(
            "project_transition",
            {"slug": slug, "to_state": "archived", "reason": f"{ctx.tag}: cleanup"},
        )
        ctx.state[f"archived:{slug}"] = True
    except Exception:
        pass


def create_project(ctx: Context) -> Check:
    ctx.require_writes()
    engine = ctx.client.tool("engine_status")
    workspace = str(engine.get("workspace_root") or "")
    if not workspace:
        return skipped(
            "engine_status did not report a workspace_root; cannot place a "
            "throwaway project"
        )
    name = ctx.tag
    root = f"{workspace.rstrip('/')}/Active/{ctx.tag}"
    payload = ctx.client.tool(
        "project_create",
        {
            "name": name,
            "root_path": root,
            "lifecycle_state": "incubating",
            "reason": f"{ctx.tag}: throwaway project",
        },
    )
    project = (
        payload.get("project") if isinstance(payload.get("project"), dict) else payload
    )
    slug = str(project.get("slug") or "")
    if not slug:
        return failed(f"project_create returned no slug: {payload!r}")
    if slug in {"vogt", "ops"}:
        return failed(f"refusing to use the real project {slug!r} as the throwaway")
    ctx.state["work.project"] = slug
    ctx.state["work.refs"] = []
    on_run_cleanup(
        f"archive throwaway project {slug}",
        lambda: _cleanup_project(ctx, slug, list(ctx.state.get("work.refs") or [])),
    )
    return passed(f"created local-only project {slug} at {root}")


def create_item(ctx: Context) -> Check:
    ctx.require_writes()
    slug = _state(ctx, "work.project")
    label = f"acceptance-{ctx.tag}"
    ctx.client.tool(
        "label_create", {"name": label, "reason": f"{ctx.tag}: throwaway label"}
    )
    payload = ctx.client.tool(
        "work_create",
        {
            "kind": "bug",
            "title": f"acceptance item {ctx.tag}",
            "body": f"created by the acceptance suite ({ctx.tag})",
            "priority": "p1",
            "project": slug,
            "labels": [label],
            "local_only": True,
            "reason": f"{ctx.tag}: throwaway work item",
        },
    )
    item = _item(payload)
    ref = str(item.get("ref") or "")
    if not ref.startswith("WI-"):
        return failed(f"work_create returned no WI ref: {payload!r}")
    if item.get("state") != "open":
        return failed(f"new item starts in {item.get('state')!r}, not open")
    if item.get("priority") != "p1":
        return failed(f"new item has priority {item.get('priority')!r}, not p1")
    if label not in (item.get("labels") or []):
        return failed(
            f"new item does not carry label {label!r}: {item.get('labels')!r}"
        )
    if item.get("project") != slug:
        return failed(f"new item belongs to {item.get('project')!r}, not {slug!r}")
    ctx.state["work.item1"] = ref
    ctx.state.setdefault("work.refs", []).append(ref)
    return passed(f"created {ref} in {slug} (open, p1, label {label})")


def update_item(ctx: Context) -> Check:
    ctx.require_writes()
    ref = _state(ctx, "work.item1")
    payload = ctx.client.tool(
        "work_update",
        {
            "ref": ref,
            "priority": "p0",
            "title": f"acceptance item {ctx.tag} (updated)",
            "add_labels": [f"acceptance-{ctx.tag}"],
            "reason": f"{ctx.tag}: throwaway update",
        },
    )
    item = _item(payload)
    if item.get("ref") != ref:
        return failed(f"update changed the ref: {item.get('ref')!r} != {ref!r}")
    if item.get("priority") != "p0":
        return failed(f"priority did not persist: {item.get('priority')!r}")
    if "updated" not in str(item.get("title")):
        return failed(f"title did not persist: {item.get('title')!r}")
    return passed(f"{ref} updated to p0; ref preserved")


def comment_item(ctx: Context) -> Check:
    ctx.require_writes()
    ref = _state(ctx, "work.item1")
    body = f"acceptance comment {ctx.tag}"
    ctx.client.tool(
        "work_comment",
        {"ref": ref, "body": body, "reason": f"{ctx.tag}: throwaway comment"},
    )
    payload = ctx.client.tool("work_get", {"ref": ref})
    item = _item(payload)
    comments = payload.get("comments") or item.get("comments") or []
    if not any(
        isinstance(comment, dict) and comment.get("body") == body
        for comment in comments
    ):
        return failed(f"comment did not read back: {comments!r}")
    return passed(f"{ref} carries the comment {body!r}")


def transition_item(ctx: Context) -> Check:
    ctx.require_writes()
    ref = _state(ctx, "work.item1")
    payload = ctx.client.tool(
        "work_transition",
        {
            "ref": ref,
            "to_state": "in_progress",
            "reason": f"{ctx.tag}: throwaway transition",
        },
    )
    item = _item(payload)
    if item.get("state") != "in_progress":
        return failed(f"transition left state {item.get('state')!r}")
    if item.get("ref") != ref:
        return failed(f"transition changed the ref: {item.get('ref')!r}")
    return passed(f"{ref} moved open -> in_progress; ref preserved")


def walk_to_done(ctx: Context) -> Check:
    ctx.require_writes()
    ref = _state(ctx, "work.item1")
    payload = ctx.client.tool(
        "work_transition",
        {
            "ref": ref,
            "to_state": "done",
            "walk": True,
            "reason": f"{ctx.tag}: throwaway walk",
        },
    )
    item = _item(payload)
    if item.get("state") != "done":
        return failed(f"walk ended in {item.get('state')!r}, not done")
    return passed(f"{ref} walked to done")


def relate_items(ctx: Context) -> Check:
    ctx.require_writes()
    ref = _state(ctx, "work.item1")
    slug = _state(ctx, "work.project")
    payload = ctx.client.tool(
        "work_create",
        {
            "kind": "chore",
            "title": f"acceptance relation target {ctx.tag}",
            "project": slug,
            "local_only": True,
            "reason": f"{ctx.tag}: throwaway relation target",
        },
    )
    item = _item(payload)
    target = str(item.get("ref") or "")
    ctx.state["work.item2"] = target
    ctx.state.setdefault("work.refs", []).append(target)
    ctx.client.tool(
        "work_relate",
        {
            "ref": target,
            "kind": "depends_on",
            "target": ref,
            "reason": f"{ctx.tag}: throwaway relation",
        },
    )
    read = ctx.client.tool("work_get", {"ref": target})
    row = _item(read)
    relations = row.get("relations") or []
    if not any(
        isinstance(relation, dict)
        and relation.get("kind") == "depends_on"
        and relation.get("target") == ref
        for relation in relations
    ):
        return failed(f"relation did not read back: {relations!r}")
    return passed(f"{target} depends_on {ref}")


def search_items(ctx: Context) -> Check:
    ctx.require_writes()
    payload = ctx.client.tool("work_list", {"query": ctx.tag, "include_finished": True})
    items = payload.get("items") or payload.get("work_items") or []
    refs = {str(item.get("ref")) for item in items if isinstance(item, dict)}
    wanted = {_state(ctx, "work.item1"), _state(ctx, "work.item2")}
    missing = sorted(wanted - refs)
    if missing:
        return failed(
            f"work_list query {ctx.tag!r} did not return {missing}; "
            f"got {sorted(refs)[:8]}"
        )
    return passed(f"work_list query={ctx.tag!r} found {sorted(wanted)}")


def upstream_refused(ctx: Context) -> Check:
    ctx.require_writes()
    slug = _state(ctx, "work.project")
    before = len(
        ctx.client.tool("work_list", {"project": slug, "include_finished": True}).get(
            "items"
        )
        or []
    )
    try:
        ctx.client.tool(
            "work_create",
            {
                "kind": "chore",
                "title": f"acceptance upstream probe {ctx.tag}",
                "project": slug,
                "upstream": True,
                "reason": f"{ctx.tag}: prove an unlinked project refuses upstream",
            },
        )
    except ToolError as exc:
        text = str(exc).lower()
        if "link" not in text and "upstream" not in text:
            return failed(f"refusal did not name the link/upstream rule: {exc}")
        after = len(
            ctx.client.tool(
                "work_list", {"project": slug, "include_finished": True}
            ).get("items")
            or []
        )
        if after != before:
            return failed(
                f"a refused upstream create still wrote an item ({before} -> {after})"
            )
        return passed(f"upstream create on an unlinked project refused as data: {exc}")
    return failed("upstream=true on an unlinked project was accepted (WI-3987)")


def cleanup_semantics(ctx: Context) -> Check:
    ctx.require_writes()
    slug = _state(ctx, "work.project")
    refs = list(ctx.state.get("work.refs") or [])
    _cleanup_project(ctx, slug, refs)
    if not ctx.state.get(f"archived:{slug}"):
        return failed(f"project {slug} could not be archived")
    payload = ctx.client.tool("project_get", {"slug": slug})
    project = (
        payload.get("project") if isinstance(payload.get("project"), dict) else payload
    )
    if project.get("lifecycle_state") != "archived":
        return failed(
            f"project lifecycle is {project.get('lifecycle_state')!r}, not archived"
        )
    return passed(f"project {slug} archived and {len(refs)} items closed")


CASES = [
    Case(
        "W1",
        "work",
        "T3",
        "high",
        "create a throwaway local-only project",
        create_project,
    ),
    Case(
        "W2",
        "work",
        "T2",
        "high",
        "create a work item with priority and a label",
        create_item,
    ),
    Case(
        "W3",
        "work",
        "T2",
        "high",
        "update fields without changing the ref",
        update_item,
    ),
    Case(
        "W4", "work", "T2", "high", "comment on an item and read it back", comment_item
    ),
    Case("W5", "work", "T2", "high", "transition an item", transition_item),
    Case(
        "W6",
        "work",
        "T2",
        "high",
        "transition with walk reaches a terminal state",
        walk_to_done,
    ),
    Case("W7", "work", "T2", "medium", "relate two items", relate_items),
    Case("W8", "work", "T2", "medium", "search finds the items", search_items),
    Case(
        "W9",
        "work",
        "T4",
        "high",
        "upstream create on an unlinked project is refused",
        upstream_refused,
    ),
    Case(
        "W10",
        "work",
        "T3",
        "medium",
        "throwaway project can be archived",
        cleanup_semantics,
    ),
]
