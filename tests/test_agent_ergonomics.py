"""WI-847: the work tools are shaped so an agent's first call succeeds.

Every alias, the walk, the allowed-edge refusal, summary mode and the
`writable` flag are pinned here — aliases widen the accepted input, and an
undocumented, untested alias is one that drifts.
"""

from __future__ import annotations

import json
from typing import Any

import pytest

from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    CreateWorkParams,
    GetWorkParams,
    ListProjectsParams,
    ListWorkParams,
    ProjectBriefParams,
    RegisterProjectParams,
    TransitionWorkParams,
    WorkItemRow,
)
from vogt.application.services import (
    backlog,
    brief_project,
    create_work,
    get_work,
    list_projects,
    list_work,
    register_project,
    transition_work,
)
from vogt.core.entities import WorkItem
from vogt.core.workflow import TransitionRejected, default_workflow
from vogt.errors import InvalidParams, NotLinked
from vogt.registry import default_registry

from tests.conftest import mark_linked, native_work_item

WHY = "agent ergonomics test"


def _item(ctx: AppContext, title: str = "A thing", body: str = "") -> str:
    return create_work(
        ctx, CreateWorkParams(kind="feature", title=title, body=body, reason=WHY)
    ).item.ref


def _surface(ctx: AppContext) -> McpSurface:
    return McpSurface(context_factory=lambda: ctx)


# -- aliases ----------------------------------------------------------------


def test_work_get_accepts_id_for_ref(instance: AppContext) -> None:
    ref = _item(instance)
    params = GetWorkParams.model_validate({"id": ref})
    assert params.ref == ref
    answer = _surface(instance).call_tool("work_get", {"id": ref})
    assert answer["item"]["ref"] == ref


def test_naming_an_alias_and_its_field_is_refused(instance: AppContext) -> None:
    with pytest.raises(InvalidParams, match="alias of 'ref'"):
        _surface(instance).call_tool("work_get", {"id": "WI-1", "ref": "WI-2"})


@pytest.mark.parametrize("alias", ["status", "state", "states"])
@pytest.mark.parametrize(
    ("value", "expected"),
    [
        ("in_progress", ["in_progress"]),
        ("open, in_progress", ["open", "in_progress"]),
        (["review"], ["review"]),
    ],
)
def test_work_list_state_aliases(
    alias: str, value: object, expected: list[str]
) -> None:
    assert ListWorkParams.model_validate({alias: value}).states == expected


@pytest.mark.parametrize("alias", ["query", "text", "search", "q"])
def test_work_list_query_aliases_filter_title_body_and_ref(
    instance: AppContext, alias: str
) -> None:
    wanted = _item(instance, "Fix the flux capacitor")
    by_body = _item(instance, "Unrelated title", body="the FLUX is wrong")
    _item(instance, "Something else entirely")

    answer = _surface(instance).call_tool("work_list", {alias: "flux"})
    assert {row["ref"] for row in answer["items"]} == {wanted, by_body}
    assert answer["total"] == 2

    by_ref = list_work(instance, ListWorkParams(query=wanted.lower()))
    assert [row.ref for row in by_ref.items] == [wanted]


def test_the_query_is_literal_text_not_a_pattern(instance: AppContext) -> None:
    _item(instance, "plain words")
    _item(instance, "100% done")
    answer = list_work(instance, ListWorkParams(query="%"))
    assert [row.title for row in answer.items] == ["100% done"]


def test_naming_a_finished_state_includes_finished_items(instance: AppContext) -> None:
    ref = _item(instance)
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="done", walk=True, reason=WHY)
    )
    answer = list_work(instance, ListWorkParams(states=["done"]))
    assert [row.ref for row in answer.items] == [ref]


def test_work_transition_accepts_id_and_to(instance: AppContext) -> None:
    ref = _item(instance)
    answer = _surface(instance).call_tool(
        "work_transition", {"id": ref, "to": "in_progress", "reason": WHY}
    )
    assert answer["item"]["state"] == "in_progress"


def test_project_brief_accepts_project_for_slug(
    instance: AppContext, tmp_path: Any
) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Alpha", root_path=str(tmp_path), reason=WHY),
    )
    assert ProjectBriefParams.model_validate({"project": "alpha"}).slug == "alpha"
    answer = _surface(instance).call_tool("project_brief", {"project": "alpha"})
    assert answer["project"]["slug"] == "alpha"


