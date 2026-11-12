#!/usr/bin/env python3
"""One-shot prod -> dev tracker snapshot importer (WI-4125).

This tool moves *tracker* data only: projects, initiatives, labels, work
items, their relations and comments.  It never moves sessions, tokens,
agent-auth material, forge credentials, observations, audit rows, events,
decisions, charters, watches, inbox rows or engine state.

The source is an export produced by the source instance's own `export`
operation (format 2).  That operation is the sanctioned read path for a
read-only instance; this tool never opens the source database.

Writing is opt-in twice: `--apply` *and* `--confirm`, plus an explicit
`--target-url` and `--token-file`.  The default is a dry-run plan that
writes nothing to the target.  Every apply is resumable through a state
file and starts with the target's own current state, so a second run does
not duplicate what the first one wrote.

Identity honesty
----------------
The target MCP surface mints its own `wrk_` ids and `WI-` refs and stamps
`created_at`/`updated_at` itself.  Work-item **ids and timestamps are
therefore not preservable through MCP**.  References *are* preserved when
the source refs are contiguous from WI-1: the tool updates an existing
WI-1 in place, then creates WI-2..N strictly in order and refuses to
continue if the target hands back a different ref.  `--ref-policy fresh`
turns that guard off.  See docs in tools/README.md.

Forge safety
------------
`project.register` creates an unlinked project written back by nothing.
Before writing any work item the tool re-reads every imported project and
refuses unless `write_back == "none"` and `link_state == "unlinked"`, so a
snapshot can never start commenting on a GitHub or Forgejo issue.
"""

from __future__ import annotations

import argparse
import contextlib
import functools
import hashlib
import json
import re
import sys
import time
import urllib.error
import urllib.request
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

JsonObject = dict[str, Any]

EXPORT_FORMAT_VERSION = 2
DEFAULT_REASON = "WI-4125: import the approved vogt-prod tracker snapshot into vogt-dev"
STATE_VERSION = 1
PAGE = 200
WORK_REF = re.compile(r"^WI-(\d+)$")

#: Hops `work_transition walk=true` takes from `open` to each state in the
#: default workflow. Every kind shares the graph (`workflow.list`).
TRANSITION_HOPS = {
    "open": 0,
    "in_progress": 1,
    "blocked": 1,
    "wont_do": 1,
    "review": 2,
    "done": 3,
}

#: Keys the tool consumes.  Anything else in the export is ignored and
#: reported, so a future export field cannot arrive here silently.
KNOWN_EXPORT_KEYS = frozenset(
    {
        "export_format_version",
        "instance_id",
        "exported_at",
        "revision",
        "scope",
        "clone_stamp",
        "projects",
        "work_items",
        "initiatives",
        "labels",
        "actors",
        "comments",
    }
)


class ImportFailure(RuntimeError):
    """The source or the plan is unusable; nothing has been written."""


class Refused(ImportFailure):
    """The target is not in a state this tool may modify."""


class McpFailure(ImportFailure):
    """A target MCP tool answered with an error."""

    def __init__(self, tool: str, code: str, message: str) -> None:
        super().__init__(f"{tool}: {code}: {message}")
        self.tool = tool
        self.code = code
        self.message = message


# ---------------------------------------------------------------------------
# JSON helpers: an export is untrusted data until it has been checked here.
# ---------------------------------------------------------------------------


def _text(value: object, where: str) -> str:
    if not isinstance(value, str):
        raise ImportFailure(f"{where} is not a string")
    return value


def _maybe_text(value: object, where: str) -> str | None:
    if value is None:
        return None
    return _text(value, where)


def _texts(value: object, where: str) -> tuple[str, ...]:
    if value is None:
        return ()
    if not isinstance(value, list):
        raise ImportFailure(f"{where} is not a list")
    return tuple(_text(entry, f"{where}[]") for entry in value)


def _object(value: object, where: str) -> JsonObject:
    if not isinstance(value, dict):
        raise ImportFailure(f"{where} is not an object")
    return value


def _objects(value: object, where: str) -> list[JsonObject]:
    if not isinstance(value, list):
        raise ImportFailure(f"{where} is not a list")
    return [_object(entry, f"{where}[]") for entry in value]


def slugify(name: str) -> str:
    """The core's slug rule (`engine/core/src/core/ids.rs`)."""
    out: list[str] = []
    previous_was_sep = False
    for char in name.strip().lower():
        if char.isalnum():
            out.append(char)
            previous_was_sep = False
        elif not previous_was_sep:
            out.append("-")
            previous_was_sep = True
    return "".join(out).strip("-")


def ref_number(ref: str) -> int | None:
    """The integer of a `WI-n` ref, or None for a subject-shaped ref."""
    match = WORK_REF.match(ref)
    return int(match.group(1)) if match else None


# ---------------------------------------------------------------------------
# Source model
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class Project:
    id: str
    slug: str
    name: str
    root_path: str
    repo_url: str | None
    lifecycle_state: str
    exclusions: tuple[str, ...]
    write_back: str
    link_state: str


@dataclass(frozen=True)
class Initiative:
    id: str
    slug: str
    title: str
    body: str
    state: str
    weight: int


@dataclass(frozen=True)
class Label:
    id: str
    name: str
    color: str | None


@dataclass(frozen=True)
class Relation:
    kind: str
    target_ref: str


@dataclass(frozen=True)
class WorkItem:
    id: str
    ref: str
    kind: str
    title: str
    body: str
    state: str
    priority: str
    effort: str | None
    project_slug: str | None
    initiative_slug: str | None
    assignee: str | None
    labels: tuple[str, ...]
    created_at: str
    updated_at: str
    origin: str
    superseded_by: str | None
    relations: tuple[Relation, ...]

    @property
    def seq(self) -> int:
        number = ref_number(self.ref)
        if number is None:
            raise ImportFailure(f"{self.ref}: a work item in the export is not WI-n")
        return number


@dataclass(frozen=True)
class Comment:
    id: str
    work_item_ref: str
    body: str
    kind: str
    created_at: str
    author: str
    handoff: JsonObject | None
    verdict: JsonObject | None
    verification: JsonObject | None


