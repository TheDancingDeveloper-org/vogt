"""CI watching: watched-ref failure alerts (WI-867), bound-branch conclusions
and deployed versions (WI-855).

The scenario these pin is the one that went unnoticed: `release-mobile`
failed on the `v0.7.1` and `v0.7.2` tag pushes, which block no pull request,
and the Inbox — which only rolled up the newest revision — never said so.
"""

from __future__ import annotations

import base64
import dataclasses
import json
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.adapters.forge.collectors import ForgeChecksCollector
from vogt.adapters.forge.lanes import DeployLanesCollector, receipt_status
from vogt.application.context import AppContext
from vogt.application.models import (
    BindBranchParams,
    DeployedVersionsParams,
    InboxEntry,
    InboxListParams,
    RegisterProjectParams,
    StartSessionParams,
)
from vogt.application.services import (
    bind_branch,
    deployed_versions,
    list_inbox,
    register_project,
    start_session,
)
from vogt.application.services.ci_watch import (
    CI_BRANCH_CONCLUDED_EVENT,
    announce_concluded,
    notice_text,
)
from vogt.collectors import CollectorContext
from vogt.collectors.base import Finding, finding
from vogt.config import DeployLane
from vogt.core.ci_alerts import branch_ci, watched_failures, watched_ref
from vogt.core.entities import Observation, Project

from tests.conftest import native_work_item
from tests.test_sessions import StandInEngine

WHY = "ci watch test"
REPO = "https://github.com/acme/app"
SHA_72 = "4bcc6a18c7246d9063286f00ff039348c01a94c9"
SHA_73 = "86fcc230adb803de46c026a79dc68f274da34f42"
BRANCHES = ("main", "master", "prod")
TAGS = ("v*",)


# -- the pure lane rules -----------------------------------------------------


def _obs(
    *,
    check: str,
    branch: str | None,
    conclusion: str | None,
    updated_at: str,
    event: str = "push",
    revision: str = SHA_72,
    project_id: str = "prj_1",
    extra: dict[str, object] | None = None,
) -> Observation:
    payload: dict[str, object] = {
        "revision": revision,
        "check": check,
        "conclusion": conclusion,
        "status": "completed" if conclusion else "in_progress",
        "branch": branch,
        "event": event,
        "updated_at": updated_at,
        **(extra or {}),
    }
    return Observation(
        id=f"obs-{check}-{branch}-{updated_at}",
        kind="ci.check",
        subject_key=f"ci:acme/app@{revision}:{check}@{branch}",
        payload=payload,
        content_digest="d",
        project_id=project_id,
        sweep_id="swp",
        collector="forge-checks",
        observed_at=datetime(2026, 9, 28, tzinfo=UTC),
    )


def test_watched_ref_excludes_github_managed_dynamic_runs() -> None:
    # Dependabot's "Update #N" runs arrive as event "dynamic" on main; they are
    # GitHub's managed workflow, not the repository's CI, and must not alert.
    assert watched_ref("main", "dynamic", branches=BRANCHES, tags=TAGS) is None
    assert watched_ref("main", "schedule", branches=BRANCHES, tags=TAGS) is not None


def test_watched_ref_excludes_pull_requests_and_unwatched_branches() -> None:
    assert watched_ref("main", "push", branches=BRANCHES, tags=TAGS) is not None
    assert watched_ref("main", "pull_request", branches=BRANCHES, tags=TAGS) is None
    assert watched_ref("wi-7", "push", branches=BRANCHES, tags=TAGS) is None
    tag = watched_ref("v0.7.2", "push", branches=BRANCHES, tags=TAGS)
    assert tag is not None and tag.kind == "tag" and tag.lane == "v*"
    assert watched_ref(None, "push", branches=BRANCHES, tags=TAGS) is None


def test_the_v072_release_mobile_failure_is_an_alert() -> None:
    failures = watched_failures(
        [
            _obs(
                check="release-mobile",
                branch="v0.7.2",
                conclusion="failure",
                updated_at="2026-09-28T00:27:48Z",
            ),
            _obs(
                check="release",
                branch="v0.7.2",
                conclusion="success",
                updated_at="2026-09-28T00:47:46Z",
            ),
        ],
        branches=BRANCHES,
        tags=TAGS,
    )
    assert [(f.workflow, f.where.ref) for f in failures] == [
        ("release-mobile", "v0.7.2")
    ]