# -- errors that say what to change -----------------------------------------


def test_a_missing_reason_is_told_exactly_what_is_wanted(instance: AppContext) -> None:
    ref = _item(instance)
    with pytest.raises(InvalidParams) as caught:
        _surface(instance).call_tool(
            "work_transition", {"ref": ref, "to_state": "in_progress"}
        )
    message = str(caught.value)
    assert caught.value.code == "invalid_params"
    assert "missing required parameter 'reason'" in message
    assert "audited" in message
    assert "required: ref, to_state, reason" in message


def test_an_unknown_parameter_lists_the_valid_ones(instance: AppContext) -> None:
    with pytest.raises(InvalidParams) as caught:
        _surface(instance).call_tool("work_list", {"colour": "blue"})
    message = str(caught.value)
    assert "unknown parameter 'colour'" in message
    assert "optional:" in message and "states" in message and "mode" in message


def test_the_registry_still_refuses_a_defaulted_reason() -> None:
    """The audited-write rule stands: aliases never made reason optional."""
    for operation in default_registry():
        if operation.mutating:
            assert operation.params_model.model_fields["reason"].is_required()


# -- transitions --------------------------------------------------------------


def test_a_refused_edge_lists_the_allowed_edges_and_the_path(
    instance: AppContext,
) -> None:
    ref = _item(instance)
    with pytest.raises(TransitionRejected) as caught:
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state="done", reason=WHY)
        )
    message = str(caught.value)
    assert "allowed from open: in_progress, blocked, wont_do" in message
    assert "open -> in_progress -> review -> done" in message
    assert "walk=true" in message


def test_shortest_path_never_passes_through_a_finished_state() -> None:
    workflow = default_workflow("bug")
    assert workflow.shortest_path("open", "done") == [
        "open",
        "in_progress",
        "review",
        "done",
    ]
    # done -> open -> ... is fine (done is the start); via wont_do is not.
    assert workflow.shortest_path("blocked", "done") == [
        "blocked",
        "in_progress",
        "review",
        "done",
    ]
    assert workflow.shortest_path("done", "in_progress") == [
        "done",
        "open",
        "in_progress",
    ]
    assert workflow.shortest_path("open", "nowhere") is None


def test_walk_takes_each_edge_as_an_audited_transition(instance: AppContext) -> None:
    ref = _item(instance)
    answer = transition_work(
        instance,
        TransitionWorkParams(ref=ref, to_state="done", walk=True, reason=WHY),
    )
    assert answer.item.state == "done"
    assert answer.walked == ["open", "in_progress", "review", "done"]

    with instance.declared.read() as view:
        rows = [
            row
            for row in view.list_audit(limit=50, offset=0)
            if row.operation == "work.transition"
        ]
    reasons = sorted(row.reason for row in rows)
    assert reasons == sorted(
        [
            f"{WHY} (walk 1/3: open -> in_progress)",
            f"{WHY} (walk 2/3: in_progress -> review)",
            f"{WHY} (walk 3/3: review -> done)",
        ]
    )


def test_walk_over_one_edge_is_an_ordinary_transition(instance: AppContext) -> None:
    ref = _item(instance)
    answer = transition_work(
        instance,
        TransitionWorkParams(ref=ref, to_state="in_progress", walk=True, reason=WHY),
    )
    assert answer.item.state == "in_progress"
    assert answer.walked == []


def test_walk_refuses_a_blocked_completion_before_moving(instance: AppContext) -> None:
    from vogt.application.models import RelateWorkParams
    from vogt.application.services import relate_work

    ref = _item(instance, "dependent")
    blocker = _item(instance, "blocker")
    relate_work(
        instance,
        RelateWorkParams(ref=ref, kind="depends_on", target=blocker, reason=WHY),
    )
    with pytest.raises(TransitionRejected, match="blocked_by_dependency"):
        transition_work(
            instance,
            TransitionWorkParams(ref=ref, to_state="done", walk=True, reason=WHY),
        )
    assert get_work(instance, GetWorkParams(ref=ref)).item.state == "open"