@dataclass(frozen=True)
class Source:
    path: Path
    instance_id: str
    exported_at: str
    projects: tuple[Project, ...]
    initiatives: tuple[Initiative, ...]
    labels: tuple[Label, ...]
    items: tuple[WorkItem, ...]
    comments: tuple[Comment, ...]
    actor_kind: Mapping[str, str]
    ignored_keys: tuple[str, ...]

    def project(self, slug: str) -> Project | None:
        return next((p for p in self.projects if p.slug == slug), None)

    def item_by_id(self, work_item_id: str) -> WorkItem | None:
        return next((i for i in self.items if i.id == work_item_id), None)


def load_source(path: Path) -> Source:
    raw: Any = json.loads(path.read_text(encoding="utf-8"))
    payload = _object(raw, "export")
    version = payload.get("export_format_version")
    if version != EXPORT_FORMAT_VERSION:
        raise ImportFailure(
            f"{path}: export format {version!r}, this tool reads "
            f"{EXPORT_FORMAT_VERSION}"
        )
    initiative_slug = {
        _text(entry.get("id"), "initiative.id"): _text(
            entry.get("slug"), "initiative.slug"
        )
        for entry in _objects(payload.get("initiatives"), "initiatives")
    }
    projects = tuple(
        _project(entry) for entry in _objects(payload.get("projects"), "projects")
    )
    items = tuple(
        _item(entry, initiative_slug)
        for entry in _objects(payload.get("work_items"), "work_items")
    )
    comments = tuple(
        _comment(entry, items)
        for entry in _objects(payload.get("comments"), "comments")
    )
    actor_kind = {
        _text(entry.get("identity_ref"), "actor.identity_ref"): _text(
            entry.get("kind"), "actor.kind"
        )
        for entry in _objects(payload.get("actors"), "actors")
    }
    return Source(
        path=path,
        instance_id=_text(payload.get("instance_id"), "instance_id"),
        exported_at=_text(payload.get("exported_at"), "exported_at"),
        projects=projects,
        initiatives=tuple(
            _initiative(entry)
            for entry in _objects(payload.get("initiatives"), "initiatives")
        ),
        labels=tuple(
            _label(entry) for entry in _objects(payload.get("labels"), "labels")
        ),
        items=items,
        comments=comments,
        actor_kind=actor_kind,
        ignored_keys=tuple(sorted(set(payload) - KNOWN_EXPORT_KEYS)),
    )


def _project(entry: JsonObject) -> Project:
    return Project(
        id=_text(entry.get("id"), "project.id"),
        slug=_text(entry.get("slug"), "project.slug"),
        name=_text(entry.get("name"), "project.name"),
        root_path=_text(entry.get("root_path"), "project.root_path"),
        repo_url=_maybe_text(entry.get("repo_url"), "project.repo_url"),
        lifecycle_state=_text(entry.get("lifecycle_state"), "project.lifecycle_state"),
        exclusions=_texts(entry.get("exclusions"), "project.exclusions"),
        write_back=_text(entry.get("write_back"), "project.write_back"),
        link_state=_text(entry.get("link_state"), "project.link_state"),
    )


def _initiative(entry: JsonObject) -> Initiative:
    weight = entry.get("weight")
    if not isinstance(weight, int):
        raise ImportFailure("initiative.weight is not an integer")
    return Initiative(
        id=_text(entry.get("id"), "initiative.id"),
        slug=_text(entry.get("slug"), "initiative.slug"),
        title=_text(entry.get("title"), "initiative.title"),
        body=_text(entry.get("body"), "initiative.body"),
        state=_text(entry.get("state"), "initiative.state"),
        weight=weight,
    )


def _label(entry: JsonObject) -> Label:
    return Label(
        id=_text(entry.get("id"), "label.id"),
        name=_text(entry.get("name"), "label.name"),
        color=_maybe_text(entry.get("color"), "label.color"),
    )


def _item(entry: JsonObject, initiative_slug: Mapping[str, str]) -> WorkItem:
    initiative_id = _maybe_text(entry.get("initiative_id"), "work_item.initiative_id")
    relations = tuple(
        Relation(
            kind=_text(rel.get("kind"), "relation.kind"),
            target_ref=_text(rel.get("related_ref"), "relation.related_ref"),
        )
        for rel in _objects(entry.get("relations"), "work_item.relations")
    )
    return WorkItem(
        id=_text(entry.get("id"), "work_item.id"),
        ref=_text(entry.get("ref"), "work_item.ref"),
        kind=_text(entry.get("kind"), "work_item.kind"),
        title=_text(entry.get("title"), "work_item.title"),
        body=_text(entry.get("body"), "work_item.body"),
        state=_text(entry.get("state"), "work_item.state"),
        priority=_text(entry.get("priority"), "work_item.priority"),
        effort=_maybe_text(entry.get("effort"), "work_item.effort"),
        project_slug=_maybe_text(entry.get("project_slug"), "work_item.project_slug"),
        initiative_slug=(
            initiative_slug.get(initiative_id) if initiative_id is not None else None
        ),
        assignee=_maybe_text(
            entry.get("assignee_identity_ref"), "work_item.assignee_identity_ref"
        ),
        labels=_texts(entry.get("labels"), "work_item.labels"),
        created_at=_text(entry.get("created_at"), "work_item.created_at"),
        updated_at=_text(entry.get("updated_at"), "work_item.updated_at"),
        origin=_text(entry.get("origin"), "work_item.origin"),
        superseded_by=_maybe_text(
            entry.get("superseded_by"), "work_item.superseded_by"
        ),
        relations=relations,
    )


def _comment(entry: JsonObject, items: Sequence[WorkItem]) -> Comment:
    work_item_id = _text(entry.get("work_item_id"), "comment.work_item_id")
    parent = next((item for item in items if item.id == work_item_id), None)
    if parent is None:
        raise ImportFailure(f"comment {entry.get('id')} names an unknown work item")
    handoff = entry.get("handoff")
    verdict = entry.get("verdict")
    verification = entry.get("verification")
    return Comment(
        id=_text(entry.get("id"), "comment.id"),
        work_item_ref=parent.ref,
        body=_text(entry.get("body"), "comment.body"),
        kind=_text(entry.get("kind"), "comment.kind"),
        created_at=_text(entry.get("created_at"), "comment.created_at"),
        author=_text(entry.get("actor_display_name"), "comment.actor_display_name"),
        handoff=None if handoff is None else _object(handoff, "comment.handoff"),
        verdict=None if verdict is None else _object(verdict, "comment.verdict"),
        verification=(
            None
            if verification is None
            else _object(verification, "comment.verification")
        ),
    )


