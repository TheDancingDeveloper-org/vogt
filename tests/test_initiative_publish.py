"""`initiative.publish` — the additive forge tracking-issue projection (#286).

Every deliverable, pinned against a fake forge (no network):

- the task list renders members as `- [ ] #n`, done → checked;
- a re-run *adopts* the marked issue rather than opening a second;
- human text outside the managed markers survives a re-render;
- a `work.update --initiative` membership change re-renders the task list;
- a *closed* initiative PROPOSES its tracking-issue close, never writes it;
- an upstream checkbox tick surfaces as `initiative_checkbox_drift`;
- a cross-project initiative gets one issue per repo, cross-referenced;
- and none of it is destructive: no DELETE, no forced replace.
"""

from __future__ import annotations

import json
import re
from pathlib import Path
from typing import Any

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    CreateInitiativeParams,
    CreateWorkParams,
    DriftDetectParams,
    ForgeLinkParams,
    InitParams,
    PublishInitiativeParams,
    RegisterProjectParams,
    SetWriteBackParams,
    SweepParams,
    UpdateWorkParams,
)
from vogt.application.services import (
    create_initiative,
    create_work,
    detect_drift,
    init_instance,
    link_project,
    publish_initiative,
    register_project,
    set_write_back,
    sweep,
    update_work,
)
from vogt.core.drift import INITIATIVE_CHECKBOX_DRIFT, INITIATIVE_TRACKING_CLOSE
from vogt.core.initiative_projection import MANAGED_END, MANAGED_START, marker_for

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

WHY = "initiative projection test"
_REPO_RE = re.compile(r"/repos/([^/]+/[^/]+)/")


class TrackingForge:
    """A fake github that stores issue bodies and segregates them per repo.

    Enough of the surface for the projection and a sweep: issues are minted
    per `owner/repo`, `GET` returns them with their bodies, a `PATCH` body edit
    is applied (the one edit verb), and everything else answers empty. Recording
    every mutation is what lets "additive, no destructive write" be an assertion.
    """

    def __init__(self) -> None:
        self.issues: dict[str, list[dict[str, Any]]] = {}
        self.requests: list[tuple[str, str]] = []
        self.bodies: list[dict[str, Any]] = []

    def _repo(self, url: str) -> str:
        match = _REPO_RE.search(url)
        return match.group(1) if match else "acme/demo"

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers
        self.requests.append((method, url))
        payload = json.loads(body.decode()) if body else {}
        if body:
            self.bodies.append(payload)
        slug = self._repo(url)
        bucket = self.issues.setdefault(slug, [])

        if method == "POST" and url.rstrip("/").endswith("/issues"):
            number = len(bucket) + 1
            issue = {
                "number": number,
                "title": payload.get("title", ""),
                "state": "open",
                "labels": [{"name": str(n)} for n in payload.get("labels", [])],
                "body": payload.get("body", ""),
                "updated_at": f"2026-08-01T00:00:{number:02d}Z",
                "html_url": f"https://github.com/{slug}/issues/{number}",
            }
            bucket.append(issue)
            return 200, json.dumps(issue).encode()
        if method == "POST" and "/labels" in url:
            return 200, json.dumps({"number": 1}).encode()
        if method == "PATCH" and "/issues/" in url:
            number = int(url.rstrip("/").rsplit("/", 1)[1])
            for issue in bucket:
                if issue["number"] == number:
                    if "state" in payload:
                        issue["state"] = payload["state"]
                    if "body" in payload:
                        issue["body"] = payload["body"]
            return 200, json.dumps({"number": number}).encode()
        if method == "GET" and "/issues" in url and "/comments" not in url:
            return 200, json.dumps(bucket).encode()
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode()
        if "/contents/" in url or "vulnerability" in url or "security" in url:
            return 404, b""
        return 200, b"[]"

    @property
    def mutations(self) -> list[tuple[str, str]]:
        return [(m, u) for m, u in self.requests if m != "GET"]

    def issue(self, slug: str, number: int) -> dict[str, Any]:
        return next(i for i in self.issues[slug] if i["number"] == number)

    def tracking_issue(self, slug: str, initiative_slug: str) -> dict[str, Any]:
        return next(
            i
            for i in self.issues[slug]
            if marker_for(initiative_slug) in (i.get("body") or "")
        )


def _instance(tmp_path: Path, forge: TrackingForge) -> AppContext:
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


def _link(ctx: AppContext, *, name: str, slug_repo: str) -> None:
    register_project(
        ctx,
        RegisterProjectParams(
            name=name,
            root_path=f"/srv/{name.lower()}",
            repo_url=f"https://github.com/{slug_repo}",
            reason=WHY,
        ),
    )
    project = name.lower()
    set_write_back(ctx, SetWriteBackParams(project=project, policy="full", reason=WHY))
    link_project(ctx, ForgeLinkParams(project=project, reason=WHY))


