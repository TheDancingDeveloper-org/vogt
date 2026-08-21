"""Upstream-truth work items and the local overlay (#181).

The #178 pivot's decisions 1, 2, 9 and 10, pinned as behaviour:

1. On a *linked* project the work items are the upstream issues; the subject
   key is the item's identity on every surface.
2. The overlay is invisible upstream: a vogt-only change produces **zero**
   provider calls, asserted against a transport that records every request.
9. Write-through is synchronous and fail-loud: a provider failure or a
   policy refusal raises a typed error and nothing local commits.
10. On an *unlinked* project the write verbs refuse with the typed
   link-or-publish error.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BacklogParams,
    BoardCellParams,
    BoardListParams,
    CommentParams,
    CreateLabelParams,
    CreateWorkParams,
    ForgeLinkParams,
    GetWorkParams,
    ImportProjectParams,
    InitParams,
    ListWorkParams,
    OnboardParams,
    RegisterProjectParams,
    SetWriteBackParams,
    TransitionWorkParams,
    UpdateWorkParams,
    WriteBackListParams,
)
from vogt.application.services import (
    backlog,
    comment_work,
    create_label,
    create_work,
    get_work,
    import_project,
    init_instance,
    link_project,
    list_board,
    list_work,
    list_write_backs,
    onboard,
    register_project,
    relate_work,
    set_write_back,
    transition_work,
    update_work,
)
from vogt.application.services.work import WORK_CREATE
from vogt.errors import (
    InvalidRequest,
    LinkRefused,
    NotLinked,
    UpstreamWriteFailed,
    UpstreamWriteRefused,
)

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock, native_work_item

WHY = "upstream-truth test"
REPO = "https://github.com/acme/demo"
SUBJECT_1 = "gh:acme/demo#1"
SUBJECT_2 = "gh:acme/demo#2"


class RecordingForge:
    """A stand-in forge that mints issues and records every request.

    Recording *every* call — reads included — is what lets the decision-2
    invariant be an assertion rather than a hope: an overlay-only write must
    leave `requests` untouched, not merely mutation-free.
    """

    def __init__(self, *, fail_mutations: bool = False) -> None:
        self.fail_mutations = fail_mutations
        self.issues: list[dict[str, Any]] = []
        self.requests: list[tuple[str, str]] = []
        self.bodies: list[dict[str, Any]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        del headers
        self.requests.append((method, url))
        payload = json.loads(body.decode()) if body else {}
        if body:
            self.bodies.append(payload)
        if method != "GET" and self.fail_mutations:
            return 500, b"upstream said no"
        if method == "POST" and url.rstrip("/").endswith("/issues"):
            number = len(self.issues) + 1
            issue = {
                "number": number,
                "title": payload.get("title", ""),
                "state": "open",
                "labels": [{"name": str(n)} for n in payload.get("labels", [])],
                "updated_at": f"2026-08-01T00:00:{number:02d}Z",
                "html_url": f"{REPO}/issues/{number}",
            }
            self.issues.append(issue)
            return 200, json.dumps(issue).encode()
        if method == "POST" and "/comments" in url:
            return 200, json.dumps({"html_url": f"{REPO}/issues/1"}).encode()
        if method == "POST" and "/labels" in url:
            return 200, json.dumps({"number": 1}).encode()
        if method == "PATCH" and "/issues/" in url:
            number = int(url.rstrip("/").rsplit("/", 1)[1])
            for issue in self.issues:
                if issue["number"] == number:
                    issue["state"] = payload.get("state", issue["state"])
            return 200, json.dumps({"number": number}).encode()
        if method == "GET" and "/issues" in url and "/comments" not in url:
            return 200, json.dumps(self.issues).encode()
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode()
        if "/contents/" in url:
            return 404, b""
        if "/vulnerability-alerts" in url or "/automated-security-fixes" in url:
            return 404, b""
        return 200, b"[]"

    @property
    def mutations(self) -> list[tuple[str, str]]:
        return [(m, u) for m, u in self.requests if m != "GET"]


def _instance(tmp_path: Path, forge: RecordingForge) -> AppContext:
    from vogt.config import VogtConfig

    token_file = tmp_path / "github_token"
    token_file.write_text("ghp_file_token", encoding="utf-8")
    ctx = build_context(
        config=VogtConfig(
            data_dir=tmp_path / "instance",
            import_root=tmp_path / "imported",
            sqlite_synchronous="off",
            github_token_file=token_file,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        forge_transport=forge,
    )
    init_instance(ctx, InitParams())
    return ctx


@pytest.fixture
def forge() -> RecordingForge:
    return RecordingForge()


@pytest.fixture
def linked(tmp_path: Path, forge: RecordingForge) -> AppContext:
    """A linked project `demo` with write-back armed, plus an unlinked one."""
    ctx = _instance(tmp_path, forge)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    register_project(
        ctx,
        RegisterProjectParams(name="Folder", root_path="/srv/folder", reason=WHY),
    )
    forge.requests.clear()
    forge.bodies.clear()
    return ctx


def _linked_project(ctx: AppContext) -> Any:
    with ctx.declared.read() as view:
        return view.project_by_slug("demo")


# -- acceptance 1: create writes through, keyed by the subject -------------


def test_create_on_linked_opens_the_issue_and_keys_the_overlay(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_label(linked, CreateLabelParams(name="bug-ish", reason=WHY))
    result = create_work(
        linked,
        CreateWorkParams(
            kind="bug",
            title="Retries forever",
            body="The loop never backs off.",
            priority="p1",
            project="demo",
            labels=["bug-ish"],
            reason=WHY,
        ),
    )

    assert result.item.ref == SUBJECT_1, "the subject key IS the ref"
    assert result.item.id == SUBJECT_1
    assert [m for m, _ in forge.mutations] == ["POST"], "one issue create"
    assert forge.bodies[0]["title"] == "Retries forever"
    assert forge.bodies[0]["labels"] == ["bug-ish"], (
        "labels are shared vocabulary and travel upstream at create"
    )

    with linked.declared.read() as view:
        overlay = view.work_overlay(SUBJECT_1)
    assert overlay is not None, "the keyed overlay row exists"
    assert overlay.priority == "p1"
    assert overlay.workflow_state == "open"

    ledger = list_write_backs(linked, WriteBackListParams()).actions
    assert [a.action for a in ledger] == ["create"]
    assert ledger[0].outcome == "succeeded"
    assert ledger[0].subject_key == SUBJECT_1


def test_the_mirror_joined_to_the_overlay_is_the_item(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_work(
        linked,
        CreateWorkParams(kind="bug", title="Mirrored", project="demo", reason=WHY),
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))

    fetched = get_work(linked, GetWorkParams(ref=SUBJECT_1))
    assert fetched.item.ref == SUBJECT_1
    assert fetched.item.title == "Mirrored"
    assert fetched.item.state == "open"
    assert fetched.item.project_slug == "demo"
    assert fetched.comments == [], "the discussion lives upstream"

    listed = list_work(linked, ListWorkParams(project="demo"))
    assert [item.ref for item in listed.items] == [SUBJECT_1]
    assert listed.total == 1


# -- acceptance 2: overlay-only changes send nothing -----------------------


def test_vogt_only_changes_produce_zero_provider_calls(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_work(
        linked,
        CreateWorkParams(kind="bug", title="Quiet", project="demo", reason=WHY),
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    forge.requests.clear()
    forge.bodies.clear()

    update_work(
        linked,
        UpdateWorkParams(ref=SUBJECT_1, priority="p0", effort="s", reason=WHY),
    )
    transition_work(
        linked,
        TransitionWorkParams(ref=SUBJECT_1, to_state="in_progress", reason=WHY),
    )

    assert forge.requests == [], (
        "priority, effort and a vogt-only state move are overlay-only "
        f"(decision 2); the forge saw {forge.requests}"
    )
    with linked.declared.read() as view:
        overlay = view.work_overlay(SUBJECT_1)
    assert overlay is not None
    assert overlay.priority == "p0"
    assert overlay.effort == "s"
    assert overlay.workflow_state == "in_progress"
    assert get_work(linked, GetWorkParams(ref=SUBJECT_1)).item.state == "in_progress"


# -- acceptance 3: fail loud, commit nothing -------------------------------


def test_a_failed_create_raises_and_commits_nothing(tmp_path: Path) -> None:
    forge = RecordingForge(fail_mutations=True)
    ctx = _instance(tmp_path, forge)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))

    with pytest.raises(UpstreamWriteFailed):
        create_work(
            ctx,
            CreateWorkParams(kind="bug", title="Lost", project="demo", reason=WHY),
        )

    with ctx.declared.read() as view:
        assert view.work_overlays([SUBJECT_1]) == {}, "no overlay row"
        operations = [r.operation for r in view.list_audit(limit=100)]
    assert WORK_CREATE not in operations, (
        "a create the forge refused must not be on the record as one that ran"
    )
    assert list_work(ctx, ListWorkParams(project="demo")).items == []


def test_a_policy_refusal_is_loud_not_a_silent_local_half(
    tmp_path: Path, forge: RecordingForge
) -> None:
    ctx = _instance(tmp_path, forge)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    forge.requests.clear()

    with pytest.raises(UpstreamWriteRefused, match="none"):
        create_work(
            ctx,
            CreateWorkParams(kind="bug", title="Refused", project="demo", reason=WHY),
        )
    assert forge.mutations == [], "refused before anything was sent"


def test_a_failed_comment_and_close_raise_and_change_nothing(
    tmp_path: Path,
) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    create_work(
        ctx, CreateWorkParams(kind="bug", title="Flaky", project="demo", reason=WHY)
    )
    onboard(ctx, OnboardParams(project="demo", reason=WHY))
    forge.fail_mutations = True

    with pytest.raises(UpstreamWriteFailed):
        comment_work(ctx, CommentParams(ref=SUBJECT_1, body="lost", reason=WHY))
    with pytest.raises(UpstreamWriteFailed):
        transition_work(
            ctx, TransitionWorkParams(ref=SUBJECT_1, to_state="wont_do", reason=WHY)
        )
    assert get_work(ctx, GetWorkParams(ref=SUBJECT_1)).item.state == "open", (
        "the refused close did not move the item"
    )


# -- acceptance 4: the unlinked refusal ------------------------------------


def test_create_on_an_unlinked_project_is_the_typed_refusal(
    linked: AppContext,
) -> None:
    with pytest.raises(NotLinked) as refused:
        create_work(
            linked,
            CreateWorkParams(kind="bug", title="Nope", project="folder", reason=WHY),
        )
    message = str(refused.value)
    assert "forge link" in message and "publish" in message, (
        "the refusal names both ways forward"
    )


def test_the_other_write_verbs_refuse_on_an_unlinked_project(
    linked: AppContext,
) -> None:
    item = native_work_item(linked, kind="bug", title="Stuck", project="folder")
    with pytest.raises(NotLinked):
        comment_work(linked, CommentParams(ref=item.ref, body="hi", reason=WHY))
    with pytest.raises(NotLinked):
        transition_work(
            linked,
            TransitionWorkParams(ref=item.ref, to_state="in_progress", reason=WHY),
        )
    create_label(linked, CreateLabelParams(name="tag", reason=WHY))
    with pytest.raises(NotLinked):
        update_work(
            linked, UpdateWorkParams(ref=item.ref, add_labels=["tag"], reason=WHY)
        )
    # A local field edit is not one of decision 10's named verbs and keeps
    # working — the surface withdrawal is #183's, not this refusal's.
    updated = update_work(
        linked, UpdateWorkParams(ref=item.ref, priority="p0", reason=WHY)
    )
    assert updated.item.priority == "p0"


def test_projectless_items_are_untouched_by_the_refusal(
    linked: AppContext,
) -> None:
    created = create_work(
        linked, CreateWorkParams(kind="chore", title="Loose", reason=WHY)
    )
    assert created.item.ref.startswith("WI-")
    moved = transition_work(
        linked,
        TransitionWorkParams(ref=created.item.ref, to_state="in_progress", reason=WHY),
    )
    assert moved.item.state == "in_progress"


# -- acceptance 5: Board and Backlog show each issue exactly once ----------


def test_board_and_backlog_count_each_upstream_issue_once(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_work(
        linked, CreateWorkParams(kind="bug", title="First", project="demo", reason=WHY)
    )
    create_work(
        linked,
        CreateWorkParams(kind="feature", title="Second", project="demo", reason=WHY),
    )
    # A third issue that exists only upstream — filed on the forge, never
    # through vogt, so it has no overlay row and must still appear once.
    forge.issues.append(
        {
            "number": 3,
            "title": "Filed on the forge",
            "state": "open",
            "labels": [],
            "updated_at": "2026-08-01T00:00:03Z",
            "html_url": f"{REPO}/issues/3",
        }
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))

    ranked = backlog(linked, BacklogParams(project="demo", limit=50))
    refs = [entry.ref for entry in ranked.items]
    assert sorted(refs) == [SUBJECT_1, SUBJECT_2, "gh:acme/demo#3"]
    assert len(set(refs)) == len(refs), "each upstream issue exactly once"

    board = list_board(
        linked,
        BoardListParams(
            project="demo",
            lane_mode="none",
            cells=[BoardCellParams(lane_key="", state="open")],
            page_size=10,
        ),
    )
    cell = board.cells[0]
    assert sorted(item.ref for item in cell.items) == sorted(refs)
    assert cell.total == 3
    assert board.total == 3


def test_the_board_pages_upstream_items_with_stable_cursors(
    linked: AppContext, forge: RecordingForge
) -> None:
    for index in range(3):
        create_work(
            linked,
            CreateWorkParams(
                kind="feature", title=f"issue {index}", project="demo", reason=WHY
            ),
        )
    onboard(linked, OnboardParams(project="demo", reason=WHY))

    request = BoardListParams(
        project="demo",
        lane_mode="none",
        cells=[BoardCellParams(lane_key="", state="open")],
        page_size=2,
    )
    first = list_board(linked, request)
    cell = first.cells[0]
    assert cell.total == 3
    assert len(cell.items) == 2
    assert cell.next_cursor is not None
    second = list_board(
        linked,
        BoardListParams(
            project="demo",
            lane_mode="none",
            cells=[BoardCellParams(lane_key="", state="open", cursor=cell.next_cursor)],
            page_size=2,
            snapshot=first.snapshot,
        ),
    ).cells[0]
    refs = [item.ref for item in cell.items + second.items]
    assert len(refs) == 3 and len(set(refs)) == 3
    assert second.next_cursor is None


# -- write-through of the remaining verbs ----------------------------------


def test_closing_and_reopening_write_through(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_work(
        linked,
        CreateWorkParams(kind="bug", title="Done soon", project="demo", reason=WHY),
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    forge.requests.clear()
    forge.bodies.clear()

    closed = transition_work(
        linked, TransitionWorkParams(ref=SUBJECT_1, to_state="wont_do", reason=WHY)
    )
    assert closed.item.state == "wont_do"
    assert [m for m, _ in forge.mutations] == ["PATCH"]
    assert forge.bodies[-1] == {"state": "closed"}

    reopened = transition_work(
        linked, TransitionWorkParams(ref=SUBJECT_1, to_state="open", reason=WHY)
    )
    assert reopened.item.state == "open"
    assert forge.bodies[-1] == {"state": "open"}


def test_a_comment_posts_upstream_and_stores_no_local_row(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_work(
        linked, CreateWorkParams(kind="bug", title="Talky", project="demo", reason=WHY)
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    forge.requests.clear()
    forge.bodies.clear()

    result = comment_work(
        linked, CommentParams(ref=SUBJECT_1, body="Looking now", reason=WHY)
    )
    assert result.write_back == "succeeded"
    assert [m for m, _ in forge.mutations] == ["POST"]
    assert forge.bodies[-1]["body"] == "Looking now"
    assert get_work(linked, GetWorkParams(ref=SUBJECT_1)).comments == [], (
        "the forge thread is the comment's home (FR-B5)"
    )


def test_label_adds_write_through_and_removals_are_refused(
    linked: AppContext, forge: RecordingForge
) -> None:
    create_label(linked, CreateLabelParams(name="shared", reason=WHY))
    create_work(
        linked, CreateWorkParams(kind="bug", title="Tagged", project="demo", reason=WHY)
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    forge.requests.clear()
    forge.bodies.clear()

    updated = update_work(
        linked, UpdateWorkParams(ref=SUBJECT_1, add_labels=["shared"], reason=WHY)
    )
    assert "shared" in updated.item.labels
    assert [m for m, _ in forge.mutations] == ["POST"]
    assert forge.bodies[-1] == {"labels": ["shared"]}

    with pytest.raises(InvalidRequest, match="append-only"):
        update_work(
            linked,
            UpdateWorkParams(ref=SUBJECT_1, remove_labels=["shared"], reason=WHY),
        )


def test_title_and_body_edits_are_refused_as_upstream_truth(
    linked: AppContext,
) -> None:
    create_work(
        linked, CreateWorkParams(kind="bug", title="Owned", project="demo", reason=WHY)
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    with pytest.raises(InvalidRequest, match="upstream truth"):
        update_work(linked, UpdateWorkParams(ref=SUBJECT_1, title="New", reason=WHY))
    with pytest.raises(InvalidRequest, match="upstream truth"):
        update_work(linked, UpdateWorkParams(ref=SUBJECT_1, body="New", reason=WHY))


def test_relations_between_upstream_items_are_refused_with_the_reason(
    linked: AppContext,
) -> None:
    from vogt.application.models import RelateWorkParams

    create_work(
        linked, CreateWorkParams(kind="bug", title="A", project="demo", reason=WHY)
    )
    create_work(
        linked, CreateWorkParams(kind="bug", title="B", project="demo", reason=WHY)
    )
    onboard(linked, OnboardParams(project="demo", reason=WHY))
    with pytest.raises(InvalidRequest, match="upstream-truth"):
        relate_work(
            linked,
            RelateWorkParams(
                ref=SUBJECT_1, kind="depends_on", target=SUBJECT_2, reason=WHY
            ),
        )


# -- forge.link and its preconditions --------------------------------------


def test_link_refuses_without_a_repo_url(linked: AppContext) -> None:
    with pytest.raises(LinkRefused, match="no repository URL"):
        link_project(linked, ForgeLinkParams(project="folder", reason=WHY))
    with linked.declared.read() as view:
        project = view.project_by_slug("folder")
    assert project is not None and project.link_state == "unlinked"


def test_link_refuses_without_a_usable_credential(tmp_path: Path) -> None:
    from vogt.config import VogtConfig

    ctx = build_context(
        config=VogtConfig(data_dir=tmp_path / "instance", sqlite_synchronous="off"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )
    init_instance(ctx, InitParams())
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    with pytest.raises(LinkRefused, match="credential"):
        link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))


def test_link_is_an_audited_explicit_act(linked: AppContext) -> None:
    project = _linked_project(linked)
    assert project is not None and project.link_state == "linked"
    with linked.declared.read() as view:
        operations = [r.operation for r in view.list_audit(limit=100)]
    assert "forge.link" in operations


def test_import_with_consolidation_links_the_project(
    tmp_path: Path, forge: RecordingForge
) -> None:
    from vogt.adapters.git import CloneOutcome, CloneRequest

    ctx = _instance(tmp_path, forge)

    def cloner(request: CloneRequest) -> CloneOutcome:
        destination = Path(str(request.destination))
        destination.mkdir(parents=True, exist_ok=True)
        return CloneOutcome(
            destination=destination, revision="0" * 40, default_branch="main"
        )

    import dataclasses

    ctx = dataclasses.replace(ctx, cloner=cloner)
    imported = import_project(
        ctx, ImportProjectParams(repo="acme/imported", reason=WHY)
    )
    assert imported.project.link_state == "linked", (
        "a successful clone+consolidate is one of the explicit linking acts"
    )
    skipped = import_project(
        ctx,
        ImportProjectParams(repo="acme/other", consolidate=False, reason=WHY),
    )
    assert skipped.project.link_state == "unlinked"