# ---------------------------------------------------------------------------
# MCP transport
# ---------------------------------------------------------------------------


class McpClient:
    """The small part of streamable HTTP MCP this tool needs."""

    def __init__(
        self,
        base_url: str,
        token: str,
        *,
        timeout: float = 60.0,
        retries: int = 4,
        log: Callable[[str], None] | None = None,
    ) -> None:
        self._url = base_url.rstrip("/") + "/mcp"
        self._token = token
        self._timeout = timeout
        self._retries = retries
        self._log = log or (lambda _: None)
        self._session: str | None = None
        self._initialised = False
        self._next_id = 0
        self.calls = 0

    def tool(self, name: str, arguments: JsonObject | None = None) -> JsonObject:
        if not self._initialised:
            self._initialise()
        self._next_id += 1
        message: JsonObject = {
            "jsonrpc": "2.0",
            "id": self._next_id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments or {}},
        }
        response = self._rpc(message)
        result = response.get("result")
        if not isinstance(result, dict):
            raise McpFailure(name, "protocol", "tools/call returned no result object")
        if result.get("isError"):
            code, _, message_text = _content_text(result).partition(": ")
            raise McpFailure(name, code or "tool_error", message_text or "tool failed")
        text = _content_text(result)
        if not text:
            return {}
        parsed: Any = json.loads(text)
        return _object(parsed, f"{name} result")

    def _initialise(self) -> None:
        self._next_id += 1
        response = self._rpc(
            {
                "jsonrpc": "2.0",
                "id": self._next_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "vogt-snapshot-import", "version": "1"},
                },
            }
        )
        if "result" not in response:
            raise McpFailure("initialize", "protocol", "no result")
        with contextlib.suppress(ImportFailure):
            self._rpc(
                {
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {},
                }
            )
        self._initialised = True

    def _rpc(self, message: JsonObject) -> JsonObject:
        body = json.dumps(message).encode("utf-8")
        headers = {
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": f"Bearer {self._token}",
        }
        if self._session:
            headers["Mcp-Session-Id"] = self._session
        attempt = 0
        while True:
            attempt += 1
            request = urllib.request.Request(
                self._url, data=body, headers=headers, method="POST"
            )
            try:
                with urllib.request.urlopen(request, timeout=self._timeout) as response:
                    self._session = (
                        response.headers.get("Mcp-Session-Id") or self._session
                    )
                    payload = response.read().decode("utf-8")
                self.calls += 1
                return _parse_rpc(payload)
            except urllib.error.HTTPError as err:
                if err.code in (429, 500, 502, 503, 504) and attempt <= self._retries:
                    self._log(f"retrying after HTTP {err.code} (attempt {attempt})")
                    time.sleep(min(2**attempt * 0.25, 8.0))
                    continue
                detail = err.read().decode("utf-8", "replace")[:300]
                raise ImportFailure(f"target HTTP {err.code}: {detail}") from err
            except (urllib.error.URLError, TimeoutError) as err:
                if attempt <= self._retries:
                    self._log(f"retrying after a transport error (attempt {attempt})")
                    time.sleep(min(2**attempt * 0.25, 8.0))
                    continue
                raise ImportFailure(f"target is unreachable: {err}") from err


def _parse_rpc(payload: str) -> JsonObject:
    text = payload.strip()
    if not text:
        return {}
    if text.startswith("{"):
        parsed = json.loads(text)
        return _object(parsed, "rpc response")
    for line in reversed(text.splitlines()):
        if line.startswith("data:"):
            data = line[5:].strip()
            if data and data != "[DONE]":
                parsed = json.loads(data)
                return _object(parsed, "rpc response")
    raise ImportFailure("target answered neither JSON nor an SSE data frame")


def _content_text(result: Mapping[str, object]) -> str:
    content = result.get("content")
    if not isinstance(content, list):
        return "tool_error: no content"
    parts: list[str] = []
    for entry in content:
        if isinstance(entry, dict) and entry.get("type") == "text":
            parts.append(str(entry.get("text", "")))
    return "\n".join(parts)


# ---------------------------------------------------------------------------
# Target state
# ---------------------------------------------------------------------------


@dataclass
class TargetState:
    url: str
    import_root: str
    project_root: str | None
    projects: dict[str, JsonObject] = field(default_factory=dict)
    labels: set[str] = field(default_factory=set)
    initiatives: dict[str, JsonObject] = field(default_factory=dict)
    actors: dict[str, JsonObject] = field(default_factory=dict)
    items: dict[str, JsonObject] = field(default_factory=dict)
    declared_work_items: int = 0

    def ref_numbers(self) -> list[int]:
        numbers = sorted(
            number for ref in self.items if (number := ref_number(ref)) is not None
        )
        return numbers


def read_target(client: McpClient) -> TargetState:
    status = client.tool("status")
    disk = status.get("disk")
    import_root = ""
    if isinstance(disk, dict):
        path = disk.get("path")
        if isinstance(path, str):
            import_root = path
    if not import_root:
        data_dir = status.get("data_dir")
        if isinstance(data_dir, str):
            import_root = str(Path(data_dir) / "repos")
    counts = status.get("counts")
    declared = 0
    if isinstance(counts, dict) and isinstance(counts.get("work_items"), int):
        declared = counts["work_items"]
    state = TargetState(
        url="",
        import_root=import_root,
        project_root=import_root or None,
        declared_work_items=declared,
    )
    for row in _paged(client, "project_list", "projects"):
        state.projects[_text(row.get("slug"), "project.slug")] = row
    for name in _paged_names(client, "label_list", "labels", "name"):
        state.labels.add(name)
    for row in _paged(client, "initiative_list", "initiatives"):
        state.initiatives[_text(row.get("slug"), "initiative.slug")] = row
    for row in _paged(client, "actor_list", "actors"):
        state.actors[_text(row.get("identity_ref"), "actor.identity_ref")] = row
    for row in _paged(
        client,
        "work_list",
        "items",
        {"include_finished": True, "mode": "summary"},
    ):
        ref = row.get("ref")
        if isinstance(ref, str):
            state.items[ref] = row
    return state