def test_a_later_success_in_the_lane_clears_it_and_cancelled_does_not() -> None:
    failed = _obs(
        check="release-mobile",
        branch="v0.7.2",
        conclusion="failure",
        updated_at="2026-09-28T00:27:48Z",
    )
    cancelled = _obs(
        check="release-mobile",
        branch="v0.7.3",
        conclusion="cancelled",
        updated_at="2026-10-04T01:00:00Z",
        revision=SHA_73,
    )
    assert watched_failures([failed, cancelled], branches=BRANCHES, tags=TAGS)
    fixed = _obs(
        check="release-mobile",
        branch="v0.7.3",
        conclusion="success",
        updated_at="2026-10-04T02:48:09Z",
        revision=SHA_73,
    )
    assert (
        watched_failures([failed, cancelled, fixed], branches=BRANCHES, tags=TAGS) == []
    )


def test_main_failures_clear_per_workflow_and_pr_runs_never_alert() -> None:
    runs = [
        _obs(
            check="build",
            branch="main",
            conclusion="failure",
            updated_at="2026-10-04T02:55:56Z",
        ),
        _obs(
            check="e2e",
            branch="main",
            conclusion="failure",
            updated_at="2026-10-04T02:33:57Z",
        ),
        _obs(
            check="e2e",
            branch="main",
            conclusion="success",
            updated_at="2026-10-04T04:45:00Z",
        ),
        _obs(
            check="ci",
            branch="main",
            conclusion="failure",
            updated_at="2026-10-04T05:00:00Z",
            event="pull_request",
        ),
    ]
    failures = watched_failures(runs, branches=BRANCHES, tags=TAGS)
    assert [f.workflow for f in failures] == ["build"]


def test_branch_ci_waits_for_every_workflow_on_the_newest_revision() -> None:
    old = _obs(
        check="ci",
        branch="wi-1",
        conclusion="failure",
        updated_at="2026-10-04T01:00:00Z",
        revision="aaa",
    )
    running = _obs(
        check="ci",
        branch="wi-1",
        conclusion=None,
        updated_at="2026-10-04T02:00:00Z",
        revision="bbb",
    )
    done = _obs(
        check="lint",
        branch="wi-1",
        conclusion="success",
        updated_at="2026-10-04T02:01:00Z",
        revision="bbb",
    )
    ci = branch_ci([old, running, done], "wi-1")
    assert ci is not None and ci.revision == "bbb" and ci.state == "running"
    finished = running.model_copy(
        update={
            "payload": {
                **running.payload,
                "conclusion": "failure",
                "status": "completed",
            }
        }
    )
    ci = branch_ci([old, finished, done], "wi-1")
    assert ci is not None and ci.state == "failed" and ci.failing == ("ci",)
    assert ci.concluded_at == "2026-10-04T02:01:00Z"
    assert branch_ci([old], "other") is None


# -- the collector -----------------------------------------------------------


class _Forges:
    """GitHub + one Forgejo host + a version endpoint, all canned."""

    def __init__(self) -> None:
        self.calls: list[str] = []
        self.runs: list[dict[str, Any]] = []
        self.push_runs: list[dict[str, Any]] = []
        self.jobs: dict[int, list[dict[str, Any]]] = {}
        self.receipt: dict[str, Any] | None = None
        self.version: dict[str, Any] | None = None
        self.compare: dict[str, Any] | None = None

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body
        assert method == "GET", "CI watching never writes to a forge"
        self.calls.append(url)
        if "/actions/runs/" in url and "/jobs" in url:
            run_id = int(url.split("/actions/runs/")[1].split("/")[0])
            return 200, _json({"jobs": self.jobs.get(run_id, [])})
        if "/actions/runs" in url:
            runs = self.push_runs if "event=push" in url else self.runs
            return 200, _json({"workflow_runs": runs})
        if "/contents/" in url and self.receipt is not None:
            raw = base64.b64encode(json.dumps(self.receipt).encode()).decode()
            return 200, _json({"type": "file", "content": raw})
        if "/compare/" in url and self.compare is not None:
            return 200, _json(self.compare)
        if url.startswith("https://dev.example.test/") and self.version is not None:
            return 200, _json(self.version)
        return 404, b""