@pytest.fixture
def forge() -> TrackingForge:
    return TrackingForge()


@pytest.fixture
def ctx(tmp_path: Path, forge: TrackingForge) -> AppContext:
    """A linked project `demo` with two member issues in an initiative."""
    instance = _instance(tmp_path, forge)
    _link(instance, name="Demo", slug_repo="acme/demo")
    create_initiative(
        instance,
        CreateInitiativeParams(title="Platform epic", body="The platform.", reason=WHY),
    )
    create_work(
        instance,
        CreateWorkParams(
            kind="feature",
            title="First",
            project="demo",
            initiative="platform-epic",
            reason=WHY,
        ),
    )
    create_work(
        instance,
        CreateWorkParams(
            kind="feature",
            title="Second",
            project="demo",
            initiative="platform-epic",
            reason=WHY,
        ),
    )
    # On a linked project the members are the upstream issues joined to the
    # overlay; a sweep is what puts the newly-created issues in the mirror the
    # projection lists from.
    sweep(instance, SweepParams(reason=WHY))
    return instance


# -- deliverable 1: create, task list, label ------------------------------


def test_publish_creates_one_labelled_tracking_issue_with_a_task_list(
    ctx: AppContext, forge: TrackingForge
) -> None:
    result = publish_initiative(
        ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY)
    )
    assert len(result.tracking_issues) == 1
    row = result.tracking_issues[0]
    assert row.action == "created"
    assert row.members == 2

    tracking = forge.tracking_issue("acme/demo", "platform-epic")
    body = tracking["body"]
    assert MANAGED_START in body and MANAGED_END in body
    # Members #1 and #2 are open → unchecked; the initiative body is carried.
    assert "- [ ] #1 First" in body
    assert "- [ ] #2 Second" in body
    assert "The platform." in body
    # The create carried the `initiative:<slug>` label.
    create = next(b for b in forge.bodies if b.get("title") == "Platform epic")
    assert create["labels"] == ["initiative:platform-epic"]


# -- deliverable 1: create-vs-adopt by marker -----------------------------


def test_a_second_publish_adopts_the_marked_issue_rather_than_duplicating(
    ctx: AppContext, forge: TrackingForge
) -> None:
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    before = len(forge.issues["acme/demo"])
    result = publish_initiative(
        ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY)
    )
    assert result.tracking_issues[0].action == "adopted"
    # No new issue was opened — the marker found the incumbent.
    assert len(forge.issues["acme/demo"]) == before


# -- deliverable 3: the managed region, and only it, is re-rendered -------


def test_human_text_outside_the_markers_survives_a_re_render(
    ctx: AppContext, forge: TrackingForge
) -> None:
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    tracking = forge.tracking_issue("acme/demo", "platform-epic")
    tracking["body"] = (
        "## A human heading\n\nNotes a person added.\n\n"
        + tracking["body"]
        + "\n\nAnd a human footer."
    )
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    body = forge.tracking_issue("acme/demo", "platform-epic")["body"]
    assert "## A human heading" in body
    assert "Notes a person added." in body
    assert "And a human footer." in body
    assert "- [ ] #1 First" in body


# -- deliverable 2: a membership change re-renders the task list ----------


def test_adding_a_member_via_work_update_re_renders_the_task_list(
    ctx: AppContext, forge: TrackingForge
) -> None:
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    # A new issue not yet in the initiative.
    created = create_work(
        ctx, CreateWorkParams(kind="feature", title="Third", project="demo", reason=WHY)
    )
    sweep(ctx, SweepParams(reason=WHY))
    number = int(created.item.ref.rsplit("#", 1)[1])
    assert "Third" not in forge.tracking_issue("acme/demo", "platform-epic")["body"]
    # Add it to the initiative — the tracking issue re-renders adopt-only.
    update_work(
        ctx,
        UpdateWorkParams(ref=created.item.ref, initiative="platform-epic", reason=WHY),
    )
    body = forge.tracking_issue("acme/demo", "platform-epic")["body"]
    assert f"- [ ] #{number} Third" in body


# -- deliverable 4: closing the initiative PROPOSES, never writes ----------