def _paged(
    client: McpClient,
    tool: str,
    key: str,
    extra: JsonObject | None = None,
) -> list[JsonObject]:
    rows: list[JsonObject] = []
    offset = 0
    while True:
        arguments: JsonObject = {"limit": PAGE, "offset": offset}
        if extra:
            arguments.update(extra)
        result = client.tool(tool, arguments)
        page = result.get(key)
        if page is None:
            raise ImportFailure(f"{tool} answered without {key!r}")
        batch = _objects(page, f"{tool}.{key}")
        rows.extend(batch)
        total = result.get("total")
        offset += len(batch)
        if not batch or (isinstance(total, int) and offset >= total):
            return rows


def _paged_names(client: McpClient, tool: str, key: str, field: str) -> list[str]:
    return [
        _text(row.get(field), f"{key}.{field}") for row in _paged(client, tool, key)
    ]


# ---------------------------------------------------------------------------
# Plan
# ---------------------------------------------------------------------------


@dataclass
class Plan:
    source: Source
    target: TargetState
    project_root: str | None
    projects_new: list[Project] = field(default_factory=list)
    projects_existing: list[Project] = field(default_factory=list)
    write_back_forced: list[str] = field(default_factory=list)
    link_state_forced: list[str] = field(default_factory=list)
    labels_new: list[Label] = field(default_factory=list)
    initiatives_new: list[Initiative] = field(default_factory=list)
    actors_new: list[str] = field(default_factory=list)
    items_new: list[WorkItem] = field(default_factory=list)
    items_existing: list[WorkItem] = field(default_factory=list)
    superseded: list[WorkItem] = field(default_factory=list)
    relations: list[tuple[str, str, str]] = field(default_factory=list)
    comments: list[Comment] = field(default_factory=list)
    replace_wi1: bool = False
    first_new_ref: int = 1
    ref_aligned: bool = True
    warnings: list[str] = field(default_factory=list)

    @property
    def kept_refs(self) -> set[str]:
        return {item.ref for item in self.items_existing} | {
            item.ref for item in self.items_new
        }

    def estimated_calls(self, comments: bool = True) -> int:
        transitions = sum(
            1 if item.superseded_by is not None else TRANSITION_HOPS.get(item.state, 0)
            for item in self.items_new
        )
        existing = 0
        if self.replace_wi1:
            existing = 1 + TRANSITION_HOPS.get(self.source.items[0].state, 0)
        comment_calls = len(self.comments) if comments else 0
        return (
            existing
            + len(self.projects_new)
            + len(self.labels_new)
            + len(self.initiatives_new)
            + len(self.actors_new)
            + len(self.items_new)
            + transitions
            + len(self.relations)
            + comment_calls
        )

    def summary(self, *, latency_ms: int = 60, comments: bool = True) -> JsonObject:
        calls = self.estimated_calls(comments=comments)
        seconds = calls * latency_ms / 1000.0
        return {
            "source": {
                "path": str(self.source.path),
                "instance_id": self.source.instance_id,
                "exported_at": self.source.exported_at,
                "ignored_top_level_keys": list(self.source.ignored_keys),
            },
            "target": {
                "url": self.target.url,
                "project_root": self.project_root,
                "existing_work_items": self.target.declared_work_items,
            },
            "superseded_refs": [item.ref for item in self.superseded],
            "counts": {
                "projects": len(self.source.projects),
                "projects_new": len(self.projects_new),
                "projects_existing": len(self.projects_existing),
                "initiatives": len(self.source.initiatives),
                "initiatives_new": len(self.initiatives_new),
                "labels": len(self.source.labels),
                "labels_new": len(self.labels_new),
                "actors_new": len(self.actors_new),
                "work_items": len(self.source.items),
                "work_items_new": len(self.items_new),
                "superseded": len(self.superseded),
                "relations": len(self.relations),
                "comments": len(self.comments),
            },
            "forge_safety": {
                "source_write_back_non_none": list(self.write_back_forced),
                "source_link_state_linked": list(self.link_state_forced),
                "target_policy": (
                    "every imported project is re-read; apply refuses unless "
                    "write_back=none and link_state=unlinked"
                ),
            },
            "refs": {
                "policy": "sequence" if self.ref_aligned else "fresh",
                "replace_wi1": self.replace_wi1,
                "first_new_ref": self.first_new_ref,
                "last_ref": max((item.seq for item in self.source.items), default=0),
            },
            "estimate": {
                "mcp_calls": calls,
                "assumed_latency_ms": latency_ms,
                "seconds": round(seconds, 1),
            },
            "warnings": list(self.warnings),
        }


def build_plan(
    source: Source,
    target: TargetState,
    *,
    project_root: str | None,
    ref_policy: str,
    superseded_policy: str = "wont_do",
    limit_items: int | None = None,
    include_observed: bool = False,
) -> Plan:
    plan = Plan(source=source, target=target, project_root=project_root)
    _plan_projects(plan)
    _plan_entities(plan)
    _plan_items(
        plan,
        ref_policy=ref_policy,
        superseded_policy=superseded_policy,
        include_observed=include_observed,
    )
    if limit_items is not None:
        plan.items_new = plan.items_new[:limit_items]
        plan.warnings.append(
            f"--limit-items stopped after {len(plan.items_new)} new work items; "
            "relations and comments beyond them were left for the next run"
        )
    _plan_relations(plan)
    _plan_comments(plan)
    return plan


def _plan_projects(plan: Plan) -> None:
    for project in plan.source.projects:
        if slugify(project.name) != project.slug:
            raise ImportFailure(
                f"project {project.slug!r} would register as "
                f"{slugify(project.name)!r}; the target derives the slug from "
                "the name, so this project cannot keep its identity"
            )
        if project.write_back != "none":
            plan.write_back_forced.append(project.slug)
        if project.link_state != "unlinked":
            plan.link_state_forced.append(project.slug)
        existing = plan.target.projects.get(project.slug)
        if existing is None:
            plan.projects_new.append(project)
            continue
        plan.projects_existing.append(project)
        for name, wanted in (
            ("write_back", "none"),
            ("link_state", "unlinked"),
        ):
            actual = existing.get(name)
            if actual != wanted:
                raise Refused(
                    f"target project {project.slug!r} has {name}={actual!r}; "
                    f"this import only writes into an unlinked, write-back-none "
                    f"project (expected {wanted!r})"
                )
    if not plan.target.import_root:
        plan.warnings.append(
            "target status did not report an import root; pass --project-root "
            "explicitly in apply mode"
        )