def _json(value: object) -> bytes:
    return json.dumps(value).encode()


def _run(
    run_id: int,
    name: str,
    branch: str,
    conclusion: str | None,
    *,
    event: str = "push",
    sha: str = SHA_72,
    updated_at: str = "2026-08-12T04:00:00Z",
) -> dict[str, Any]:
    return {
        "id": run_id,
        "name": name,
        "head_branch": branch,
        "head_sha": sha,
        "event": event,
        "status": "completed" if conclusion else "in_progress",
        "conclusion": conclusion,
        "run_number": run_id % 1000,
        "run_attempt": 1,
        "updated_at": updated_at,
        "html_url": f"https://github.com/acme/app/actions/runs/{run_id}",
    }


@pytest.fixture
def wired(instance: AppContext, tmp_path: Path) -> tuple[AppContext, _Forges]:
    token = tmp_path / "token"
    token.write_text("t0ken")
    forges = _Forges()
    config = instance.config.model_copy(
        update={
            "github_token_file": token,
            "forge_token_files": {"forge.example.test": token},
        }
    )
    ctx = dataclasses.replace(instance, config=config, forge_transport=forges)
    register_project(
        ctx,
        RegisterProjectParams(
            name="App", root_path=str(tmp_path / "app"), repo_url=REPO, reason=WHY
        ),
    )
    return ctx, forges


def _project(ctx: AppContext) -> Project:
    with ctx.declared.read() as view:
        return view.list_projects(limit=10, offset=0)[0]


def _sweep(ctx: AppContext, collector: Any) -> list[Finding]:
    project = _project(ctx)
    found = list(
        collector.collect(CollectorContext(config=ctx.config, clock=ctx.clock), project)
    )
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector=collector.name, scope=[project.id], at=now)
    ctx.observed.append(row.id, found, at=now)
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={"projects": 1}, at=now)
    ctx.observed.rebuild_latest()
    return found


def _checks_collector(ctx: AppContext, forges: _Forges) -> ForgeChecksCollector:
    return ForgeChecksCollector(transport=forges, store=ctx.observed)


