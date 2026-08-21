"""Native-item migration on link/publish (#183, design §4, decision 7).

The acceptance box, pinned:

- linking (or publishing) a project with N open native items creates N
  upstream issues, re-keyed to their subject keys and overlaid with the
  vogt-only fields — none silently dropped;
- closed native items stay historical, untouched;
- a mid-migration provider failure fails loud, names which items migrated
  and which are still native, and re-linking resumes exactly there;
- the retired rows are excluded from every surface (each issue counted
  once) while their refs still resolve, and **no `work_links` row** is
  written for the new subject — the #181 dedup reads one as "this declared
  row IS the item", which a retired row is not.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BacklogParams,
    CreateActorParams,
    CreateLabelParams,
    ForgeLinkParams,
    ForgePublishParams,
    GetWorkParams,
    InitParams,
    ListWorkParams,
    OnboardParams,
    RegisterProjectParams,
    SetWriteBackParams,
    WriteBackListParams,
)
from vogt.application.services import (
    backlog,
    create_actor,
    create_label,
    get_work,
    init_instance,
    link_project,
    list_work,
    list_write_backs,
    onboard,
    publish_project,
    register_project,
    set_write_back,
)
from vogt.errors import UpstreamWriteFailed, UpstreamWriteRefused

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock, native_work_item
from tests.test_forge_publish import PublishForge, RecordingPusher, _seed_repo
from tests.test_upstream_truth import RecordingForge

WHY = "native migration test"
REPO = "https://github.com/acme/demo"


class FlakyForge(RecordingForge):
    """Succeeds for `allow` mutations, then fails — the partial-failure case."""

    def __init__(self, *, allow: int) -> None:
        super().__init__()
        self.allow = allow
        self._mutations_seen = 0

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if method != "GET":
            self._mutations_seen += 1
            if self._mutations_seen > self.allow:
                self.requests.append((method, url))
                return 500, b"upstream said no"
        return super().__call__(url, headers, body, method)


def _instance(tmp_path: Path, forge: Any) -> AppContext:
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


def _project_with_native_items(ctx: AppContext) -> None:
    """`demo`, unlinked, policy full, holding two open items and a closed one."""
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path="/srv/demo", repo_url=REPO, reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    create_label(ctx, CreateLabelParams(name="carried", reason=WHY))
    create_actor(
        ctx,
        CreateActorParams(
            identity_ref="agent:mig", display_name="Migrator", reason=WHY
        ),
    )
    native_work_item(
        ctx,
        kind="bug",
        title="Oldest first",
        body="The prose travels too.",
        project="demo",
        priority="p1",
        effort="s",
        assignee="agent:mig",
        labels=("carried",),
    )
    native_work_item(
        ctx,
        kind="feature",
        title="In flight",
        project="demo",
        state="in_progress",
    )
    native_work_item(
        ctx,
        kind="chore",
        title="Long done",
        project="demo",
        state="done",
    )


# -- acceptance: N open items -> N issues, re-keyed and overlaid ------------


def test_link_migrates_every_open_native_item(tmp_path: Path) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)

    result = link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))

    assert [(m.ref, m.subject_key) for m in result.migrated] == [
        ("WI-1", "gh:acme/demo#1"),
        ("WI-2", "gh:acme/demo#2"),
    ], "two open items, two issues, oldest first — and only the open ones"

    issue_posts = [b for b in forge.bodies if "title" in b]
    assert [p["title"] for p in issue_posts] == ["Oldest first", "In flight"]
    assert issue_posts[0]["labels"] == ["carried"], "labels ride along"
    assert "migrated from Vogt as WI-1" in issue_posts[0]["body"], (
        "the issue names its provenance"
    )

    with ctx.declared.read() as view:
        first = view.work_overlay("gh:acme/demo#1")
        second = view.work_overlay("gh:acme/demo#2")
        wi1 = view.work_item_by_ref("WI-1")
        actor = view.actor_by_identity("agent:mig")
    assert first is not None and second is not None
    assert first.priority == "p1" and first.effort == "s"
    assert actor is not None and first.assignee_actor_id == actor.id
    assert first.workflow_state == "open"
    assert second.workflow_state == "in_progress", (
        "the richer-than-open/closed state folds into the overlay"
    )
    assert wi1 is not None and wi1.superseded_by == "gh:acme/demo#1", (
        "the native row is retired with the marker, not deleted"
    )

    ledger = list_write_backs(ctx, WriteBackListParams()).actions
    creates = [a for a in ledger if a.action == "create"]
    assert sorted(a.subject_key or "" for a in creates) == [
        "gh:acme/demo#1",
        "gh:acme/demo#2",
    ], "one FR-B2 ledger row per migrated item"
    assert all("migrated from" in (a.detail or "") for a in creates)


def test_each_issue_is_counted_exactly_once_after_migration(
    tmp_path: Path,
) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    onboard(ctx, OnboardParams(project="demo", reason=WHY))

    listed = list_work(ctx, ListWorkParams(project="demo"))
    assert sorted(item.ref for item in listed.items) == [
        "gh:acme/demo#1",
        "gh:acme/demo#2",
    ], "the upstream items are the items; the retired husks do not list"
    assert listed.total == 2

    ranked = backlog(ctx, BacklogParams(project="demo", limit=50))
    refs = [entry.ref for entry in ranked.items]
    assert sorted(refs) == ["gh:acme/demo#1", "gh:acme/demo#2"]
    assert len(set(refs)) == len(refs)

    with ctx.declared.read() as view:
        assert view.work_links_for_subjects(["gh:acme/demo#1"]) == {}, (
            "no work_link for the new subject — one would hide the upstream "
            "item behind its own retired husk (#181 dedup semantics)"
        )


def test_a_retired_ref_still_resolves_for_history(tmp_path: Path) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)
    link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))

    held = get_work(ctx, GetWorkParams(ref="WI-1"))
    assert held.item.superseded_by == "gh:acme/demo#1", (
        "an old trail ends at a marker naming where the item went"
    )


def test_closed_native_items_stay_historical(tmp_path: Path) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)
    result = link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))

    assert all(m.title != "Long done" for m in result.migrated)
    assert len(forge.issues) == 2, "no issue was opened for finished work"
    with ctx.declared.read() as view:
        done = view.work_item_by_ref("WI-3")
    assert done is not None and done.superseded_by is None


# -- acceptance: nothing silently dropped -----------------------------------


def test_a_policy_that_refuses_creates_refuses_the_whole_link(
    tmp_path: Path,
) -> None:
    forge = RecordingForge()
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="none", reason=WHY))

    with pytest.raises(UpstreamWriteRefused, match="2 open native item"):
        link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    with ctx.declared.read() as view:
        project = view.project_by_slug("demo")
    assert project is not None and project.link_state == "unlinked", (
        "no project ends up linked with items no policy would let migrate"
    )
    assert forge.mutations == []


def test_a_mid_migration_failure_is_loud_and_resumable(tmp_path: Path) -> None:
    forge = FlakyForge(allow=1)
    ctx = _instance(tmp_path, forge)
    _project_with_native_items(ctx)

    with pytest.raises(UpstreamWriteFailed) as failure:
        link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    message = str(failure.value)
    assert "stopped at WI-2" in message
    assert "Migrated upstream: WI-1" in message
    assert "Still native: WI-2" in message, (
        "the typed error says which items moved and which did not"
    )

    with ctx.declared.read() as view:
        project = view.project_by_slug("demo")
        wi1 = view.work_item_by_ref("WI-1")
        wi2 = view.work_item_by_ref("WI-2")
    assert project is not None and project.link_state == "linked"
    assert wi1 is not None and wi1.superseded_by == "gh:acme/demo#1"
    assert wi2 is not None and wi2.superseded_by is None, (
        "the un-migrated item is still native, not half-moved"
    )

    # Re-linking resumes: only the still-native item migrates.
    forge.allow = 10_000
    resumed = link_project(ctx, ForgeLinkParams(project="demo", reason=WHY))
    assert [(m.ref, m.subject_key) for m in resumed.migrated] == [
        ("WI-2", "gh:acme/demo#2")
    ]
    with ctx.declared.read() as view:
        wi2 = view.work_item_by_ref("WI-2")
    assert wi2 is not None and wi2.superseded_by == "gh:acme/demo#2"


# -- publish runs the same migration ----------------------------------------


def test_publish_migrates_open_native_items_too(tmp_path: Path) -> None:
    forge = PublishForge()
    pusher = RecordingPusher()
    from vogt.config import VogtConfig

    token_file = tmp_path / "github_token"
    token_file.write_text("ghp_file_token", encoding="utf-8")
    ctx = build_context(
        config=VogtConfig(
            data_dir=tmp_path / "instance",
            sqlite_synchronous="off",
            github_token_file=token_file,
        ),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
        forge_transport=forge,
        pusher=pusher,
    )
    init_instance(ctx, InitParams())
    _seed_repo(tmp_path / "demo")
    register_project(
        ctx,
        RegisterProjectParams(
            name="Demo", root_path=str(tmp_path / "demo"), reason=WHY
        ),
    )
    set_write_back(ctx, SetWriteBackParams(project="demo", policy="full", reason=WHY))
    native_work_item(ctx, kind="bug", title="Carried across", project="demo")

    result = publish_project(ctx, ForgePublishParams(project="demo", reason=WHY))
    assert [(m.ref, m.subject_key) for m in result.migrated] == [
        ("WI-1", "gh:acme/demo#1")
    ]
    with ctx.declared.read() as view:
        wi1 = view.work_item_by_ref("WI-1")
        overlay = view.work_overlay("gh:acme/demo#1")
    assert wi1 is not None and wi1.superseded_by == "gh:acme/demo#1"
    assert overlay is not None