def _plan_entities(plan: Plan) -> None:
    for label in plan.source.labels:
        if label.name not in plan.target.labels:
            plan.labels_new.append(label)
    for initiative in plan.source.initiatives:
        if slugify(initiative.title) != initiative.slug:
            raise ImportFailure(
                f"initiative {initiative.slug!r} would be created as "
                f"{slugify(initiative.title)!r}; the target derives the slug "
                "from the title"
            )
        if initiative.slug not in plan.target.initiatives:
            plan.initiatives_new.append(initiative)
    needed_assignees = {
        item.assignee for item in plan.source.items if item.assignee is not None
    }
    for identity in sorted(needed_assignees):
        if identity not in plan.target.actors:
            plan.actors_new.append(identity)


def _plan_items(
    plan: Plan,
    *,
    ref_policy: str,
    superseded_policy: str,
    include_observed: bool,
) -> None:
    items = [
        item
        for item in plan.source.items
        if include_observed or item.origin != "observed"
    ]
    if len(items) != len(plan.source.items):
        plan.warnings.append(
            f"ignored {len(plan.source.items) - len(items)} observed-only rows; "
            "an export from the declared store should not contain them"
        )
    assert all(item.origin != "observed" for item in items)
    items.sort(key=lambda item: item.seq)
    numbers = [item.seq for item in items]
    if numbers != list(range(1, len(numbers) + 1)):
        raise ImportFailure(
            "the export's WI refs are not contiguous from 1; identity-preserving "
            "import needs a contiguous sequence (use a database-level copy instead)"
        )
    target_numbers = plan.target.ref_numbers()
    if plan.target.declared_work_items != len(plan.target.items):
        raise Refused(
            "the target reports "
            f"{plan.target.declared_work_items} declared work items but lists "
            f"{len(plan.target.items)}; it may hold superseded rows this tool "
            "cannot see. Reset the target or copy the database instead."
        )
    if ref_policy == "sequence":
        if target_numbers and target_numbers != list(range(1, len(target_numbers) + 1)):
            raise Refused(
                "sequence ref policy needs the target's refs to be contiguous "
                f"WI-1..WI-{len(target_numbers)}; found {target_numbers[:5]}..."
            )
        if len(target_numbers) > len(items):
            raise Refused(
                f"the target already has {len(target_numbers)} items, more than "
                f"the source's {len(items)}"
            )
        for number in target_numbers:
            existing = plan.target.items.get(f"WI-{number}")
            source_item = items[number - 1]
            if existing is None:
                raise Refused(f"WI-{number} vanished from the target between reads")
            _require_same_item(existing, source_item, number, compare_title=number != 1)
        if target_numbers:
            first = items[0]
            if target_numbers[0] != 1:
                raise Refused(
                    "sequence ref policy needs WI-1 present or absent as a whole"
                )
            if first.kind != _text(
                plan.target.items["WI-1"].get("kind"), "target WI-1 kind"
            ):
                raise Refused(
                    "WI-1 on the target is kind "
                    f"{plan.target.items['WI-1'].get('kind')!r}, the source's is "
                    f"{first.kind!r}; sequence preservation needs the same kind "
                    "so WI-1 can be updated in place"
                )
            plan.replace_wi1 = True
            plan.first_new_ref = len(target_numbers) + 1
        else:
            plan.first_new_ref = 1
    else:
        plan.ref_aligned = False
        plan.first_new_ref = 0
    plan.items_existing = (
        items[: len(target_numbers)] if ref_policy == "sequence" else []
    )
    plan.items_new = items[len(target_numbers) :] if ref_policy == "sequence" else items
    plan.superseded = [item for item in items if item.superseded_by is not None]
    if superseded_policy == "skip" and plan.superseded:
        if ref_policy == "sequence":
            raise Refused(
                "--superseded-policy skip would remove WI refs from the sequence; "
                "sequence preservation needs the retired items created (default)"
            )
        skipped = {item.id for item in plan.superseded}
        plan.items_new = [item for item in plan.items_new if item.id not in skipped]
        plan.items_existing = [
            item for item in plan.items_existing if item.id not in skipped
        ]
        plan.warnings.append(
            f"--superseded-policy skip dropped {len(skipped)} retired items"
        )
    if plan.superseded:
        plan.warnings.append(
            f"{len(plan.superseded)} superseded items keep their WI number but show "
            "as wont_do on the target; their upstream successor subject is in the body"
        )


def _require_same_item(
    existing: JsonObject,
    source_item: WorkItem,
    number: int,
    *,
    compare_title: bool,
) -> None:
    if not compare_title:
        return
    title = existing.get("title")
    if title != source_item.title:
        raise Refused(
            f"WI-{number} on the target is {str(title)[:60]!r}, but the source's "
            f"is {source_item.title[:60]!r}; refusing to overwrite unrelated work"
        )


def _plan_relations(plan: Plan) -> None:
    known = plan.kept_refs
    for item in plan.source.items:
        for relation in item.relations:
            if relation.kind not in {
                "depends_on",
                "relates_to",
                "duplicate_of",
                "parent_of",
            }:
                plan.warnings.append(
                    f"skipped {item.ref} {relation.kind} {relation.target_ref}: "
                    "the MCP surface cannot declare that relation kind"
                )
                continue
            if relation.target_ref not in known:
                plan.warnings.append(
                    f"skipped {item.ref} {relation.kind} {relation.target_ref}: "
                    "the target work item is not in the export"
                )
                continue
            plan.relations.append((item.ref, relation.kind, relation.target_ref))


def _plan_comments(plan: Plan) -> None:
    known = plan.kept_refs
    for comment in plan.source.comments:
        if comment.work_item_ref not in known:
            plan.warnings.append(
                f"skipped comment {comment.id}: its work item is not in the export"
            )
            continue
        plan.comments.append(comment)


# ---------------------------------------------------------------------------
# State file
# ---------------------------------------------------------------------------