def test_walk_to_the_current_state_is_still_a_no_op(instance: AppContext) -> None:
    ref = _item(instance)
    with pytest.raises(TransitionRejected, match="no_op"):
        transition_work(
            instance,
            TransitionWorkParams(ref=ref, to_state="open", walk=True, reason=WHY),
        )


# -- summary mode and paging ------------------------------------------------------


def test_work_list_defaults_to_compact_rows(instance: AppContext) -> None:
    _item(instance, body="a long body " * 50)
    answer = list_work(instance, ListWorkParams())
    assert answer.mode == "summary"
    assert all(isinstance(row, WorkItemRow) for row in answer.items)
    full = list_work(instance, ListWorkParams(mode="full"))
    assert all(isinstance(row, WorkItem) for row in full.items)
    assert full.items[0].body.startswith("a long body")  # type: ignore[union-attr]


def test_two_hundred_summary_rows_fit_one_tool_result(instance: AppContext) -> None:
    for number in range(200):
        _item(
            instance,
            f"Work item number {number} with a realistic title length",
            body="Problem statement and evidence. " * 40,
        )
    answer = _surface(instance).call_tool("work_list", {"limit": 200})
    assert len(answer["items"]) == 200
    assert set(answer["items"][0]) == {
        "ref",
        "title",
        "kind",
        "state",
        "priority",
        "project_slug",
    }
    size = len(json.dumps(answer))
    assert size < 40_000, size
    full = _surface(instance).call_tool("work_list", {"limit": 200, "mode": "full"})
    assert len(json.dumps(full)) > 5 * size


def test_work_list_pages_with_next_offset(instance: AppContext) -> None:
    for number in range(5):
        _item(instance, f"item {number}")
    first = list_work(instance, ListWorkParams(limit=2))
    assert first.total == 5 and first.next_offset == 2
    last = list_work(instance, ListWorkParams(limit=2, offset=4))
    assert len(last.items) == 1 and last.next_offset is None


def test_backlog_and_brief_summary_drop_the_embedded_item(
    instance: AppContext, tmp_path: Any
) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Alpha", root_path=str(tmp_path), reason=WHY),
    )
    mark_linked(instance, "alpha")
    for number in range(3):
        native_work_item(instance, title=f"ranked {number}", project="alpha")

    summary = backlog(instance, BacklogParams(limit=2))
    assert summary.items and all(row.item is None for row in summary.items)
    assert summary.next_offset == 2
    full = backlog(instance, BacklogParams(limit=2, mode="full"))
    assert all(row.item is not None for row in full.items)

    brief = brief_project(instance, ProjectBriefParams(slug="alpha"))
    assert brief.top_backlog and all(row.item is None for row in brief.top_backlog)
    brief_full = brief_project(instance, ProjectBriefParams(slug="alpha", mode="full"))
    assert all(row.item is not None for row in brief_full.top_backlog)


# -- can I create here? -------------------------------------------------------


def test_project_list_says_which_projects_accept_a_create(
    instance: AppContext, tmp_path: Any
) -> None:
    (tmp_path / "a").mkdir()
    (tmp_path / "b").mkdir()
    register_project(
        instance,
        RegisterProjectParams(name="Local", root_path=str(tmp_path / "a"), reason=WHY),
    )
    register_project(
        instance,
        RegisterProjectParams(
            name="Linked",
            root_path=str(tmp_path / "b"),
            repo_url="https://github.com/example/linked",
            reason=WHY,
        ),
    )
    mark_linked(instance, "linked")

    listed = {p.slug: p for p in list_projects(instance, ListProjectsParams()).projects}
    assert listed["local"].writable is False
    assert "project_not_linked" in listed["local"].writable_reason
    assert "local_only" in listed["local"].writable_reason
    # Linked, but the default write-back policy (`none`) does not permit
    # create — exactly what `work.create` would refuse with.
    assert listed["linked"].writable is False
    assert "upstream_write_refused" in listed["linked"].writable_reason
    assert "forge writeback" in listed["linked"].writable_reason

    with pytest.raises(NotLinked):
        create_work(
            instance,
            CreateWorkParams(kind="feature", title="x", project="local", reason=WHY),
        )