def test_the_collector_reads_pushed_runs_and_names_failed_jobs(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, forges = wired
    # The first page is all pull-request churn; the tag run is only on the
    # push-only page — exactly how it fell off before.
    forges.runs = [
        _run(1, "ci", "feature", "success", event="pull_request", sha="f" * 40)
    ]
    forges.push_runs = [
        _run(36362189605, "release-mobile", "v0.7.2", "failure"),
        _run(36362189750, "release", "v0.7.2", "success"),
        _run(36362189751, "build", "main", "success"),
    ]
    forges.jobs[36362189605] = [
        {
            "name": "build the signed Play AAB and upload to internal testing",
            "conclusion": "failure",
            "html_url": "https://github.com/acme/app/actions/runs/36362189605/job/1",
        },
        {"name": "lint", "conclusion": "success", "html_url": "x"},
    ]

    found = [
        f for f in _sweep(ctx, _checks_collector(ctx, forges)) if f.kind == "ci.check"
    ]

    keys = {f.subject_key for f in found}
    assert f"ci:acme/app@{SHA_72}:release-mobile@v0.7.2" in keys
    # The same commit built on main and on the tag stays two subjects.
    assert f"ci:acme/app@{SHA_72}:build@main" in keys
    mobile = next(f for f in found if f.payload["check"] == "release-mobile")
    assert mobile.payload["failed_jobs"] == [
        {
            "name": "build the signed Play AAB and upload to internal testing",
            "conclusion": "failure",
            "url": "https://github.com/acme/app/actions/runs/36362189605/job/1",
        }
    ]
    assert (
        "failed_jobs"
        not in next(f for f in found if f.payload["check"] == "release").payload
    )
    job_calls = [url for url in forges.calls if "/jobs" in url]
    assert len(job_calls) == 1

    # A second sweep reuses the stored job list: no new job lookup.
    _sweep(ctx, _checks_collector(ctx, forges))
    assert len([url for url in forges.calls if "/jobs" in url]) == 1

    # And the Inbox raises it within that one sweep, naming job and log.
    entries = _ci_entries(ctx)
    alert = next(e for e in entries if e.kind == "ci.ref_failure")
    assert alert.title == "release-mobile failed on v0.7.2"
    assert "build the signed Play AAB" in alert.summary
    assert alert.source_url is not None and "/job/1" in alert.source_url
    assert alert.action is not None and alert.action.kind == "observation"
    assert not [e for e in entries if e.title == "CI failing: release-mobile"], (
        "a watched-ref alert is not repeated as a newest-revision failure"
    )


def test_the_job_lookup_budget_is_bounded(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, forges = wired
    forges.push_runs = [_run(100 + i, f"wf-{i}", "main", "failure") for i in range(12)]
    found = _sweep(ctx, _checks_collector(ctx, forges))
    with_jobs = [f for f in found if "failed_jobs" in f.payload]
    assert len(with_jobs) == 5
    assert len([url for url in forges.calls if "/jobs" in url]) == 5


def test_a_later_successful_tag_run_clears_the_alert(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, forges = wired
    forges.push_runs = [_run(1, "release-mobile", "v0.7.2", "failure")]
    _sweep(ctx, _checks_collector(ctx, forges))
    assert any(e.kind == "ci.ref_failure" for e in _ci_entries(ctx))

    forges.push_runs = [
        _run(
            2,
            "release-mobile",
            "v0.7.3",
            "success",
            sha=SHA_73,
            updated_at="2026-08-12T05:00:00Z",
        )
    ]
    _sweep(ctx, _checks_collector(ctx, forges))
    assert not any(e.kind == "ci.ref_failure" for e in _ci_entries(ctx))


def _ci_entries(ctx: AppContext) -> list[InboxEntry]:
    return list_inbox(ctx, InboxListParams(sources=["ci"], limit=100)).entries


# -- bound branches ----------------------------------------------------------


class _InputEngine(StandInEngine):
    """The stand-in engine, plus the `/input` route the CI nudge uses."""

    def __init__(self) -> None:
        super().__init__()
        self.inputs: list[tuple[str, dict[str, Any]]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        if method == "POST" and url.endswith("/input"):
            engine_id = url.rsplit("/", 2)[-2]
            self.inputs.append((engine_id, json.loads(body.decode())))
            return (200, b'{"ok":true}') if engine_id in self.alive else (404, b"")
        return super().__call__(url, headers, body, method)


def _seed(ctx: AppContext, findings: list[Finding]) -> None:
    project = _project(ctx)
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector="forge-checks", scope=[project.id], at=now)
    ctx.observed.append(row.id, findings, at=now)
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={"projects": 1}, at=now)
    ctx.observed.rebuild_latest()


def _branch_check(
    project: Project, name: str, conclusion: str | None, updated_at: str
) -> Finding:
    return finding(
        kind="ci.check",
        subject_key=f"ci:acme/app@{'b' * 40}:{name}@wi-1",
        project=project,
        source_url=f"https://github.com/acme/app/actions/runs/{name}",
        payload={
            "revision": "b" * 40,
            "check": name,
            "conclusion": conclusion,
            "status": "completed" if conclusion else "in_progress",
            "branch": "wi-1",
            "event": "pull_request",
            "updated_at": updated_at,
        },
    )


def test_a_bound_branch_conclusion_reaches_the_inbox_and_the_session_once(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, _forges = wired
    engine = _InputEngine()
    ctx = dataclasses.replace(
        ctx, engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine)
    )
    native_work_item(ctx, title="Watch me", project="app")  # WI-1
    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    bind_branch(ctx, BindBranchParams(ref="WI-1", branch="wi-1", reason=WHY))
    project = _project(ctx)
    now = ctx.clock()
    stamp = (now - timedelta(minutes=5)).isoformat().replace("+00:00", "Z")

    _seed(
        ctx,
        [
            _branch_check(project, "ci", None, stamp),
            _branch_check(project, "lint", "success", stamp),
        ],
    )
    assert announce_concluded(ctx) == 0, "still running: nothing to announce"
    assert not [e for e in _ci_entries(ctx) if e.kind == "ci.branch_concluded"]

    _seed(
        ctx,
        [
            _branch_check(project, "ci", "failure", stamp),
            _branch_check(project, "lint", "success", stamp),
        ],
    )
    entry = next(e for e in _ci_entries(ctx) if e.kind == "ci.branch_concluded")
    assert entry.work_item_ref == "WI-1"
    assert entry.title == "CI failed on wi-1"
    assert "ci" in entry.summary

    assert announce_concluded(ctx) == 1
    typed = [body for _, body in engine.inputs]
    assert typed[0]["text"].startswith("[vogt] CI FAILED on wi-1")
    assert "WI-1" in typed[0]["text"]
    assert typed[1] == {"text": "", "submit": True}

    # Once per conclusion: a second sweep's announce types nothing new.
    assert announce_concluded(ctx) == 0
    assert len(engine.inputs) == 2
    with ctx.declared.read() as view:
        events = [
            e
            for e in view.list_events(after=0, limit=500)
            if e.kind == CI_BRANCH_CONCLUDED_EVENT
        ]
    assert len(events) == 1 and events[0].summary["sessions_notified"] == 1


def test_a_stale_conclusion_is_never_announced(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, _forges = wired
    native_work_item(ctx, title="Old", project="app")  # WI-1
    bind_branch(ctx, BindBranchParams(ref="WI-1", branch="wi-1", reason=WHY))
    project = _project(ctx)
    _seed(ctx, [_branch_check(project, "ci", "success", "2026-01-01T00:00:00Z")])
    assert announce_concluded(ctx) == 0
    # The Inbox still says how it ended.
    assert any(e.kind == "ci.branch_concluded" for e in _ci_entries(ctx))


def test_notice_text_is_one_plain_line() -> None:
    ci = branch_ci(
        [
            _obs(
                check="ci",
                branch="wi-9",
                conclusion="success",
                updated_at="2026-10-04T00:00:00Z",
            )
        ],
        "wi-9",
    )
    assert ci is not None
    from vogt.application.services.ci_watch import BoundBranch

    text = notice_text(
        BoundBranch(
            work_item_ref="WI-9",
            work_item_id=None,
            project_id="prj_1",
            branch="wi-9",
            ci=ci,
        )
    )
    assert "\n" not in text and "\r" not in text
    assert text.startswith("[vogt] CI PASSED on wi-9")
    hostile = ci.runs[0].workflow
    assert hostile == "ci"
    evil = branch_ci(
        [
            _obs(
                check="ci\x1b[2J\rrm -rf",
                branch="wi-9",
                conclusion="failure",
                updated_at="2026-10-04T00:00:00Z",
            )
        ],
        "wi-9",
    )
    assert evil is not None
    line = notice_text(
        BoundBranch(
            work_item_ref="WI-9",
            work_item_id=None,
            project_id="prj_1",
            branch="wi-9",
            ci=evil,
        )
    )
    assert "\x1b" not in line and "\r" not in line


# -- deployed versions -------------------------------------------------------


def _with_lanes(ctx: AppContext, *lanes: DeployLane) -> AppContext:
    return dataclasses.replace(
        ctx, config=ctx.config.model_copy(update={"deploy_lanes": tuple(lanes)})
    )


DEV = DeployLane(
    name="dev",
    project="app",
    receipt_repo="https://forge.example.test/ops/estate",
    receipt_path="receipts/dev/latest.json",
    version_url="https://dev.example.test/api/config",
)


def test_deployed_versions_is_not_configured_without_lanes(
    instance: AppContext,
) -> None:
    result = deployed_versions(instance, DeployedVersionsParams())
    assert result.lanes == [] and result.configured == 0
    assert result.detail is not None and "not configured" in result.detail


def test_a_configured_lane_nobody_swept_is_not_collected(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx = _with_lanes(wired[0], DEV)
    result = deployed_versions(ctx, DeployedVersionsParams())
    assert [lane.status for lane in result.lanes] == ["not_collected"]


def test_deployed_versions_says_how_far_behind_head_a_lane_is(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, forges = wired
    ctx = _with_lanes(ctx, DEV)
    native_work_item(ctx, title="Unpromoted thing", project="app")  # WI-1
    forges.receipt = {
        "environment": "dev",
        "source_sha": SHA_72,
        "live_smoke": {"status": "passed"},
        "timestamp": "2026-10-04T03:03:33Z",
        "image_digest": "sha256:not-retained",
    }
    forges.version = {"source_sha": SHA_72, "product_version": "0.7.2"}
    forges.compare = {
        "status": "ahead",
        "ahead_by": 2,
        "behind_by": 0,
        "commits": [
            {"sha": "c1" * 20, "commit": {"message": "feat: thing (WI-1)\n\nbody"}},
            {"sha": SHA_73, "commit": {"message": "fix: other"}},
        ],
    }
    found = _sweep(ctx, DeployLanesCollector(transport=forges))
    assert "image_digest" not in json.dumps(found[0].payload)
    compare_calls = [url for url in forges.calls if "/compare/" in url]
    assert compare_calls and compare_calls[0].endswith(f"/compare/{SHA_72}...main")

    result = deployed_versions(ctx, DeployedVersionsParams(lane="dev"))
    lane = result.lanes[0]
    assert lane.status == "behind"
    assert lane.deployed_sha == SHA_72 and lane.deployed_from == "live"
    assert lane.version == "0.7.2" and lane.receipt_status == "passed"
    assert lane.head_sha == SHA_73 and lane.commits_behind == 2
    assert [c.work_items for c in lane.unpromoted_commits] == [["WI-1"], []]
    assert [(i.ref, i.title) for i in lane.unpromoted_work_items] == [
        ("WI-1", "Unpromoted thing")
    ]

    forges.compare = {"status": "identical", "ahead_by": 0, "behind_by": 0}
    _sweep(ctx, DeployLanesCollector(transport=forges))
    lane = deployed_versions(ctx, DeployedVersionsParams()).lanes[0]
    assert lane.status == "at_head" and lane.head_sha == SHA_72


def test_a_failed_receipt_raises_an_inbox_alert(
    wired: tuple[AppContext, _Forges],
) -> None:
    ctx, forges = wired
    ctx = _with_lanes(
        ctx,
        DeployLane(
            name="dev",
            project="app",
            receipt_repo="https://forge.example.test/ops/estate",
            receipt_path="receipts/dev/latest.json",
        ),
    )
    forges.receipt = {"source_sha": SHA_72, "live_smoke": {"status": "failed"}}
    _sweep(ctx, DeployLanesCollector(transport=forges))
    alert = next(e for e in _ci_entries(ctx) if e.kind == "deploy.failed")
    assert alert.title == "Deploy failed on lane dev"

    forges.receipt = {"source_sha": SHA_73, "live_smoke": {"status": "passed"}}
    _sweep(ctx, DeployLanesCollector(transport=forges))
    assert not [e for e in _ci_entries(ctx) if e.kind == "deploy.failed"]


def test_an_unreadable_lane_says_why_instead_of_failing() -> None:
    assert receipt_status("PASSED") == "passed"
    assert receipt_status("failure") == "failed"
    assert receipt_status(None) is None
    with pytest.raises(ValueError, match="names no receipt"):
        DeployLane(name="x", project="app")
    with pytest.raises(ValueError, match="together"):
        DeployLane(name="x", project="app", receipt_repo="https://f/a/b")


def test_a_sweep_announces_and_survives_an_announce_failure(
    wired: tuple[AppContext, _Forges], monkeypatch: pytest.MonkeyPatch
) -> None:
    from vogt.application.models import SweepParams
    from vogt.application.services import collect, sweep

    ctx, forges = wired
    forges.push_runs = [_run(1, "release-mobile", "v0.7.2", "failure")]
    calls: list[int] = []

    def boom(_ctx: AppContext) -> int:
        calls.append(1)
        raise RuntimeError("engine fell over")

    monkeypatch.setattr(collect, "announce_concluded", boom)
    result = sweep(ctx, SweepParams(collectors=["forge-checks"], reason=WHY))
    assert calls == [1]
    assert [report.outcome for report in result.reports] == ["ok"]
    assert any(e.kind == "ci.ref_failure" for e in _ci_entries(ctx))