@dataclass
class State:
    path: Path
    data: JsonObject

    @classmethod
    def open(cls, path: Path, *, source: Source, target_url: str) -> State:
        if path.exists():
            raw: Any = json.loads(path.read_text(encoding="utf-8"))
            data = _object(raw, "state")
            if data.get("version") != STATE_VERSION:
                raise ImportFailure(f"{path}: unsupported state version")
            if data.get("source_instance_id") != source.instance_id:
                raise ImportFailure(
                    f"{path} belongs to source {data.get('source_instance_id')!r}, "
                    f"not {source.instance_id!r}"
                )
            if data.get("target") != target_url:
                raise ImportFailure(
                    f"{path} belongs to target {data.get('target')!r}, "
                    f"not {target_url!r}"
                )
            for key in (
                "items",
                "relations",
                "comments",
                "projects",
                "labels",
                "initiatives",
            ):
                data.setdefault(key, {})
            return cls(path=path, data=data)
        data = {
            "version": STATE_VERSION,
            "source_instance_id": source.instance_id,
            "target": target_url,
            "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
            "items": {},
            "relations": {},
            "comments": {},
            "projects": {},
            "labels": {},
            "initiatives": {},
        }
        return cls(path=path, data=data)

    def save(self) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.path.with_suffix(self.path.suffix + ".tmp")
        temporary.write_text(
            json.dumps(self.data, indent=1, sort_keys=True) + "\n", encoding="utf-8"
        )
        temporary.replace(self.path)

    def has_item(self, work_item_id: str) -> bool:
        items = self.data.get("items")
        return isinstance(items, dict) and work_item_id in items

    def record_item(self, work_item_id: str, ref: str, state: str) -> None:
        self.data["items"][work_item_id] = {"ref": ref, "state": state}

    def has_relation(self, key: str) -> bool:
        relations = self.data.get("relations")
        return isinstance(relations, dict) and key in relations

    def record_relation(self, key: str) -> None:
        self.data["relations"][key] = True

    def has_comment(self, comment_id: str) -> bool:
        comments = self.data.get("comments")
        return isinstance(comments, dict) and comment_id in comments

    def record_comment(self, comment_id: str) -> None:
        self.data["comments"][comment_id] = True


def default_state_path(source: Source, target_url: str) -> Path:
    digest = hashlib.sha256(f"{source.instance_id}\0{target_url}".encode()).hexdigest()[
        :16
    ]
    return Path.home() / ".local" / "state" / "vogt-snapshot-import" / f"{digest}.json"


# ---------------------------------------------------------------------------
# Apply
# ---------------------------------------------------------------------------