def test_a_closed_initiative_proposes_the_tracking_issue_close(
    tmp_path: Path, forge: TrackingForge
) -> None:
    ctx = _instance(tmp_path, forge)
    _link(ctx, name="Demo", slug_repo="acme/demo")
    create_initiative(
        ctx,
        CreateInitiativeParams(
            title="Sunset epic", body="", state="closed", reason=WHY
        ),
    )
    create_work(
        ctx,
        CreateWorkParams(
            kind="feature",
            title="Only",
            project="demo",
            initiative="sunset-epic",
            reason=WHY,
        ),
    )
    sweep(ctx, SweepParams(reason=WHY))
    publish_initiative(ctx, PublishInitiativeParams(slug="sunset-epic", reason=WHY))

    # The projection never issued a state change on the tracking issue.
    assert not any("state" in b for b in forge.bodies)
    # Closing is proposed as drift, human-gated.
    with ctx.declared.read() as view:
        proposals = view.list_drift(status="open", kind=INITIATIVE_TRACKING_CLOSE)
    assert len(proposals) == 1
    assert proposals[0].proposed_change["action"] == "close"


def test_re_publishing_a_closed_initiative_does_not_pile_up_proposals(
    tmp_path: Path, forge: TrackingForge
) -> None:
    ctx = _instance(tmp_path, forge)
    _link(ctx, name="Demo", slug_repo="acme/demo")
    create_initiative(
        ctx, CreateInitiativeParams(title="Sunset", state="closed", reason=WHY)
    )
    create_work(
        ctx,
        CreateWorkParams(
            kind="feature",
            title="Only",
            project="demo",
            initiative="sunset",
            reason=WHY,
        ),
    )
    sweep(ctx, SweepParams(reason=WHY))
    publish_initiative(ctx, PublishInitiativeParams(slug="sunset", reason=WHY))
    publish_initiative(ctx, PublishInitiativeParams(slug="sunset", reason=WHY))
    with ctx.declared.read() as view:
        proposals = view.list_drift(status="open", kind=INITIATIVE_TRACKING_CLOSE)
    assert len(proposals) == 1


# -- deliverable 5: an upstream tick surfaces as drift --------------------


def test_a_human_tick_upstream_surfaces_as_checkbox_drift(
    ctx: AppContext, forge: TrackingForge
) -> None:
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    sweep(ctx, SweepParams(reason=WHY))
    # A human ticks member #1's box upstream while the item is still open here.
    tracking = forge.tracking_issue("acme/demo", "platform-epic")
    tracking["body"] = tracking["body"].replace("- [ ] #1 First", "- [x] #1 First")
    tracking["updated_at"] = "2026-08-02T00:00:00Z"
    sweep(ctx, SweepParams(reason=WHY))

    result = detect_drift(ctx, DriftDetectParams(reason=WHY))
    kinds = {p.kind for p in result.raised}
    assert INITIATIVE_CHECKBOX_DRIFT in kinds


# -- deliverable 6: one issue per repo, cross-referenced ------------------


def test_a_cross_project_initiative_gets_one_issue_per_repo_cross_referenced(
    tmp_path: Path, forge: TrackingForge
) -> None:
    ctx = _instance(tmp_path, forge)
    _link(ctx, name="Demo", slug_repo="acme/demo")
    _link(ctx, name="Other", slug_repo="acme/other")
    create_initiative(ctx, CreateInitiativeParams(title="Wide epic", reason=WHY))
    create_work(
        ctx,
        CreateWorkParams(
            kind="feature",
            title="Here",
            project="demo",
            initiative="wide-epic",
            reason=WHY,
        ),
    )
    create_work(
        ctx,
        CreateWorkParams(
            kind="feature",
            title="There",
            project="other",
            initiative="wide-epic",
            reason=WHY,
        ),
    )
    sweep(ctx, SweepParams(reason=WHY))
    result = publish_initiative(
        ctx, PublishInitiativeParams(slug="wide-epic", reason=WHY)
    )
    assert {r.project_slug for r in result.tracking_issues} == {"demo", "other"}

    demo_body = forge.tracking_issue("acme/demo", "wide-epic")["body"]
    other_body = forge.tracking_issue("acme/other", "wide-epic")["body"]
    # Each tracking issue links its sibling.
    assert "acme/other/issues" in demo_body
    assert "acme/demo/issues" in other_body


# -- additive / forward-only across every path ----------------------------


def test_nothing_the_projection_does_is_destructive(
    ctx: AppContext, forge: TrackingForge
) -> None:
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    publish_initiative(ctx, PublishInitiativeParams(slug="platform-epic", reason=WHY))
    methods = {m for m, _ in forge.mutations}
    # POST (create/label) and PATCH (managed-region edit) only — never DELETE,
    # and no PUT replace anywhere.
    assert methods <= {"POST", "PATCH"}
    assert not any(m == "DELETE" for m, _ in forge.requests)