class Importer:
    def __init__(
        self,
        client: McpClient,
        plan: Plan,
        state: State,
        *,
        reason: str,
        superseded_policy: str,
        comment_provenance: bool,
        strict_comments: bool,
        project_root: str,
        log: Callable[[str], None],
    ) -> None:
        self._client = client
        self._plan = plan
        self._state = state
        self._reason = reason
        self._superseded_policy = superseded_policy
        self._comment_provenance = comment_provenance
        self._strict_comments = strict_comments
        self._project_root = project_root
        self._log = log
        self.written = 0
        self.failed_comments = 0

    def run(self) -> JsonObject:
        self._write_projects()
        self._check_forge_safety()
        self._write_labels()
        self._write_initiatives()
        self._write_actors()
        self._write_items()
        self._write_relations()
        self._write_comments()
        return {
            "items": len(self._plan.source.items),
            "written": self.written,
            "comments_fallback": self.failed_comments,
            "state": str(self._state.path),
        }

    def _write_projects(self) -> None:
        for project in self._plan.projects_new:
            root = str(Path(self._project_root) / project.slug)
            self._record_write(
                f"project {project.slug}",
                functools.partial(
                    self._client.tool,
                    "project_register",
                    {
                        "name": project.name,
                        "root_path": root,
                        "repo_url": project.repo_url,
                        "lifecycle_state": project.lifecycle_state,
                        "exclusions": list(project.exclusions) or None,
                        "reason": self._reason,
                    },
                ),
            )
            self._state.data["projects"][project.slug] = "created"
            self._state.save()

    def _check_forge_safety(self) -> None:
        for project in self._plan.source.projects:
            result = self._client.tool("project_get", {"slug": project.slug})
            row = _object(result.get("project"), f"project {project.slug}")
            if row.get("write_back") != "none" or row.get("link_state") != "unlinked":
                raise Refused(
                    f"project {project.slug!r} came back write_back="
                    f"{row.get('write_back')!r}, link_state={row.get('link_state')!r}; "
                    "refusing to write work that could comment upstream"
                )

    def _write_labels(self) -> None:
        for label in self._plan.labels_new:
            self._record_write(
                f"label {label.name}",
                functools.partial(
                    self._client.tool,
                    "label_create",
                    {"name": label.name, "color": label.color, "reason": self._reason},
                ),
            )
            self._state.data["labels"][label.name] = True
            self._state.save()

    def _write_initiatives(self) -> None:
        for initiative in self._plan.initiatives_new:
            self._record_write(
                f"initiative {initiative.slug}",
                functools.partial(
                    self._client.tool,
                    "initiative_create",
                    {
                        "title": initiative.title,
                        "body": initiative.body,
                        "state": initiative.state,
                        "weight": initiative.weight,
                        "reason": self._reason,
                    },
                ),
            )
            self._state.data["initiatives"][initiative.slug] = True
            self._state.save()

    def _write_actors(self) -> None:
        for identity in self._plan.actors_new:
            kind = self._plan.source.actor_kind.get(identity, "agent")
            self._record_write(
                f"actor {identity}",
                functools.partial(
                    self._client.tool,
                    "actor_create",
                    {
                        "identity_ref": identity,
                        "display_name": identity,
                        "kind": kind,
                        "reason": self._reason,
                    },
                ),
            )

    def _write_items(self) -> None:
        existing = sorted(self._plan.items_existing, key=lambda item: item.seq)
        new = sorted(self._plan.items_new, key=lambda item: item.seq)
        total = len(existing) + len(new)
        for index, item in enumerate(existing, start=1):
            if index % 50 == 0 or index == total:
                self._log(f"items {index}/{total}")
            self._sync_existing(item)
            self._state.save()
        for offset, item in enumerate(new, start=len(existing) + 1):
            if offset % 50 == 0 or offset == total:
                self._log(f"items {offset}/{total}")
            self._create_item(item)
            self._state.save()

    def _sync_existing(self, item: WorkItem) -> None:
        if (
            item.seq == 1
            and self._plan.replace_wi1
            and not self._state.has_item(item.id)
        ):
            self._update_wi1(item)
            return
        if self._state.has_item(item.id):
            entry = _object(self._state.data["items"][item.id], "state item")
            ref = _text(entry.get("ref"), "state item.ref")
        else:
            ref = item.ref
        desired = "wont_do" if item.superseded_by is not None else item.state
        current = self._current_state(ref, item)
        if current != desired:
            self._transition(ref, desired)
        entry = _object(self._state.data["items"].setdefault(item.id, {}), "state item")
        entry["ref"] = ref
        entry["state"] = desired

    def _current_state(self, ref: str, item: WorkItem) -> str:
        if self._state.has_item(item.id):
            entry = _object(self._state.data["items"][item.id], "state item")
            recorded = entry.get("state")
            if isinstance(recorded, str):
                return recorded
        existing = self._plan.target.items.get(ref)
        if existing is not None:
            state = existing.get("state")
            if isinstance(state, str):
                return state
        return "open"

    def _create_item(self, item: WorkItem) -> None:
        body = item.body
        superseded = (
            item.superseded_by is not None and self._superseded_policy != "skip"
        )
        if superseded:
            body = (
                f"{body}\n\n---\n_Imported from the prod snapshot as superseded "
                f"by `{item.superseded_by}`; vogt-dev has no superseded link._"
            )
        result = self._client.tool(
            "work_create",
            self._create_arguments(item, body),
        )
        returned = _object(result.get("item") or result, "work_create")
        ref = _text(returned.get("ref"), "work_create.ref")
        if self._plan.ref_aligned and ref != item.ref:
            raise Refused(
                f"{item.ref} was created as {ref}; sequence preservation is "
                "broken. Stop, inspect the target, and resume only after the "
                "refs line up."
            )
        self.written += 1
        target_state = "wont_do" if superseded else item.state
        self._state.record_item(item.id, ref, "created")
        if target_state != "open":
            self._transition(ref, target_state)
        self._state.data["items"][item.id]["state"] = target_state

    def _create_arguments(self, item: WorkItem, body: str) -> JsonObject:
        arguments: JsonObject = {
            "kind": item.kind,
            "title": item.title,
            "body": body,
            "priority": item.priority,
            "local_only": True,
            "reason": self._reason,
            "mode": "compact",
        }
        if item.project_slug is not None:
            arguments["project"] = item.project_slug
        if item.labels:
            arguments["labels"] = list(item.labels)
        if item.initiative_slug is not None:
            arguments["initiative"] = item.initiative_slug
        if item.effort is not None:
            arguments["effort"] = item.effort
        if item.assignee is not None:
            arguments["assignee"] = item.assignee
        return arguments

    def _update_wi1(self, item: WorkItem) -> None:
        existing = self._plan.target.items.get("WI-1")
        if existing is None:
            raise Refused("WI-1 disappeared from the target after planning")
        arguments: JsonObject = {
            "ref": "WI-1",
            "title": item.title,
            "body": item.body,
            "priority": item.priority,
            "reason": self._reason,
            "mode": "compact",
        }
        if item.project_slug is not None:
            arguments["project"] = item.project_slug
        for key, wanted in (
            ("initiative", item.initiative_slug),
            ("effort", item.effort),
            ("assignee", item.assignee),
        ):
            if wanted is None:
                arguments[f"clear_{key}"] = True
            else:
                arguments[key] = wanted
        existing_labels = set(_texts(existing.get("labels"), "target WI-1 labels"))
        add = sorted(set(item.labels) - existing_labels)
        remove = sorted(existing_labels - set(item.labels))
        if add:
            arguments["add_labels"] = add
        if remove:
            arguments["remove_labels"] = remove
        self._record_write(
            "WI-1 update", lambda: self._client.tool("work_update", arguments)
        )
        self._state.record_item(item.id, "WI-1", "updated")
        if item.state != "open":
            self._transition("WI-1", item.state)
        self._state.data["items"][item.id]["state"] = item.state

    def _transition(self, ref: str, state: str) -> None:
        self._record_write(
            f"{ref} -> {state}",
            functools.partial(
                self._client.tool,
                "work_transition",
                {
                    "ref": ref,
                    "to_state": state,
                    "walk": True,
                    "reason": self._reason,
                    "mode": "compact",
                },
            ),
        )

    def _write_relations(self) -> None:
        for source_ref, kind, target_ref in self._plan.relations:
            key = f"{source_ref}|{kind}|{target_ref}"
            if self._state.has_relation(key):
                continue
            try:
                self._record_write(
                    f"{source_ref} {kind} {target_ref}",
                    functools.partial(
                        self._client.tool,
                        "work_relate",
                        {
                            "ref": source_ref,
                            "kind": kind,
                            "target": target_ref,
                            "reason": self._reason,
                        },
                    ),
                )
            except McpFailure as err:
                if err.code != "conflict":
                    raise
                self._log(f"{source_ref} {kind} {target_ref} already present")
            self._state.record_relation(key)
            self._state.save()

    def _write_comments(self) -> None:
        total = len(self._plan.comments)
        for index, comment in enumerate(self._plan.comments, 1):
            if index % 100 == 0 or index == total:
                self._log(f"comments {index}/{total}")
            if self._state.has_comment(comment.id):
                continue
            self._post_comment(comment)
            self._state.record_comment(comment.id)
            self._state.save()

    def _post_comment(self, comment: Comment) -> None:
        body = comment.body
        if self._comment_provenance:
            body = (
                f"_prod comment by {comment.author} at {comment.created_at}_\n\n{body}"
            )
        arguments: JsonObject = {
            "ref": comment.work_item_ref,
            "body": body,
            "kind": comment.kind,
            "reason": self._reason,
            "mode": "compact",
        }
        if comment.kind == "handoff" and comment.handoff is not None:
            arguments["handoff"] = comment.handoff
        elif comment.kind == "verdict" and comment.verdict is not None:
            arguments["verdict"] = _strip(
                comment.verdict, {"reviewer_session", "reviewer_started_by"}
            )
        elif comment.kind == "verification" and comment.verification is not None:
            arguments["verification"] = _strip(comment.verification, {"by"})
        try:
            self._record_write(
                f"comment {comment.id}",
                lambda: self._client.tool("work_comment", arguments),
            )
        except McpFailure as err:
            if self._strict_comments:
                raise
            self.failed_comments += 1
            self._log(f"comment {comment.id} fell back to a note: {err}")
            fallback = dict(arguments)
            fallback["kind"] = "note"
            fallback.pop("handoff", None)
            fallback.pop("verdict", None)
            fallback.pop("verification", None)
            fallback["body"] = (
                f"> {comment.kind} imported as a note ({err.code}).\n\n{body}"
            )
            self._client.tool("work_comment", fallback)

    def _record_write(self, what: str, call: Callable[[], JsonObject]) -> JsonObject:
        result = call()
        self.written += 1
        if self.written % 500 == 0:
            self._log(f"wrote {self.written} records ({what})")
        return result


def _strip(payload: JsonObject, names: set[str]) -> JsonObject:
    return {key: value for key, value in payload.items() if key not in names}


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Import a vogt-prod tracker export into a target Vogt instance. "
            "Dry-run unless --apply and --confirm are both given."
        )
    )
    parser.add_argument(
        "--source", required=True, type=Path, help="export format 2 JSON"
    )
    parser.add_argument(
        "--target-url", help="target engine base URL, e.g. https://vogt-dev.sprooty.com"
    )
    parser.add_argument(
        "--token-file", type=Path, help="file holding the target bearer token"
    )
    parser.add_argument("--apply", action="store_true", help="write to the target")
    parser.add_argument("--confirm", action="store_true", help="required with --apply")
    parser.add_argument(
        "--ref-policy", choices=("sequence", "fresh"), default="sequence"
    )
    parser.add_argument(
        "--superseded-policy",
        choices=("wont_do", "skip"),
        default="wont_do",
        help="wont_do keeps the WI sequence intact (default); skip breaks it",
    )
    parser.add_argument(
        "--project-root", help="target directory for registered projects"
    )
    parser.add_argument(
        "--state-file", type=Path, help="resume state (default per target)"
    )
    parser.add_argument("--reason", default=DEFAULT_REASON)
    parser.add_argument("--plan-out", type=Path, help="write the plan/result JSON here")
    parser.add_argument(
        "--limit-items",
        type=int,
        help="stop after this many new work items (a resumable pilot)",
    )
    parser.add_argument("--skip-comments", action="store_true")
    parser.add_argument("--skip-relations", action="store_true")
    parser.add_argument("--no-comment-provenance", action="store_true")
    parser.add_argument("--strict-comments", action="store_true")
    parser.add_argument("--latency-ms", type=int, default=60)
    parser.add_argument("--json", action="store_true", help="print JSON only")
    return parser


def _log(message: str) -> None:
    print(message, file=sys.stderr, flush=True)


def main(argv: Sequence[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    source = load_source(args.source)
    if args.apply:
        if not args.confirm:
            _log("--apply needs --confirm: this run would write to the target")
            return 2
        if not args.target_url or not args.token_file:
            _log("--apply needs an explicit --target-url and --token-file")
            return 2
    token = ""
    if args.target_url and args.token_file:
        token = args.token_file.read_text(encoding="utf-8").strip()
        if not token:
            _log(f"{args.token_file} is empty")
            return 2
    client: McpClient | None = None
    target = TargetState(url=args.target_url or "", import_root="", project_root=None)
    if args.target_url:
        client = McpClient(args.target_url, token, log=_log)
        try:
            target = read_target(client)
            target.url = args.target_url
        except ImportFailure as err:
            _log(f"reading the target failed: {err}")
            return 1
    project_root = args.project_root or target.project_root
    if project_root is None and args.apply:
        _log("apply needs --project-root (the target did not report one)")
        return 2
    try:
        plan = build_plan(
            source,
            target,
            project_root=project_root,
            ref_policy=args.ref_policy,
            superseded_policy=args.superseded_policy,
            limit_items=args.limit_items,
        )
    except (ImportFailure, Refused) as err:
        _log(f"refusing to plan: {err}")
        return 1
    if args.skip_relations:
        plan.relations = []
    if args.skip_comments:
        plan.comments = []
    summary = plan.summary(latency_ms=args.latency_ms, comments=not args.skip_comments)
    if args.plan_out:
        args.plan_out.parent.mkdir(parents=True, exist_ok=True)
        args.plan_out.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    if not args.apply:
        if args.json:
            print(json.dumps(summary, indent=2))
        else:
            _print_plan(summary)
        return 0
    assert client is not None
    assert project_root is not None
    state_path = args.state_file or default_state_path(source, args.target_url or "")
    try:
        state = State.open(state_path, source=source, target_url=args.target_url or "")
    except ImportFailure as err:
        _log(str(err))
        return 1
    importer = Importer(
        client,
        plan,
        state,
        reason=args.reason,
        superseded_policy=args.superseded_policy,
        comment_provenance=not args.no_comment_provenance,
        strict_comments=args.strict_comments,
        project_root=project_root,
        log=_log,
    )
    try:
        result = importer.run()
    except (ImportFailure, Refused) as err:
        _log(f"stopped: {err}")
        _log(f"state is in {state_path}; fix the cause and resume the same command")
        return 1
    summary["result"] = result
    if args.json:
        print(json.dumps(summary, indent=2))
    else:
        print(
            f"wrote {result['written']} records to {args.target_url}; "
            f"comments fell back to notes: {result['comments_fallback']}; "
            f"state: {state_path}"
        )
    return 0


def _print_plan(summary: JsonObject) -> None:
    counts = _object(summary.get("counts"), "counts")
    refs = _object(summary.get("refs"), "refs")
    estimate = _object(summary.get("estimate"), "estimate")
    print("vogt snapshot import — dry run (nothing written)")
    print(f"  source: {summary['source']['path']} ({summary['source']['instance_id']})")
    print(f"  target: {summary['target']['url'] or 'not given'}")
    print(
        "  projects: {projects} ({projects_new} new, "
        "{projects_existing} existing)".format(**counts)
    )
    print(
        "  initiatives: {initiatives} ({initiatives_new} new); labels: {labels} "
        "({labels_new} new)".format(**counts)
    )
    print(
        "  work items: {work_items} ({work_items_new} to create, {superseded} "
        "superseded); relations: {relations}; comments: {comments}".format(**counts)
    )
    print(
        "  refs: {policy}, first new WI-{first_new_ref}, WI-1 replaced: {replace_wi1}, "
        "last WI-{last_ref}".format(**refs)
    )
    print(
        "  estimate: {mcp_calls} MCP calls, about {seconds}s at "
        "{assumed_latency_ms}ms".format(**estimate)
    )
    warnings = summary.get("warnings")
    if warnings:
        print(f"  warnings: {len(warnings)}")
        for warning in warnings[:40]:
            print(f"    - {warning}")
        if len(warnings) > 40:
            print(f"    ... and {len(warnings) - 40} more")


if __name__ == "__main__":
    sys.exit(main())
