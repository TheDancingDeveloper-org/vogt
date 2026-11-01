"""Coding sessions.

The engine is stood in for by a transport that records what was sent. That
is the only honest way to assert the thing these tests are actually about:
not that a session was created, but *where* it was told to open and *what
credential* it was given. A stub that returned a session id without keeping
the spec could not tell a session opened in a project's tree from one opened
wherever the engine felt like.
"""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Any, Literal

import pytest
from pydantic import ValidationError

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    CommitForSessionParams,
    CreateWorkParams,
    GetWorkParams,
    HistoryListParams,
    ListAuditParams,
    ListEventsParams,
    ListSessionsParams,
    LogTailParams,
    RegisterProjectParams,
    RelateWorkParams,
    SearchOutputParams,
    SessionBudgetParams,
    SessionSummary,
    StartSessionParams,
    StopSessionParams,
    UpdateProjectParams,
)
from vogt.application.services import (
    commit_for_session,
    create_work,
    get_work,
    history_list,
    list_audit,
    list_events,
    list_sessions,
    log_tail,
    register_project,
    relate_work,
    search_output,
    start_session,
    stop_session,
    update_project,
)
from vogt.application.services.sessions import IMPLEMENT_FOOTER
from vogt.core.auth import hash_token
from vogt.core.entities import AgentPolicy, ReviewPolicy
from vogt.errors import (
    Conflict,
    InvalidRequest,
    MissingReason,
    NotFound,
    PolicyRefused,
    RoleRefused,
)
from vogt.storage.interface import ProjectUpdate

from tests.conftest import native_work_item

WHY = "session test"
ROOT = "/srv/estate/vogt"


class StandInEngine:
    """An engine that answers, and remembers what it was asked."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.killed: list[str] = []
        self.alive: dict[str, str] = {}
        #: Sessions the engine holds that Vogt never started — keyed by id,
        #: value is the activity. Listed by GET /api/sessions like any other.
        self.unlinked: dict[str, str] = {}
        #: Exit codes of sessions whose process has exited but which the
        #: engine still lists (it keeps them until deleted), as it reports
        #: them: with `exit_code` set and `alive` false.
        self.exit_codes: dict[str, int] = {}
        self.counter = 0
        #: When set, the history log endpoint 404s — the engine has no log for
        #: that id (history off, or the id is unknown).
        self.log_missing = False
        #: When set, POST /api/sessions is refused with this reason (400),
        #: the way the engine refuses an effort with no agent CLI to take it.
        self.refuse_start: str | None = None
        #: POST /api/git/op answers. `None` commits; a tuple is the status and
        #: error text the engine refuses with (400 checklist, 409 mid-turn).
        self.git_refusal: tuple[int, str] | None = None
        #: Engine role of a session id, for GET /api/sessions/{id}. A session
        #: that is not named here is a worker.
        self.roles: dict[str, str] = {}
        #: How many the engine says it hid from the default list (WI-1289).
        self.hidden_count: int = 0
        #: A resumed session starts back in the directory its transcript
        #: records; tests that assert that set this.
        self.resume_cwd: str | None = None

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        self.sent.append({"url": url, "method": method, "body": payload})

        if method == "POST" and url.endswith("/api/git/op"):
            if self.git_refusal is not None:
                status, said = self.git_refusal
                return status, json.dumps({"error": said}).encode()
            return 200, json.dumps(
                {
                    "ok": True,
                    "commit": "abc123def456",
                    "digest": "1 file changed, 2 insertions(+)",
                    "diff_stat": " a.rs | 2 ++\n 1 file changed, 2 insertions(+)",
                    "review": ["1 pin"],
                }
            ).encode()
        if method == "POST" and url.endswith("/api/sessions"):
            if self.refuse_start is not None:
                return 400, json.dumps({"error": self.refuse_start}).encode()
            self.counter += 1
            engine_id = f"eng-{self.counter}"
            self.alive[engine_id] = "running"
            return 200, json.dumps(
                {
                    "id": engine_id,
                    "name": payload.get("name", ""),
                    "activity": "running",
                    "cwd": (
                        str(Path(payload["cwd"]).parent / payload["worktree"]["branch"])
                        if payload.get("worktree")
                        else self.resume_cwd
                        if self.resume_cwd and payload.get("resume")
                        else payload.get("cwd", "")
                    ),
                    "exit_code": None,
                }
            ).encode()
        if method == "POST" and url.endswith("/kill"):
            engine_id = url.rsplit("/", 2)[-2]
            self.killed.append(engine_id)
            existed = self.alive.pop(engine_id, None) is not None
            return (200, b'{"ok":true}') if existed else (404, b"")
        if method == "GET" and url.split("?", 1)[0].endswith("/api/sessions"):
            rows = [
                {
                    "id": key,
                    "name": key,
                    "activity": state,
                    "cwd": ROOT,
                    "created_at": "2026-01-03T00:00:00Z",
                    "exit_code": self.exit_codes.get(key),
                    "alive": key not in self.exit_codes,
                }
                for key, state in {**self.alive, **self.unlinked}.items()
            ]
            # The engine's list is `{sessions, hidden_count}` (WI-1289). The
            # bare-list shape an older engine still answers is covered by the
            # client's own reader; this stand-in speaks the current one.
            return 200, json.dumps(
                {"sessions": rows, "hidden_count": self.hidden_count}
            ).encode()
        if method == "GET" and "/api/sessions/" in url:
            engine_id = url.rstrip("/").rsplit("/", 1)[-1]
            known = {**self.alive, **self.unlinked}
            if engine_id not in known and engine_id not in self.roles:
                return 404, b""
            return 200, json.dumps(
                {
                    "summary": {
                        "id": engine_id,
                        "name": engine_id,
                        "activity": known.get(engine_id, "idle"),
                        "cwd": ROOT,
                        "role": self.roles.get(engine_id, "worker"),
                    }
                }
            ).encode()
        # Session history. Canned rows; tests assert both the mapping
        # and, via `self.sent`, that the query params were forwarded.
        path = url.split("?", 1)[0]
        if method == "GET" and path.endswith("/api/history/sessions"):
            return 200, json.dumps(
                [
                    {
                        "id": "hist-1",
                        "name": "old-session",
                        "created_at": "2026-01-01T00:00:00Z",
                        "ended_at": "2026-01-01T00:05:00Z",
                        "exit_code": 0,
                        "cwd": ROOT,
                        "command": "make test",
                        "scrollback_bytes": 128,
                    },
                    {
                        "id": "hist-2",
                        "name": "Oversight",
                        "created_at": "2026-01-01T00:00:00Z",
                        "command": "/usr/local/bin/vogt-agent-auth shell",
                        "role": "oversight",
                        "conversation_agent": "claude",
                        "conversation_id": "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f",
                        "resume_template": "claude",
                        "work_item": "WI-7",
                    },
                ]
            ).encode()
        if method == "GET" and path.endswith("/api/history/search"):
            return 200, json.dumps(
                [
                    {
                        "session_id": "hist-1",
                        "session_name": "old-session",
                        "created_at": "2026-01-01T00:00:00Z",
                        "match_snippet": "found the needle here",
                        "rank": -2.0,
                        "live": False,
                    },
                    {
                        "session_id": "live-9",
                        "session_name": "running-now",
                        "created_at": "2026-01-02T00:00:00Z",
                        "match_snippet": "a needle in a live run",
                        "rank": 0.0,
                        "live": True,
                    },
                ]
            ).encode()
        if method == "GET" and path.endswith("/log"):
            if self.log_missing:
                return 404, b""
            return 200, json.dumps(
                {
                    "session_id": "hist-1",
                    "text": "plain readable tail",
                    "bytes": 19,
                    "total_bytes": 40,
                    "truncated": True,
                }
            ).encode()
        return 404, b""

    @property
    def last_spec(self) -> dict[str, Any]:
        creates = [
            row
            for row in self.sent
            if row["method"] == "POST" and "kill" not in row["url"]
        ]
        spec: dict[str, Any] = creates[-1]["body"]
        return spec

    def env_of_last_start(self) -> dict[str, str]:
        return dict(self.last_spec.get("env", []))


class DeadEngine:
    """An engine that is configured and not answering."""

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        raise EngineUnavailable("the engine is not answering: connection refused")


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def wired(instance: AppContext, engine: StandInEngine) -> AppContext:
    """An instance with a project, a work item, and an engine to talk to."""
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    native_work_item(
        ctx, kind="bug", title="A bug to open a terminal on", project="vogt"
    )
    return ctx


# -- where the session opens ---------------------------------------


def test_a_session_opens_in_the_path_the_registry_records(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The one property the working-directory rule exists for.

    The engine would default the working directory to its own workspace
    root. A session that opened there when Vogt meant this project's tree
    would look right in every list and be wrong about the only thing that
    matters.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert engine.last_spec["cwd"] == ROOT


def test_a_session_can_be_opened_on_a_project(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert result.session.project == "vogt"
    assert result.session.work_item is None
    assert engine.last_spec["cwd"] == ROOT


def test_a_task_is_folded_into_the_session_brief(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A spoken request carries the doing part; the brief must too.

    Without this the agent opens on the project brief, which says in as many
    words that there is no task, and waits — the person then types what they
    already said out loud.
    """
    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="claude",
            task="check how many containers need an update",
            # Off, so no unbound chore is cut and the project brief leads.
            autopilot=False,
            reason=WHY,
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "## Task" in brief
    assert "check how many containers need an update" in brief
    # The project context is still there, under the task.
    assert brief.index("# vogt") < brief.index("## Task")


def test_a_task_without_an_agent_template_is_refused(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1807: a plain shell never reads the brief, so a task would be dropped
    while the session sat idle. Refused before the engine is asked."""
    before = len(engine.alive)
    with pytest.raises(InvalidRequest, match="without a template"):
        start_session(
            wired,
            StartSessionParams(work_item="WI-1", task="fix the sweep", reason=WHY),
        )
    assert len(engine.alive) == before
    # A blank task is no task: the plain shell still starts.
    start_session(wired, StartSessionParams(project="vogt", task="  ", reason=WHY))


def test_an_agent_start_takes_its_task_from_the_reason(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1854: an agent launched with no Task section summarises and idles."""
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1",
            template="claude",
            reason="Task: fix the flaky sweep test and push",
        ),
    )
    brief = engine.last_spec["prompt"]
    task = brief.index("## Task")
    assert "fix the flaky sweep test and push" in brief[task:]
    assert "Task: fix" not in brief


def test_an_explicit_task_wins_over_the_reason(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            project="vogt", template="claude", task="do this", reason="Task: not that"
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "do this" in brief
    assert "not that" not in brief


def test_the_purpose_picks_the_mcp_set(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-2740: workers and reviewers start on the short MCP set."""
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="review", reason=WHY)
    )
    assert engine.last_spec["mcp_set"] == "worker"
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="design", reason=WHY)
    )
    assert "mcp_set" not in engine.last_spec, "no profile ask leaves the engine default"
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", purpose="review", mcp_set="full", reason=WHY
        ),
    )
    assert engine.last_spec["mcp_set"] == "full", "the caller's choice wins"
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "mcp_set" not in engine.last_spec


def test_project_mcp_extra_rides_a_worker_start_only(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-2746: the project's extras go with a worker set, never a full one."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt", mcp_extra=["komodo", " ", "komodo"], reason=WHY
        ),
    )
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="review", reason=WHY)
    )
    assert engine.last_spec["mcp_extra"] == ["komodo"]
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="design", reason=WHY)
    )
    assert "mcp_extra" not in engine.last_spec


def test_a_review_purpose_gets_a_default_review_task(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="review", reason=WHY)
    )
    brief = engine.last_spec["prompt"]
    task = brief[brief.index("## Task") :]
    assert "Review the pull request or branch bound to WI-1" in task
    assert "report blockers" in task


def test_no_task_leaves_the_brief_untouched(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "## Task" not in engine.last_spec["prompt"]


def test_every_brief_says_how_to_report_being_blocked(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-873: the convention travels with the session, not left to be discovered.

    It lives in the companion guide rather than copied into the brief: the
    brief names that file, and the guide carries the full text.
    """
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    prompt = engine.last_spec["prompt"]
    guide = engine.last_spec["companion"]
    assert ".guide.md" in prompt
    assert "session_report_blocked" not in prompt
    assert "session_report_blocked" in guide
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "session_report_unblocked" in engine.last_spec["companion"]


def test_autopilot_adds_the_keep_going_convention_and_is_audited(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-878: autopilot tells the agent to carry on until blocked or done."""
    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="claude",
            task="work the backlog",
            autopilot=True,
            reason=WHY,
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "## Autopilot" in brief
    assert brief.index("## Autopilot") < brief.index("## Task")
    started = [
        event
        for event in list_events(wired, ListEventsParams()).events
        if event.kind == "session.started"
    ]
    assert started[-1].summary["autopilot"] is True
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "## Autopilot" not in engine.last_spec["prompt"]


def test_autopilot_reaches_the_engine_and_is_on_by_default_for_an_agent_task(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-949: the engine keeps an autopilot session awake and re-prompts it,
    so it has to be told; a session started to work something through is
    on autopilot unless asked otherwise."""
    start_session(
        wired,
        StartSessionParams(
            project="vogt", template="opencode", task="work the backlog", reason=WHY
        ),
    )
    assert engine.last_spec.get("autopilot") is True
    brief = engine.last_spec["prompt"]
    assert "## Autopilot" in brief
    assert "`AUTOPILOT: DONE`" in brief, "the line the engine stops nudging on"

    # Explicitly off wins over the default.
    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="opencode",
            task="one question",
            autopilot=False,
            reason=WHY,
        ),
    )
    assert "autopilot" not in engine.last_spec
    assert "## Autopilot" not in engine.last_spec["prompt"]

    # No task, or no agent: nothing to work through, so off by default.
    start_session(
        wired, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    assert "autopilot" not in engine.last_spec
    # And with no agent the task is refused outright (WI-1807).
    with pytest.raises(InvalidRequest, match="without a template"):
        start_session(
            wired, StartSessionParams(project="vogt", task="look around", reason=WHY)
        )


def test_the_enforced_policy_reaches_the_engine_and_guidance_does_not(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1243: the session spec carries only what a hook can refuse on.

    A project with nothing to refuse sends no policy at all, so the start
    stays byte-identical to one from before enforcement.
    """
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "policy" not in engine.last_spec

    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                forbidden_text=["(?i)grok"],
                merge_deploys_prod=True,
                main_branches=["main", "release"],
                commit_identity="a person",
                test_recipe="uv run pytest",
                rules=["guidance only"],
            ),
            reason=WHY,
        ),
    )
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert engine.last_spec["policy"] == {
        "forbidden_text": ["(?i)grok"],
        "merge_deploys_prod": True,
        "main_branches": ["main", "release"],
    }


def test_a_worktree_session_is_cut_from_the_integration_branch(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1298: the session opens in its own worktree, never the checkout.

    The branch is the work item plus the session, cut from the project's
    integration branch. The engine reports the worktree back as the cwd, and
    that is what the session records — the shared checkout stays untouched.
    """
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(base_branch="integrate/vogt-next"),
            reason=WHY,
        ),
    )
    result = start_session(
        wired, StartSessionParams(work_item="WI-1", worktree=True, reason=WHY)
    )
    spec = engine.last_spec["worktree"]
    assert spec["base_ref"] == "integrate/vogt-next"
    assert spec["branch"].startswith("WI-1-")
    assert engine.last_spec["cwd"] == ROOT
    worktree = str(Path(ROOT).parent / spec["branch"])
    assert result.session.cwd == worktree

    # No stated integration branch means main, and a session with no work
    # item takes the project's slug.
    update_project(
        wired,
        UpdateProjectParams(slug="vogt", agent_policy=AgentPolicy(), reason=WHY),
    )
    start_session(wired, StartSessionParams(project="vogt", worktree=True, reason=WHY))
    spec = engine.last_spec["worktree"]
    assert spec["base_ref"] == "main"
    assert spec["branch"].startswith("vogt-")

    # The default leaves the request exactly as it was.
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "worktree" not in engine.last_spec


def test_a_worktree_start_sends_the_checkouts_it_may_not_write_to(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1326: a worktree session carries every registered project's root,
    which the engine refuses git writes into; a start in the checkout itself
    sends exactly what it sent before."""
    start_session(wired, StartSessionParams(project="vogt", worktree=True, reason=WHY))
    assert engine.last_spec["policy"] == {
        "forbidden_text": [],
        "merge_deploys_prod": False,
        "protected_checkouts": [ROOT],
    }
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "policy" not in engine.last_spec


def test_a_cwd_opens_in_an_existing_worktree(
    wired: AppContext,
    engine: StandInEngine,
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """WI-1671: name a worktree that already exists and start in it.

    The engine is handed that directory as cwd and is not asked to cut one.
    The Active/ check is the parent of the project's checkout, so a directory
    that merely has an `Active` component elsewhere is refused, and so is the
    shared checkout itself. A relative path, a stranger repository, a missing
    directory, and an explicit worktree=true are refused before any session
    is started. A symlink is accepted as the directory it points at.
    """
    active_root = tmp_path / "Active"
    checkout = active_root / "vogt"
    checkout.mkdir(parents=True)
    worktree = active_root / "vogt-wi-7"
    worktree.mkdir()
    linked = active_root / "vogt-link"
    linked.symlink_to(worktree, target_is_directory=True)
    stranger = active_root / "other"
    stranger.mkdir()
    decoy = tmp_path / "decoy" / "Active" / "evil"
    decoy.mkdir(parents=True)
    common = tmp_path / "common.git"
    other = tmp_path / "other.git"
    seen: list[Path] = []

    nested = checkout / "src"
    nested.mkdir()

    def fake_git(root: Path, flag: str) -> str:
        seen.append(root)
        if flag == "--show-toplevel":
            # A subdirectory of the checkout reports the checkout. A linked
            # worktree reports itself.
            if root.resolve() in {worktree.resolve(), linked.resolve()}:
                return str(worktree.resolve())
            return str(checkout.resolve())
        if root.resolve() in {worktree.resolve(), checkout.resolve(), nested.resolve()}:
            return str(common)
        return str(other)

    monkeypatch.setattr("vogt.application.services.sessions._git_probe", fake_git)
    register_project(
        wired,
        RegisterProjectParams(name="Checkout", root_path=str(checkout), reason=WHY),
    )

    result = start_session(
        wired,
        StartSessionParams(project="checkout", cwd=str(worktree), reason=WHY),
    )
    assert engine.last_spec["cwd"] == str(worktree.resolve())
    assert "worktree" not in engine.last_spec
    assert result.session.cwd == str(worktree.resolve())

    linked_result = start_session(
        wired,
        StartSessionParams(project="checkout", cwd=str(linked), reason=WHY),
    )
    assert linked_result.session.cwd == str(worktree.resolve())

    with pytest.raises(InvalidRequest, match="absolute"):
        start_session(
            wired,
            StartSessionParams(project="checkout", cwd="Active/vogt-wi-7", reason=WHY),
        )
    with pytest.raises(InvalidRequest, match="shared checkout"):
        start_session(
            wired,
            StartSessionParams(project="checkout", cwd=str(checkout), reason=WHY),
        )
    with pytest.raises(InvalidRequest, match="shared checkout"):
        start_session(
            wired,
            StartSessionParams(project="checkout", cwd=str(nested), reason=WHY),
        )
    with pytest.raises(InvalidRequest, match="not under the workspace Active"):
        start_session(
            wired,
            StartSessionParams(project="checkout", cwd=str(decoy), reason=WHY),
        )
    with pytest.raises(InvalidRequest, match="not a git worktree"):
        start_session(
            wired,
            StartSessionParams(project="checkout", cwd=str(stranger), reason=WHY),
        )
    with pytest.raises(InvalidRequest, match="does not exist"):
        start_session(
            wired,
            StartSessionParams(
                project="checkout",
                cwd=str(active_root / "missing"),
                reason=WHY,
            ),
        )
    with pytest.raises(InvalidRequest, match="cannot be combined"):
        start_session(
            wired,
            StartSessionParams(
                project="checkout",
                cwd=str(worktree),
                worktree=True,
                reason=WHY,
            ),
        )
    # The implement profile sets worktree itself. A cwd still wins, and the
    # engine is not asked to cut one.
    purpose = start_session(
        wired,
        StartSessionParams(
            project="checkout",
            cwd=str(worktree),
            purpose="implement",
            reason=WHY,
        ),
    )
    assert purpose.session.cwd == str(worktree.resolve())
    assert "worktree" not in engine.last_spec
    # The refusals never reached the engine: two accepted starts, then the
    # purpose start.
    assert engine.counter == 3
    assert decoy.resolve() not in seen


def test_the_whole_session_clock_is_unset_unless_the_project_sets_it(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1281 (operator, 2026-10-13): the whole-session wall clock is off by
    default. A project's agent_policy turns it on for the default budget; the
    per-turn 90 minutes and the 120 turns stay as they are."""
    task = StartSessionParams(
        project="vogt", template="klaudia", task="work the backlog", reason=WHY
    )
    start_session(wired, task)
    assert "session_wall_clock_minutes" not in engine.last_spec["budget"]
    assert AgentPolicy().session_wall_clock_minutes is None
    with pytest.raises(ValueError):
        AgentPolicy(session_wall_clock_minutes=0)

    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(session_wall_clock_minutes=240),
            reason=WHY,
        ),
    )
    start_session(wired, task)
    assert engine.last_spec["budget"] == {
        "turns": 120,
        "wall_clock_minutes": 90,
        "session_wall_clock_minutes": 240,
    }
    # A session's own budget is used exactly, and can name the clock itself.
    start_session(
        wired,
        task.model_copy(
            update={"budget": SessionBudgetParams(session_wall_clock_minutes=30)}
        ),
    )
    assert engine.last_spec["budget"] == {"session_wall_clock_minutes": 30}


def test_a_worker_agent_task_gets_the_default_budget(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1245: an agent template with a task in the worker role is bounded
    unless the caller says otherwise."""
    start_session(
        wired,
        StartSessionParams(
            project="vogt", template="opencode", task="work the backlog", reason=WHY
        ),
    )
    assert engine.last_spec["budget"] == {"turns": 120, "wall_clock_minutes": 90}

    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="opencode",
            task="one question",
            budget=SessionBudgetParams(turns=5),
            reason=WHY,
        ),
    )
    assert engine.last_spec["budget"] == {"turns": 5}

    # An empty object is the explicit decline, not the default.
    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="opencode",
            task="unbounded",
            budget=SessionBudgetParams(),
            reason=WHY,
        ),
    )
    assert engine.last_spec["budget"] == {}

    # A shell, a start with no task, and an overseer are not bounded.
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "budget" not in engine.last_spec
    start_session(
        wired,
        StartSessionParams(project="vogt", template="claude", reason=WHY),
    )
    assert "budget" not in engine.last_spec
    start_session(
        wired,
        StartSessionParams(
            project="vogt",
            template="opencode",
            task="watch the others",
            role="oversight",
            reason=WHY,
        ),
    )
    assert "budget" not in engine.last_spec


def test_the_digest_leaves_the_budget_line_to_the_economics_tailer() -> None:
    """C7's digest line is built by the economics tailer (WI-1245 A2), not here."""
    from vogt.application.services.sessions import _digest_of

    digest = _digest_of(
        SessionSummary(
            id="ses-1",
            engine_session_id="eng-1",
            cwd="/work",
            budget=SessionBudgetParams(turns=120, context_tokens=200_000),
        )
    )
    assert digest.budget is None


def test_a_template_is_sent_by_name_for_the_engine_to_expand(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The engine owns the command a template runs; Vogt sends the name.

    Sending `claude` (not `["claude"]`) is what lets the engine expand it to
    the deployment's protected wrapper rather than an unwrapped binary.
    """
    start_session(
        wired,
        StartSessionParams(project="vogt", template="claude", reason=WHY),
    )
    assert engine.last_spec.get("template") == "claude"
    assert "command" not in engine.last_spec


def test_a_session_needs_exactly_one_subject(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        start_session(wired, StartSessionParams(reason=WHY))
    with pytest.raises(InvalidRequest):
        start_session(
            wired, StartSessionParams(work_item="WI-1", project="vogt", reason=WHY)
        )


def test_the_engines_refusal_to_start_reaches_the_caller_verbatim(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A 400 from the engine is its reason, not "answered 400".

    Vogt passes `model` and `effort` through and lets the engine refuse by
    name — an effort with no agent CLI to take it, a cwd outside the
    workspace. Flattening that into "engine unavailable" left the assistant
    reporting "the session engine returned a 400 error" to a person who
    had only asked for a terminal; the sentence names what to change.
    """
    engine.refuse_start = (
        "a session with no command runs the default shell, which has no model "
        "to choose; start it with an agent template (Claude Code, Codex or "
        "OpenCode) to name a model"
    )
    with pytest.raises(InvalidRequest, match="start it with an agent template"):
        start_session(
            wired, StartSessionParams(project="vogt", effort="low", reason=WHY)
        )
    # Nothing was recorded for a session that never started.
    with wired.declared.read() as view:
        assert not [
            r for r in view.list_audit(limit=20) if r.operation == "session.start"
        ]


def test_a_blank_reason_is_refused_before_the_engine_starts(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Audit validation must precede the external side effect.

    The engine and SQLite cannot share a transaction. If the reason is first
    validated inside ``audited_write``, the terminal has already started by
    the time the write is refused, leaving an unrecorded process with a token
    that can never authenticate. Blank input is fully knowable before making
    the HTTP call, so it is rejected there.
    """
    before = len(engine.sent)

    with pytest.raises(MissingReason):
        start_session(
            wired,
            # Adapters validate this shape before the service sees it. Build
            # the impossible transport value directly to prove the service's
            # own write boundary still cannot put an external side effect
            # ahead of its audit invariant.
            StartSessionParams.model_construct(work_item="WI-1", reason=" \t\n"),
        )

    assert len(engine.sent) == before, "an invalid write must start no terminal"
    assert list_sessions(wired, ListSessionsParams(mode="full")).sessions == []


def test_a_work_item_with_no_project_has_no_tree_to_open_in(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Refused rather than guessed.

    Opening in the estate root, or in the first project that happened to
    match, is exactly the heuristic the rule was written against.
    """
    create_work(wired, CreateWorkParams(kind="chore", title="Unassigned", reason=WHY))
    before = len(engine.sent)
    with pytest.raises(InvalidRequest) as raised:
        start_session(wired, StartSessionParams(work_item="WI-2", reason=WHY))
    assert "belongs to no project" in str(raised.value)
    assert len(engine.sent) == before, "nothing should have been started"


# -- who the session writes as -----------------------------


def test_the_session_carries_its_own_token(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A per-session actor, so its writes are distinguishable from every other."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    env = engine.env_of_last_start()
    assert env["VOGT_SESSION_ID"] == result.session.id
    assert env["VOGT_HTTP_TOKEN"].startswith("vogt_")
    assert result.session.actor == f"agent:session:{result.session.id}"

    with wired.declared.read() as view:
        stored = view.token_by_hash(hash_token(env["VOGT_HTTP_TOKEN"]))
    assert stored is not None, "the token the session was given must authenticate"
    assert stored.actor_identity_ref == result.session.actor
    assert set(stored.scopes) == {"read", "work.write", "project.write", "writeback"}, (
        "a session token holds the deployment-chosen scope set — the default is "
        "everything except admin (agent_session_scopes)"
    )


def test_session_scopes_follow_the_deployment_setting(
    wired: AppContext, engine: StandInEngine
) -> None:
    """agent_session_scopes is the one knob; the session token holds exactly it."""
    narrowed = dataclasses.replace(
        wired,
        config=wired.config.model_copy(update={"agent_session_scopes": "read"}),
    )
    result = start_session(narrowed, StartSessionParams(work_item="WI-1", reason=WHY))
    env = engine.env_of_last_start()
    with narrowed.declared.read() as view:
        stored = view.token_by_hash(hash_token(env["VOGT_HTTP_TOKEN"]))
    assert stored is not None
    assert set(stored.scopes) == {"read"}
    assert stored.actor_identity_ref == result.session.actor, (
        "only the scope set is shared — attribution stays per-session"
    )


def test_the_secret_is_written_nowhere(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A credential in a history row is a leak with a timestamp on it.

    Checked against the rows a reader can actually get at — the audit trail
    and the event stream — rather than against the code that writes them.
    The audit row stores a digest of its payload rather than the payload, so
    the failure this guards against would arrive through the *summary*: the
    one part of a write that is stored verbatim and is easy to widen.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    secret = engine.env_of_last_start()["VOGT_HTTP_TOKEN"]

    with wired.declared.read() as view:
        audit = view.list_audit(limit=50)
        events = view.list_events(after=0, limit=50)
    assert audit, "the start is an audited write"
    written = json.dumps([row.model_dump(mode="json") for row in (*audit, *events)])
    assert secret not in written


def test_stopping_a_session_revokes_what_it_held(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    secret = engine.env_of_last_start()["VOGT_HTTP_TOKEN"]

    stop_session(wired, StopSessionParams(id=result.session.id, reason=WHY))

    assert engine.killed == [result.session.engine_session_id]
    with wired.declared.read() as view:
        token = view.token_by_hash(hash_token(secret))
    assert token is not None
    assert token.revoked_at is not None, (
        "the session is over, so what it was running with must stop working"
    )


# -- the link, and what it is not ---------------------------


def test_the_session_is_linked_to_its_work_item(wired: AppContext) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    listed = list_sessions(wired, ListSessionsParams(mode="full", work_item="WI-1"))
    assert [row.id for row in listed.sessions] == [result.session.id]
    assert listed.sessions[0].work_item == "WI-1"
    assert listed.sessions[0].reason == WHY


def test_activity_comes_from_the_engine_not_from_storage(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Liveness is read at the moment of asking, never cached.

    The engine forgetting a session is how a session ends; a stored
    "running" would outlive the process it described.
    """
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert (
        list_sessions(wired, ListSessionsParams(mode="full")).sessions[0].activity
        == "running"
    )

    engine.alive.clear()
    after = list_sessions(wired, ListSessionsParams(mode="full")).sessions[0]
    assert after.activity is None
    assert after.alive is False
    assert after.id == result.session.id, "the link survives the process"


def test_an_exited_process_is_not_alive_though_the_engine_still_lists_it(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-830: listed by the engine is not alive.

    The engine keeps an exited session (its output stays readable) until it
    is deleted; reading presence as liveness is how a `/bin/true` smoke
    session stayed `alive: true` forever.
    """
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine_id = result.session.engine_session_id
    assert engine_id is not None
    engine.alive[engine_id] = "exited"
    engine.exit_codes[engine_id] = 0

    row = list_sessions(wired, ListSessionsParams(mode="full")).sessions[0]
    assert row.activity == "exited"
    assert row.alive is False


def test_an_exited_unlinked_session_is_listed_only_with_the_stopped_ones(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.unlinked["eng-smoke"] = "exited"
    engine.exit_codes["eng-smoke"] = 0

    default = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    assert all(row.id != "eng-smoke" for row in default)

    kept = list_sessions(
        wired, ListSessionsParams(mode="full", include_stopped=True)
    ).sessions
    smoke = [row for row in kept if row.id == "eng-smoke"]
    assert len(smoke) == 1
    assert smoke[0].alive is False
    assert smoke[0].activity == "exited"


def test_an_older_engine_without_alive_is_read_from_the_exit_code() -> None:
    from vogt.adapters.engine.client import EngineSession

    exited = EngineSession.from_payload(
        {"id": "a", "activity": "running", "exit_code": 1}
    )
    running = EngineSession.from_payload({"id": "b", "activity": "running"})
    assert exited.alive is False
    assert running.alive is True


def test_a_stopped_session_leaves_the_list(wired: AppContext) -> None:
    result = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    stop_session(wired, StopSessionParams(id=result.session.id, reason=WHY))

    assert list_sessions(wired, ListSessionsParams(mode="full")).sessions == []
    kept = list_sessions(wired, ListSessionsParams(mode="full", include_stopped=True))
    assert [row.id for row in kept.sessions] == [result.session.id]
    assert kept.sessions[0].stopped_at is not None


def test_stopping_an_unknown_session_is_a_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        stop_session(wired, StopSessionParams(id="ses_nope", reason=WHY))


# -- sessions the engine holds that Vogt never linked --------------
#
# A session started outside Vogt, or one whose link Vogt lost, is still a
# running terminal. An agent asking "what is running here" must see it — that
# it does not is why one session could not see another. It is reported with
# its declared half null and `linked` false, never dressed up as a Vogt link.


def test_an_unlinked_live_session_is_reported(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.unlinked["eng-foreign"] = "running"

    listed = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    foreign = [row for row in listed if row.id == "eng-foreign"]
    assert len(foreign) == 1, "a live session Vogt never started must still show"
    row = foreign[0]
    assert row.linked is False
    assert row.engine_session_id == "eng-foreign"
    assert row.activity == "running" and row.alive is True
    # Null declared half: Vogt will not name what it did not start.
    assert row.project is None
    assert row.work_item is None
    assert row.actor is None
    assert row.reason is None
    # The one honest start time there is: the engine's own.
    assert row.started_at is not None and row.started_at.year == 2026


def test_linked_and_unlinked_sessions_appear_together(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.unlinked["eng-foreign"] = "idle"

    listed = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    by_linked = {row.linked: row for row in listed}
    assert by_linked[True].id == result.session.id
    assert by_linked[True].work_item == "WI-1"
    assert by_linked[False].id == "eng-foreign"


def test_a_project_filter_excludes_unlinked_sessions(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The declared filters filter the declared half, which these lack."""
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    engine.unlinked["eng-foreign"] = "running"

    listed = list_sessions(
        wired, ListSessionsParams(mode="full", project="vogt")
    ).sessions
    assert all(row.linked for row in listed)
    assert "eng-foreign" not in {row.id for row in listed}


def test_unlinked_sessions_are_not_repeated_across_pages(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.unlinked["eng-foreign"] = "running"

    first = list_sessions(wired, ListSessionsParams(mode="full", limit=50, offset=0))
    later = list_sessions(wired, ListSessionsParams(mode="full", limit=50, offset=50))
    assert "eng-foreign" in {row.id for row in first.sessions}
    assert "eng-foreign" not in {row.id for row in later.sessions}
    assert first.total == 1 and first.truncated is False
    assert later.total == 1 and later.sessions == []


def test_a_page_of_hibernated_links_does_not_hide_a_live_unlinked_session(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1266: filter, then page.

    A declared page that is already full of hibernated links used to be
    sliced to `limit` before the digest state filter ran, so the one live
    session the engine held unlinked never appeared and digest mode then
    dropped the hibernated rows, returning nothing.
    """
    for _ in range(51):
        started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
        engine_id = started.session.engine_session_id
        assert engine_id is not None
        engine.alive[engine_id] = "hibernated"
    engine.unlinked["eng-live"] = "running"

    # The unlinked session is the operator's own: the default digest drops
    # it (WI-1525), and `exclude_roles=[]` keeps every role.
    assert list_sessions(wired, ListSessionsParams()).digests == []
    digest = list_sessions(wired, ListSessionsParams(exclude_roles=[]))
    assert [row.id for row in digest.digests] == ["eng-live"]
    assert digest.total == 1 and digest.truncated is False
    assert digest.sessions == []

    # Full mode does not apply the digest state filter, so the hibernated
    # links lead and the live unlinked session is on the next page.
    full = list_sessions(wired, ListSessionsParams(mode="full"))
    assert "eng-live" not in {row.id for row in full.sessions}
    assert full.total == 52 and full.truncated is True
    assert len(full.sessions) == 50

    page = list_sessions(wired, ListSessionsParams(mode="full", limit=50, offset=50))
    assert [row.id for row in page.sessions] == ["ses_0001", "eng-live"]
    assert page.total == 52 and page.truncated is False


def test_more_declared_links_than_the_read_cap_is_reported_truncated(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1266 review: the declared half is capped, and the cap must not look
    complete. One past the 500 links that are read makes `truncated` stay true
    and `total` a floor, on the last page and on an empty page past it."""
    for _ in range(501):
        start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    last = list_sessions(
        wired, ListSessionsParams(mode="full", states=[], limit=500, offset=0)
    )
    assert len(last.sessions) == 500
    assert last.truncated is True and last.total == 501

    beyond = list_sessions(
        wired, ListSessionsParams(mode="full", states=[], limit=500, offset=500)
    )
    assert beyond.sessions == []
    assert beyond.truncated is True and beyond.total == 501
    assert "eng-1" not in {row.engine_session_id for row in last.sessions}


def test_a_dead_engine_hides_no_unlinked_sessions(wired: AppContext) -> None:
    """With the engine unreachable there is nothing to enumerate, and the
    declared links still list — the unlinked pass simply has no input."""
    dead = dataclasses.replace(
        wired,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    listed = list_sessions(dead, ListSessionsParams(mode="full")).sessions
    assert all(row.linked for row in listed)


# -- an absent engine costs sessions and nothing else --------------


def test_with_no_engine_configured_starting_says_so(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    with pytest.raises(EngineUnavailable) as raised:
        start_session(instance, StartSessionParams(project="vogt", reason=WHY))
    assert "VOGT_ENGINE_URL" in str(raised.value), (
        "a refusal names what is missing; 'unavailable' alone reads like an outage"
    )


def test_with_no_engine_the_links_still_list(wired: AppContext) -> None:
    """Vogt's record of what it started does not depend on the engine being up."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    without = dataclasses.replace(wired, engine=None)

    listed = list_sessions(without, ListSessionsParams(mode="full"))
    assert [row.id for row in listed.sessions] == [result.session.id]
    assert listed.engine is not None and "VOGT_ENGINE_URL" in listed.engine
    assert listed.sessions[0].alive is None, (
        "unasked is not the same answer as not running"
    )
    assert listed.hidden_count == 0


def test_the_engines_hidden_count_is_reported_and_the_flag_is_forwarded(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The rail's "N older sessions hidden" reads the engine's count, and
    Show asks for the rows it left out (`?include=hidden`, WI-1289)."""
    engine.hidden_count = 4
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    listed = list_sessions(wired, ListSessionsParams(mode="full"))
    assert len(listed.sessions) == 1
    assert listed.hidden_count == 4
    assert all("include=hidden" not in row["url"] for row in engine.sent)

    shown = list_sessions(wired, ListSessionsParams(mode="full", include_hidden=True))
    assert shown.hidden_count == 4
    assert any(
        row["url"].endswith("/api/sessions?include=hidden") for row in engine.sent
    )


def test_a_dead_engine_is_reported_not_rendered_as_stopped(
    wired: AppContext,
) -> None:
    dead = dataclasses.replace(
        wired,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    listed = list_sessions(dead, ListSessionsParams(mode="full"))
    assert len(listed.sessions) == 1
    assert listed.engine is not None and "not answering" in listed.engine
    assert listed.sessions[0].alive is None


def test_a_session_can_be_closed_when_the_engine_is_gone(wired: AppContext) -> None:
    """Otherwise an engine outage would strand every open session in Vogt."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    dead = dataclasses.replace(
        wired,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    stopped = stop_session(dead, StopSessionParams(id=result.session.id, reason=WHY))
    assert stopped.session.stopped_at is not None


# -- the brief the agent is handed ---------------------------------


def test_the_work_items_brief_travels_with_the_session(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The agent is told what it is working on, from what Vogt records.

    Sent as text, not as a path: the file it will be read from lives on the
    engine's state directory, and the engine is the process that owns that
    filesystem even when both halves share a container.
    """
    native_work_item(
        wired,
        kind="bug",
        title="Sweep drops a page",
        body="The second page of results never arrives.",
        project="vogt",
    )
    result = start_session(wired, StartSessionParams(work_item="WI-2", reason=WHY))

    brief = engine.last_spec["prompt"]
    assert "WI-2 — Sweep drops a page" in brief
    assert "The second page of results never arrives." in brief
    assert result.session.id in brief, "the brief says which session it belongs to"
    guide = engine.last_spec["companion"]
    # The engine names the guide for its own id, so the brief points at it by
    # its relation to the brief file, never by the Vogt id.
    assert ".guide.md" in brief
    assert f"{result.session.id}.guide.md" not in brief
    assert "VOGT_HTTP_TOKEN" not in brief
    assert "VOGT_HTTP_TOKEN" in guide, (
        "a session that describes the work without saying how to record what was "
        "found leaves the write capability undiscovered"
    )


def test_the_brief_carries_the_items_relations(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The brief names the relations, and they change what the work *is*.

    An item that blocks another is not the same job as one that stands alone,
    and an agent handed only a title and a body cannot discover that: the
    relation lives in Vogt and nowhere in the item's own text. It is rendered
    by the related item's ref and title rather than by its id, because an id
    tells a reader nothing they can act on.
    """
    native_work_item(wired, kind="bug", title="Sweep drops a page", project="vogt")
    native_work_item(
        wired, kind="feature", title="Paginate the collector", project="vogt"
    )
    relate_work(
        wired,
        RelateWorkParams(ref="WI-2", kind="depends_on", target="WI-3", reason=WHY),
    )

    start_session(wired, StartSessionParams(work_item="WI-2", reason=WHY))
    brief = engine.last_spec["prompt"]

    assert "## Relations" in brief
    assert "depends on" in brief, (
        "the kind is spelled for a reader, not left as `depends_on`"
    )
    assert "WI-3 — Paginate the collector" in brief, (
        "the related item is named by ref and title; an id is not something "
        "an agent can act on"
    )


def test_the_brief_says_why_the_item_is_ranked_where_it_is(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The brief names the `why`, and it is the half an agent cannot reconstruct.

    A description says what the item is. The ranking says why it is above the
    others, which is the question "should I be working on this?" actually
    turns on — and it is computed from inputs (blocking fan-out, initiative
    weight, age) that are nowhere in the item's own text.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert "## Why this is ranked where it is" in brief
    assert "Score " in brief
    # Every contribution the ranking reports, named — not a single number.
    assert brief.count("- **") >= 2, (
        "the explanation is the per-input contributions; one total is a score, "
        "not a reason"
    )


def test_a_brief_survives_a_ranking_that_cannot_be_computed(
    wired: AppContext,
    engine: StandInEngine,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The session starts either way.

    A brief that refused to be written because a score was unavailable would
    make the ranking a precondition for starting work — the inversion the
    compliance-gates-nothing rule spends its whole sentence forbidding.
    """

    def explodes(*args: object, **kwargs: object) -> None:
        raise NotFound("no such ranked entry")

    monkeypatch.setattr("vogt.application.services.sessions.why", explodes)
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    assert result.session.work_item == "WI-1"
    brief = engine.last_spec["prompt"]
    assert "WI-1" in brief
    assert "## Why this is ranked where it is" not in brief


def test_the_brief_carries_no_credential(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert engine.env_of_last_start()["VOGT_HTTP_TOKEN"] not in brief, (
        "the token is passed in the environment; a copy in a file on disk is "
        "one more place it can be read from"
    )


def test_a_project_session_is_told_no_task_was_asked_for(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Vogt does not invent work for a terminal nobody attached an item to.

    Suggesting "have a look at the backlog" would be the system deciding to
    start work, which is the half of the reversed non-goal that stayed
    refused.
    """
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert "No work item is attached" in brief
    assert "backlog" not in brief.lower()


def test_the_work_item_view_shows_what_is_running_for_it(
    wired: AppContext,
) -> None:
    """The brief's last clause, read through the operation a client actually
    calls."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    item = get_work(wired, GetWorkParams(ref="WI-1"))
    assert [row.id for row in item.sessions] == [started.session.id]
    assert item.sessions[0].activity == "running"

    stop_session(wired, StopSessionParams(id=started.session.id, reason=WHY))
    assert get_work(wired, GetWorkParams(ref="WI-1")).sessions == []


# -- which model, and where a subjectless request goes -------


def test_a_model_and_an_effort_reach_the_engine(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Vogt says which model; how it reaches a CLI is the engine's business.

    Asserted on the wire rather than on the returned summary, because a
    session that recorded `gpt-5.6` in its own row and started the default
    model would satisfy every reader of `session_list` and be wrong about
    the only thing the caller asked for.
    """
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", model="gpt-5.6", effort="medium", reason=WHY
        ),
    )
    assert engine.last_spec["model"] == "gpt-5.6"
    assert engine.last_spec["effort"] == "medium"


def test_naming_no_model_sends_no_model(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The ordinary request is unchanged, field for field.

    The engine refuses a command it cannot pass a model to, so a client that
    started sending an empty field and had it arrive as a present one would
    break every plain shell session.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "model" not in engine.last_spec
    assert "effort" not in engine.last_spec
    assert "resume" not in engine.last_spec


def test_a_resume_reaches_the_engine_and_the_audit(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-833: `resume` names the agent CLI's conversation to continue.

    The engine maps it to `claude --resume` / `codex resume`; Vogt passes it
    through and records what was asked for on the start's event.
    """
    conversation = "0f8fad5b-d9cb-469f-a165-70867728950e"
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", template="claude", resume=conversation, reason=WHY
        ),
    )
    assert engine.last_spec["resume"] == conversation
    started = [
        event
        for event in list_events(wired, ListEventsParams()).events
        if event.kind == "session.started"
    ]
    assert started[-1].summary["resume"] == conversation


def test_a_resumed_session_records_where_the_engine_started_it(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-871: a resumed conversation runs in its transcript's directory.

    The engine moves it there when the transcript records one inside the
    workspace; Vogt records the directory it actually runs in, not the
    project root it asked for.
    """
    engine.resume_cwd = "/srv/elsewhere/worktree"
    result = start_session(
        wired,
        StartSessionParams(
            work_item="WI-1",
            template="claude",
            resume="0f8fad5b-d9cb-469f-a165-70867728950e",
            reason=WHY,
        ),
    )
    assert engine.last_spec["cwd"] == ROOT, "Vogt still asks for the project root"
    assert result.session.cwd == "/srv/elsewhere/worktree"
    # A fresh session is unaffected by the stand-in's override.
    fresh = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert fresh.session.cwd == ROOT


@pytest.mark.parametrize(
    "hostile",
    ["--dangerously-skip-permissions", "-r", "abc def", "a/b", "a;b", "x" * 200, ""],
)
def test_a_resume_that_could_become_a_flag_is_refused_before_the_engine(
    hostile: str, wired: AppContext, engine: StandInEngine
) -> None:
    """It becomes argv, so it is refused at the boundary rather than escaped."""
    with pytest.raises(ValidationError):
        StartSessionParams(
            work_item="WI-1", template="claude", resume=hostile, reason=WHY
        )
    assert not engine.sent


def test_the_model_asked_for_is_recorded_and_read_back(
    wired: AppContext,
) -> None:
    started = start_session(
        wired,
        StartSessionParams(
            project="vogt", model="claude-opus-4-5", effort="high", reason=WHY
        ),
    )
    assert started.session.model == "claude-opus-4-5"
    assert started.session.effort == "high"

    listed = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    assert [(row.model, row.effort) for row in listed] == [("claude-opus-4-5", "high")]


def test_a_subjectless_session_is_refused_when_no_scratch_project_exists(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The working-directory rule, held.

    The tempting alternative is a default working directory, and it is the
    one failure the rule is written against: it succeeds, somewhere plausible.
    The refusal names the setting so a reader can make it work rather than
    guess why it did not.
    """
    before = len(engine.sent)
    with pytest.raises(InvalidRequest) as raised:
        start_session(wired, StartSessionParams(reason=WHY))
    assert "session_scratch_project" in str(raised.value)
    assert len(engine.sent) == before, "nothing should have been started"


def _with_scratch(ctx: AppContext) -> AppContext:
    register_project(
        ctx,
        RegisterProjectParams(
            name="Scratch", root_path="/srv/estate/scratch", reason=WHY
        ),
    )
    return dataclasses.replace(
        ctx,
        config=ctx.config.model_copy(update={"session_scratch_project": "scratch"}),
    )


def test_a_subjectless_session_opens_in_the_configured_scratch_project(
    wired: AppContext, engine: StandInEngine
) -> None:
    """U4: "research the best risotto in Wollongong" has no repository.

    It still opens in a *registered* project's tree — the scratch project is
    a project like any other, chosen by an operator — so the path comes from
    the registry and nowhere else.
    """
    ctx = _with_scratch(wired)

    started = start_session(ctx, StartSessionParams(model="gpt-5.6", reason=WHY))

    assert started.session.project == "scratch"
    assert started.session.work_item is None
    assert engine.last_spec["cwd"] == "/srv/estate/scratch"
    # The name says it fell back rather than being chosen, so a list of
    # sessions does not read as though somebody picked this project.
    assert engine.last_spec["name"].startswith("scratch/")


def test_a_scratch_fallback_says_so_on_the_record(
    wired: AppContext,
) -> None:
    """Otherwise the record names a project nobody asked for.

    A session that fell back to scratch and one deliberately opened in it
    are different decisions, and months later the trail is the only place
    that difference survives. The write is audited either way — this is
    about the summary being able to tell them apart.
    """
    ctx = _with_scratch(wired)
    started = start_session(ctx, StartSessionParams(model="gpt-5.6", reason=WHY))

    assert list_audit(ctx, ListAuditParams(operation="session.start")).records, (
        "the start is audited like every other write"
    )

    events = [
        event
        for event in list_events(ctx, ListEventsParams()).events
        if event.kind == "session.started"
    ]
    assert events, "the start should raise an event"
    summary = events[-1].summary
    assert summary["scratch"] is True
    assert summary["project"] == "scratch"
    assert summary["model"] == "gpt-5.6"
    assert summary["effort"] is None
    assert started.session.project == "scratch"


def test_a_session_somebody_chose_is_not_marked_scratch(
    wired: AppContext,
) -> None:
    """The other half: `scratch` must mean "fell back", not "is that project"."""
    ctx = _with_scratch(wired)
    start_session(ctx, StartSessionParams(project="scratch", reason=WHY))

    events = [
        event
        for event in list_events(ctx, ListEventsParams()).events
        if event.kind == "session.started"
    ]
    assert events[-1].summary["scratch"] is False


def test_naming_both_a_work_item_and_a_project_is_still_refused(
    wired: AppContext,
) -> None:
    with pytest.raises(InvalidRequest):
        start_session(
            wired, StartSessionParams(work_item="WI-1", project="vogt", reason=WHY)
        )


# -- session history reads ------------------------------------------
#
# Thin pass-throughs to the engine, degrading the same way `list_sessions`
# does. The stand-in returns canned rows; `engine.sent` lets a test assert the
# query params were forwarded, since that forwarding is the whole behaviour.


def _history_urls(engine: StandInEngine) -> list[str]:
    return [row["url"] for row in engine.sent if "/api/history" in row["url"]]


def test_history_list_forwards_pagination_and_maps_rows(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = history_list(wired, HistoryListParams(limit=10, offset=5))
    assert result.engine is None
    assert [row.id for row in result.sessions] == ["hist-1", "hist-2"]
    row = result.sessions[0]
    assert row.name == "old-session"
    assert row.exit_code == 0
    assert row.scrollback_bytes == 128
    assert row.conversation_id is None and row.role is None
    # WI-962: a shell with a hand-typed agent reads as what it was, with
    # what a resume needs.
    lost = result.sessions[1]
    assert lost.role == "oversight"
    assert lost.conversation_agent == "claude"
    assert lost.conversation_id == "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f"
    assert lost.resume_template == "claude"
    assert lost.template is None
    # WI-998: what a resume binds again.
    assert lost.work_item == "WI-7"
    assert row.work_item is None
    url = _history_urls(engine)[-1]
    assert "limit=10" in url and "offset=5" in url


def test_search_output_maps_hits_and_flags_live(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = search_output(wired, SearchOutputParams(q="needle", limit=25))
    assert result.engine is None
    assert [m.live for m in result.matches] == [False, True]
    assert result.matches[0].match_snippet == "found the needle here"
    assert result.matches[1].session_id == "live-9"
    url = _history_urls(engine)[-1]
    assert "q=needle" in url
    assert "include_live=true" in url
    assert "limit=25" in url


def test_search_output_can_ask_for_archive_only(
    wired: AppContext, engine: StandInEngine
) -> None:
    search_output(wired, SearchOutputParams(q="needle", include_live=False))
    assert "include_live=false" in _history_urls(engine)[-1]


def test_log_tail_maps_the_preview_and_forwards_flags(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = log_tail(
        wired, LogTailParams(id="hist-1", tail_bytes=4096, strip_ansi=True)
    )
    assert result.engine is None
    assert result.session_id == "hist-1"
    assert result.text == "plain readable tail"
    assert result.bytes == 19
    assert result.total_bytes == 40
    assert result.truncated is True
    url = _history_urls(engine)[-1]
    assert "tail_bytes=4096" in url
    assert "strip_ansi=true" in url


def test_log_tail_missing_log_is_empty_not_an_error(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.log_missing = True
    result = log_tail(wired, LogTailParams(id="gone"))
    assert result.engine is None
    assert result.session_id is None
    assert result.text == ""


def test_history_reads_report_no_engine_configured(instance: AppContext) -> None:
    ctx = dataclasses.replace(instance, engine=None)
    assert "VOGT_ENGINE_URL" in (history_list(ctx, HistoryListParams()).engine or "")
    assert "VOGT_ENGINE_URL" in (
        search_output(ctx, SearchOutputParams(q="x")).engine or ""
    )
    assert "VOGT_ENGINE_URL" in (log_tail(ctx, LogTailParams(id="x")).engine or "")


def test_history_reads_report_a_dead_engine_not_an_error(
    instance: AppContext,
) -> None:
    dead = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    hist = history_list(dead, HistoryListParams())
    assert hist.sessions == []
    assert hist.engine is not None and "not answering" in hist.engine
    search = search_output(dead, SearchOutputParams(q="x"))
    assert search.matches == []
    assert search.engine is not None and "not answering" in search.engine
    tail = log_tail(dead, LogTailParams(id="x"))
    assert tail.session_id is None
    assert tail.engine is not None and "not answering" in tail.engine


# -- permission posture (WI-926) -----------------------------------------------


def _last_started_summary(ctx: AppContext) -> dict[str, Any]:
    with ctx.declared.read() as view:
        events = view.list_events(after=0, limit=500)
    started = [e for e in events if e.kind == "session.started"]
    return dict(started[-1].summary)


def test_the_default_posture_sends_nothing_and_is_audited_as_default(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "permission_mode" not in engine.last_spec
    assert _last_started_summary(wired)["permission_mode"] == "default"


def test_a_person_can_grant_one_task_a_posture_and_it_is_on_the_record(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", template="claude", permission_mode="bypass", reason=WHY
        ),
    )
    assert engine.last_spec["permission_mode"] == "bypass"
    assert _last_started_summary(wired)["permission_mode"] == "bypass"
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1",
            template="claude",
            permission_mode="accept_edits",
            reason=WHY,
        ),
    )
    assert engine.last_spec["permission_mode"] == "accept-edits"


def test_no_account_sends_nothing_and_a_named_one_is_sent_and_audited(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "account" not in engine.last_spec
    assert _last_started_summary(wired)["account"] is None
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", template="claude", account="acct2", reason=WHY
        ),
    )
    # The name only: the engine owns the mapping to a config directory, so no
    # path (and no CLAUDE_CONFIG_DIR) comes from Vogt.
    assert engine.last_spec["account"] == "acct2"
    assert "CLAUDE_CONFIG_DIR" not in engine.env_of_last_start()
    assert _last_started_summary(wired)["account"] == "acct2"


@pytest.mark.parametrize("bad", ["../x", "Acct", "a b", "/etc", "", "x" * 33])
def test_an_account_must_be_a_plain_name(bad: str) -> None:
    with pytest.raises(ValidationError):
        StartSessionParams(work_item="WI-1", account=bad, reason=WHY)


def test_a_sessions_account_comes_back_from_the_engine() -> None:
    from vogt.adapters.engine.client import EngineSession

    payload = {"id": "s1", "name": "n", "alive": True, "account": "acct2"}
    assert EngineSession.from_payload(payload).account == "acct2"
    assert EngineSession.from_payload({**payload, "account": None}).account is None
    assert EngineSession.from_payload({"id": "s1", "name": "n"}).account is None


def test_an_agent_cannot_start_a_bypassed_session(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Not even the overseeing runner: a child without permission checks
    would let any agent escape its own guardrails by proxy."""
    from vogt.core.principal import Principal
    from vogt.errors import BypassRefused

    agent = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="agent:session:ses_runner",
            kind="agent",
            display_name="Session ses_runner",
        ),
    )
    starts_before = len(engine.sent)
    with pytest.raises(BypassRefused, match="only be granted by a person"):
        start_session(
            agent,
            StartSessionParams(
                work_item="WI-1",
                template="claude",
                permission_mode="bypass",
                reason=WHY,
            ),
        )
    assert len(engine.sent) == starts_before, "nothing reached the engine"
    # The other postures stay open to agents.
    start_session(
        agent,
        StartSessionParams(
            work_item="WI-1",
            template="claude",
            permission_mode="accept_edits",
            reason=WHY,
        ),
    )


def test_every_brief_says_a_denial_is_reported_not_routed_around(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired, StartSessionParams(work_item="WI-1", autopilot=True, reason=WHY)
    )
    prompt = engine.last_spec["prompt"]
    guide = engine.last_spec["companion"]
    assert "A permission denial is one of these" not in prompt
    assert "A permission denial is one of these" in guide
    # Autopilot's own line stays in the brief: it is per start, not boilerplate.
    assert "never routed around" in prompt


# -- a requested stop is not a crash (WI-913) ----------------------------------


def test_stopping_tells_the_engine_who_and_why_so_the_exit_reads_stopped(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    stop_session(
        wired, StopSessionParams(id=result.session.id, reason="answer ingested")
    )
    kill = next(r for r in engine.sent if r["url"].endswith("/kill"))
    assert kill["body"] == {
        "reason": "answer ingested",
        "by": wired.principal.identity_ref,
    }


def test_a_stopped_row_carries_who_stopped_it(
    wired: AppContext, engine: StandInEngine
) -> None:
    from vogt.adapters.engine.client import EngineSession

    row = EngineSession.from_payload(
        {
            "id": "eng-9",
            "name": "child",
            "activity": "stopped",
            "alive": False,
            "exit_code": 137,
            "cwd": ROOT,
            "stop": {"by": "agent:session:ses_parent", "reason": "ingested", "at": "x"},
        }
    )
    assert (row.stopped_by, row.stop_reason) == ("agent:session:ses_parent", "ingested")


# -- WI-1324: the engine's workspace record reaches the summary ---------------


def test_a_row_in_a_git_tree_carries_its_workspace_kind_and_path() -> None:
    from vogt.adapters.engine.client import EngineSession
    from vogt.application.services.sessions import _live_fields

    base = {"id": "eng-9", "name": "w", "activity": "idle", "cwd": ROOT}
    row = EngineSession.from_payload(
        {
            **base,
            "workspace": {
                "kind": "worktree",
                "path": "/w/vogt-wi-1",
                "repo_root": "/w/vogt",
                "branch": "wi-1",
                "baseline_dirty": [],
            },
        }
    )
    fields = _live_fields(row)
    assert (fields["workspace_kind"], fields["workspace_path"]) == (
        "worktree",
        "/w/vogt-wi-1",
    )
    for payload in ({**base}, {**base, "workspace": None}):
        plain = EngineSession.from_payload(payload)
        assert (plain.workspace_kind, plain.workspace_path) == (None, None)


# -- WI-926: an engine-started agent session gets an agent identity --------------


def _as(
    ctx: AppContext, identity_ref: str, kind: Literal["human", "agent"] = "agent"
) -> AppContext:
    from vogt.core.principal import Principal

    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=identity_ref, kind=kind, display_name=identity_ref
        ),
    )


ENGINE_SESSION = "0cc46582-ed57-4327-9a28-624a7ae2474a"


def test_the_engine_mints_an_agent_token_for_a_session_it_started(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The gap WI-926's dev check found: an agent in a session the engine
    started (GUI, protected template) ran with the pod's token, bound to a
    person, so the core let it grant `bypass`. Its own token is an agent's."""
    from vogt.application.models import EngineSessionTokenParams
    from vogt.application.services import engine_session_token
    from vogt.application.services.auth import authenticate
    from vogt.errors import BypassRefused

    the_engine = _as(wired, wired.config.bootstrap_core_token_actor)
    minted = engine_session_token(
        the_engine,
        EngineSessionTokenParams(engine_session_id=ENGINE_SESSION, reason=WHY),
    )
    assert minted.actor == f"agent:engine:{ENGINE_SESSION}"
    assert minted.token is not None and minted.revoked == 0

    who = authenticate(wired, bearer=minted.token).principal
    assert who.kind == "agent" and who.identity_ref == minted.actor
    with pytest.raises(BypassRefused):
        start_session(
            dataclasses.replace(wired, principal=who),
            StartSessionParams(
                work_item="WI-1",
                template="claude",
                permission_mode="bypass",
                reason=WHY,
            ),
        )

    # A second mint (a wake) supersedes the first; revoking ends both.
    again = engine_session_token(
        the_engine,
        EngineSessionTokenParams(engine_session_id=ENGINE_SESSION, reason=WHY),
    )
    assert again.revoked == 1 and again.token != minted.token
    gone = engine_session_token(
        the_engine,
        EngineSessionTokenParams(
            engine_session_id=ENGINE_SESSION, revoke=True, reason=WHY
        ),
    )
    assert gone.token is None and gone.revoked == 1
    from vogt.errors import VogtError

    with pytest.raises(VogtError):
        authenticate(wired, bearer=again.token)


def test_only_the_engine_may_mint_a_session_token(wired: AppContext) -> None:
    from vogt.application.models import EngineSessionTokenParams
    from vogt.application.services import engine_session_token
    from vogt.errors import EngineOnly

    for who in (
        _as(wired, "agent:session:ses_runner"),
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        _as(wired, "local:vogt", kind="human"),
    ):
        with pytest.raises(EngineOnly):
            engine_session_token(
                who,
                EngineSessionTokenParams(engine_session_id=ENGINE_SESSION, reason=WHY),
            )
    with pytest.raises(ValidationError):
        EngineSessionTokenParams(engine_session_id="../../etc", reason=WHY)


def test_commit_for_a_session_returns_the_digest_and_is_audited(
    wired: AppContext, engine: StandInEngine
) -> None:
    """C14: the commit goes through the engine's git op, named by the session,
    and the audit records the SHA and digest — never the diff."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    result = commit_for_session(
        wired,
        CommitForSessionParams(
            id=started.session.id,
            message="record the session commit",
            paths=["src/a.rs"],
            reason=WHY,
        ),
    )
    assert result.commit == "abc123def456"
    assert result.digest == "1 file changed, 2 insertions(+)"
    assert result.diff is None
    assert result.review == ["1 pin"]
    sent = [row for row in engine.sent if row["url"].endswith("/api/git/op")]
    assert sent[-1]["body"] == {
        "op": "commit",
        "session": started.session.engine_session_id,
        "message": "record the session commit",
        "paths": ["src/a.rs"],
        "full_diff": False,
        "forbidden_text": [],
    }
    audit = list_audit(wired, ListAuditParams(operation="session.commit_for"))
    assert audit.records, "the commit is an audited action"
    events = [
        event
        for event in list_events(wired, ListEventsParams()).events
        if event.kind == "session.committed"
    ]
    assert events[-1].summary["commit"] == "abc123def456"
    assert "diff" not in events[-1].summary


def test_a_blank_commit_message_never_reaches_the_engine(
    wired: AppContext, engine: StandInEngine
) -> None:
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    with pytest.raises(InvalidRequest):
        commit_for_session(
            wired,
            CommitForSessionParams(id=started.session.id, message="   ", reason=WHY),
        )
    assert not [row for row in engine.sent if row["url"].endswith("/api/git/op")]


@pytest.mark.parametrize(
    ("status", "said", "error"),
    [
        (400, "staged diff has a conflict marker", InvalidRequest),
        (400, "staged diff has an intent-to-add entry", InvalidRequest),
        (400, "message matches forbidden text", InvalidRequest),
        (409, "session eng-1 is Running: a commit waits until it is quiet", Conflict),
    ],
)
def test_the_engines_commit_refusal_reaches_the_caller(
    wired: AppContext,
    engine: StandInEngine,
    status: int,
    said: str,
    error: type[Exception],
) -> None:
    """Each checklist rejection (conflict, secret, intent-to-add, forbidden
    text, a worker mid-turn) is the engine's word, surfaced as a typed error
    and never audited as a commit."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.git_refusal = (status, said)
    with pytest.raises(error, match=said):
        commit_for_session(
            wired,
            CommitForSessionParams(
                id=started.session.id, message="should not land", reason=WHY
            ),
        )
    assert not list_audit(
        wired, ListAuditParams(operation="session.commit_for")
    ).records


def test_a_named_worktree_is_forwarded_and_patterns_are_not(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The caller names a worktree; it never names the forbidden patterns.
    A project with no policy sends none."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    commit_for_session(
        wired,
        CommitForSessionParams(
            id=started.session.id,
            message="commit in the linked worktree",
            worktree="/srv/estate/vogt-wt",
            reason=WHY,
        ),
    )
    sent = [row for row in engine.sent if row["url"].endswith("/api/git/op")]
    assert sent[-1]["body"]["worktree"] == "/srv/estate/vogt-wt"
    assert sent[-1]["body"]["forbidden_text"] == []


def test_the_projects_forbidden_text_is_what_the_engine_checks(
    wired: AppContext, engine: StandInEngine
) -> None:
    """C13 meets C14: the patterns come from the target's project policy."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(forbidden_text=["(?i)grok"]),
            reason=WHY,
        ),
    )
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    commit_for_session(
        wired,
        CommitForSessionParams(
            id=started.session.id, message="commit for the worker", reason=WHY
        ),
    )
    sent = [row for row in engine.sent if row["url"].endswith("/api/git/op")]
    assert sent[-1]["body"]["forbidden_text"] == ["(?i)grok"]
    assert "forbidden_text" not in CommitForSessionParams.model_fields


def test_a_session_cannot_commit_its_own_work(
    wired: AppContext, engine: StandInEngine
) -> None:
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    caller = _as(wired, f"agent:session:{started.session.id}")
    with pytest.raises(RoleRefused, match="does not commit its own work"):
        commit_for_session(
            caller,
            CommitForSessionParams(
                id=started.session.id, message="my own commit", reason=WHY
            ),
        )
    assert not [row for row in engine.sent if row["url"].endswith("/api/git/op")]


def test_a_worker_cannot_commit_for_another_worker(
    wired: AppContext, engine: StandInEngine
) -> None:
    target = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    caller = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    engine.roles[ENGINE_SESSION] = "worker"
    with pytest.raises(RoleRefused, match="is worker"):
        commit_for_session(
            caller,
            CommitForSessionParams(
                id=target.session.id, message="another worker's commit", reason=WHY
            ),
        )
    assert not [row for row in engine.sent if row["url"].endswith("/api/git/op")]


def test_oversight_may_commit_for_a_worker(
    wired: AppContext, engine: StandInEngine
) -> None:
    target = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    caller = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    engine.roles[ENGINE_SESSION] = "oversight"
    result = commit_for_session(
        caller,
        CommitForSessionParams(
            id=target.session.id, message="oversight commits it", reason=WHY
        ),
    )
    assert result.commit == "abc123def456"


def test_a_person_may_commit_without_a_role_lookup(
    wired: AppContext, engine: StandInEngine
) -> None:
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    person = _as(wired, "local:someone", kind="human")
    result = commit_for_session(
        person,
        CommitForSessionParams(
            id=started.session.id, message="a person commits it", reason=WHY
        ),
    )
    assert result.commit == "abc123def456"
    looked_up = [
        row
        for row in engine.sent
        if row["method"] == "GET" and "/api/sessions/" in row["url"]
    ]
    assert looked_up == []


def test_the_engines_stack_secret_may_mint_whoever_it_was_issued_to(
    wired: AppContext, tmp_path: Any
) -> None:
    """Found validating WI-926 on vogt-dev: the stack secret the engine and the
    core share had been issued by hand to the person `local:vogt`, and
    adoption is idempotent on the hash, so it never became `agent:vogt-engine`.
    The engine's mint was refused and its sessions fell back to the pod's
    token. The core now knows the engine by its exact credential."""
    from vogt.application.models import EngineSessionTokenParams, IssueTokenParams
    from vogt.application.services import engine_session_token
    from vogt.application.services.auth import authenticate, issue_token
    from vogt.errors import EngineOnly

    def issued(name: str) -> str:
        return issue_token(
            wired,
            IssueTokenParams(
                actor=wired.principal.identity_ref,
                name=name,
                scopes="admin",
                reason=WHY,
            ),
        ).secret

    stack_secret, phone = issued("stack secret"), issued("phone")
    secret_file = tmp_path / "vogt-core-token"
    secret_file.write_text(stack_secret + "\n", encoding="utf-8")
    ctx = dataclasses.replace(
        wired,
        config=wired.config.model_copy(
            update={"bootstrap_core_token_file": secret_file}
        ),
    )

    def as_bearer(secret: str) -> AppContext:
        who = authenticate(ctx, bearer=secret)
        return dataclasses.replace(ctx, principal=who.principal, token=who.token)

    engine = as_bearer(stack_secret)
    assert engine.principal.kind == "human", "a person, as on the estate"
    minted = engine_session_token(
        engine, EngineSessionTokenParams(engine_session_id=ENGINE_SESSION, reason=WHY)
    )
    assert minted.token is not None
    # The same person's other credential is still not the engine.
    with pytest.raises(EngineOnly):
        engine_session_token(
            as_bearer(phone),
            EngineSessionTokenParams(engine_session_id=ENGINE_SESSION, reason=WHY),
        )


def test_a_project_policy_is_in_the_brief_and_only_the_enforced_slice_is_sent(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The brief says the whole policy. The engine gets only what a hook can
    refuse on (WI-1243): guidance stays in the brief."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                forbidden_text=["(?i)wip"],
                merge_deploys_prod=True,
                main_branches=["release"],
                commit_identity="Author Name <name@example.com>",
                test_recipe="uv run pytest",
                rules=["Never commit the design directory."],
            ),
            reason=WHY,
        ),
    )
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert "## Project policy — vogt" in brief
    assert "(?i)wip" in brief
    assert "Merges deploy production" in brief
    assert "`release`" in brief
    assert "uv run pytest" in brief
    assert "Never commit the design directory." in brief
    assert engine.last_spec["policy"] == {
        "forbidden_text": ["(?i)wip"],
        "merge_deploys_prod": True,
        "main_branches": ["release"],
    }


def test_a_landing_block_is_stored_and_rendered_with_the_item_number(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1318: the three blocks round-trip, and the brief fills `{n}` in."""
    from vogt.core.entities import LandingPolicy, ReviewPolicy, VerificationPolicy

    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                landing=LandingPolicy(
                    base_branch="integrate/vogt-next",
                    worker_lands_by="push_branch",
                    merge_by="oversight",
                    merge_mode="rebase",
                ),
                review=ReviewPolicy(never_models=["grok-4.7"]),
                verification=VerificationPolicy(
                    required_for_done=True, lanes=["vogt-dev"]
                ),
            ),
            reason=WHY,
        ),
    )
    with wired.declared.read() as view:
        stored = view.project_by_slug("vogt")
    assert stored is not None and stored.agent_policy.landing is not None
    assert stored.agent_policy.landing.merge_mode == "rebase"
    assert stored.agent_policy.review is not None
    assert stored.agent_policy.review.never_models == ["grok-4.7"]
    assert stored.agent_policy.verification is not None
    assert stored.agent_policy.verification.required_for_done is True

    item = native_work_item(wired, title="land me", project="vogt")
    start_session(wired, StartSessionParams(work_item=item.ref, reason=WHY))
    brief = engine.last_spec["prompt"]
    assert "## Landing rule for this repo" in brief
    assert f"`^wi-{item.ref.split('-', 1)[1]}-[a-z0-9-]+$`" in brief
    assert "**Base** `integrate/vogt-next`" in brief
    assert "**Lands by** push_branch" in brief
    assert "**Merge by** oversight (rebase)" in brief
    assert "**Deploy by** operator" in brief
    # The blocks are recorded, not enforced: the hook payload is unchanged.
    assert "policy" not in engine.last_spec


def test_a_project_without_a_landing_block_renders_no_landing_rule(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "Landing rule" not in engine.last_spec["prompt"]


def test_a_graft_landing_renders_graft_wording_not_push_branch(
    wired: AppContext, engine: StandInEngine
) -> None:
    """#1240: a graft project's brief says to land on the graft line."""
    from vogt.core.entities import GraftLanding, LandingPolicy

    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                landing=LandingPolicy(
                    base_branch="main",
                    worker_lands_by="graft",
                    graft=GraftLanding(
                        line="backlog-1",
                        mirror="vogt",
                        endpoint="http://graft-dev:17888",
                    ),
                    merge_by="oversight",
                    merge_mode="rebase",
                )
            ),
            reason=WHY,
        ),
    )
    with wired.declared.read() as view:
        stored = view.project_by_slug("vogt")
    assert stored is not None and stored.agent_policy.landing is not None
    assert stored.agent_policy.landing.graft is not None
    assert stored.agent_policy.landing.graft.line == "backlog-1"
    item = native_work_item(wired, title="graft me", project="vogt")
    start_session(wired, StartSessionParams(work_item=item.ref, reason=WHY))
    brief = engine.last_spec["prompt"]
    assert (
        "**Lands by** land on graft line `backlog-1` with your session token "
        "(see worker setup)"
    ) in brief
    assert "`http://graft-dev:17888`" in brief
    assert "**Lands by** push_branch" not in brief


def test_a_bad_branch_pattern_or_a_merge_commit_mode_is_refused(
    wired: AppContext,
) -> None:
    from vogt.core.entities import LandingPolicy

    with pytest.raises(ValidationError, match="branch_pattern"):
        update_project(
            wired,
            UpdateProjectParams(
                slug="vogt",
                agent_policy=AgentPolicy(
                    landing=LandingPolicy(
                        base_branch="main",
                        branch_pattern="(unclosed",
                        worker_lands_by="push_branch",
                        merge_by="oversight",
                        merge_mode="rebase",
                    )
                ),
                reason=WHY,
            ),
        )
    with pytest.raises(ValidationError, match="merge_mode"):
        LandingPolicy(
            base_branch="main",
            worker_lands_by="push_branch",
            merge_by="oversight",
            merge_mode="merge",  # type: ignore[arg-type]
        )


def test_a_policy_with_nothing_to_enforce_sends_no_hook_payload(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Guidance alone changes the brief and nothing about the start request."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(test_recipe="uv run pytest"),
            reason=WHY,
        ),
    )
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert "uv run pytest" in engine.last_spec["prompt"]
    assert "policy" not in engine.last_spec


def test_a_pattern_that_does_not_compile_is_refused(wired: AppContext) -> None:
    """A rule that could never match is a mistake, stored it would do nothing."""
    with pytest.raises(ValidationError, match="does not compile"):
        update_project(
            wired,
            UpdateProjectParams(
                slug="vogt",
                agent_policy=AgentPolicy(forbidden_text=["(unclosed"]),
                reason=WHY,
            ),
        )


def test_a_pattern_the_engine_dialect_cannot_match_is_refused(
    wired: AppContext,
) -> None:
    """Python compiles lookaround, backreferences, conditional groups and
    named-group references; the engine's regex crate does not, so such a
    pattern would be stored and then never fire. The refusal happens at the
    write, not when the model is built."""
    for pattern in (
        "secret(?=key)",
        "secret(?!key)",
        "(?<=pre)secret",
        "(?<!pre)secret",
        r"(\w+)\s+\1",
        r"(?P<name>\w+) (?P=name)",
        r"(a)?(?(1)yes|no)",
    ):
        # Building the model is allowed: a project stored before this rule
        # existed must still load.
        AgentPolicy(forbidden_text=[pattern])
        with pytest.raises(InvalidRequest, match="does not support"):
            update_project(
                wired,
                UpdateProjectParams(
                    slug="vogt",
                    agent_policy=AgentPolicy(forbidden_text=[pattern]),
                    reason=WHY,
                ),
            )


def test_a_stored_unsupported_pattern_still_loads(wired: AppContext) -> None:
    """The dialect rule is a write-time check. A project whose policy was
    stored before it existed must still load, not vanish from every read."""
    stored = AgentPolicy(
        forbidden_text=[r"(?P<name>\w+) (?P=name)", r"(a)?(?(1)yes|no)"]
    )
    with wired.declared.write() as txn:
        project = txn.project_by_slug("vogt")
        assert project is not None
        txn.update_project(
            project.id,
            ProjectUpdate(agent_policy=stored),
            at=wired.clock(),
        )
    with wired.declared.read() as view:
        loaded = view.project_by_slug("vogt")
    assert loaded is not None
    assert loaded.agent_policy.forbidden_text == [
        r"(?P<name>\w+) (?P=name)",
        r"(a)?(?(1)yes|no)",
    ]


def test_touch_points_render_and_the_map_stays_inside_the_tree(
    instance: AppContext, engine: StandInEngine, tmp_path: Path
) -> None:
    """Files, symbols, done and gates are a block; the map is the tree they sit in.

    A file outside the working tree is a hint that was wrong, so it adds no
    directory. Dotfiles are skipped, and a brief with no touch points is
    unchanged.
    """
    src = tmp_path / "src"
    src.mkdir()
    (src / "lib.py").write_text("x = 1\n", encoding="utf-8")
    (src / ".secret").write_text("no\n", encoding="utf-8")
    (tmp_path / "README.md").write_text("hi\n", encoding="utf-8")
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Mapped", root_path=str(tmp_path), reason=WHY)
    )
    start_session(
        ctx,
        StartSessionParams(
            project="mapped",
            template="claude",
            task="wire the map",
            files=["src/lib.py", "README.md:1-3", "/etc/passwd"],
            symbols=["lib"],
            done=["the map renders"],
            gates=["uv run pytest"],
            reason=WHY,
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "## Touch points" in brief
    assert "`src/lib.py`" in brief
    assert "**Symbols** `lib`" in brief
    assert "the map renders" in brief
    assert "uv run pytest" in brief
    assert "`/etc/passwd`" in brief
    outline = brief.split("**Repo map**", 1)[1]
    assert "src/" in outline
    assert "lib.py" in outline
    assert ".secret" not in outline
    assert "passwd" not in outline
    start_session(ctx, StartSessionParams(project="mapped", reason=WHY))
    assert "## Touch points" not in engine.last_spec["prompt"]


# -- dispatch purposes and start provenance (WI-1323) -----------------------


def _last_started(ctx: AppContext) -> dict[str, Any]:
    started = [
        event
        for event in list_events(ctx, ListEventsParams()).events
        if event.kind == "session.started"
    ]
    return dict(started[-1].summary)


def test_purpose_implement_resolves_the_implement_profile(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", purpose="implement", task="fix it", reason=WHY
        ),
    )
    spec = engine.last_spec
    assert spec["template"] == "klaudia"
    assert spec["model"] == "grok-4.7"
    assert "effort" not in spec
    assert spec["budget"] == {"turns": 120, "wall_clock_minutes": 90}
    assert spec["autopilot"] is True
    summary = _last_started(wired)
    assert summary["purpose"] == "implement"
    assert summary["worktree"] is True
    assert summary["repo_map"] is True
    assert summary["overrides"] == []
    assert result.session.purpose == "implement"
    assert IMPLEMENT_FOOTER in spec["prompt"]


def test_only_an_implement_start_gets_the_stop_at_push_footer(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", template="claude", task="look", reason=WHY
        ),
    )
    assert IMPLEMENT_FOOTER not in engine.last_spec["prompt"]


def test_a_purpose_whose_model_the_project_forbids_is_refused(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1590: the review policy is a rule, not text in the brief. The
    implement profile runs grok-4.7, and a project that forbids it gets no
    session and no engine call."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(review=ReviewPolicy(never_models=["grok-4.7"])),
            reason=WHY,
        ),
    )
    with pytest.raises(PolicyRefused, match=r"grok-4\.7"):
        start_session(
            wired,
            StartSessionParams(
                work_item="WI-1", purpose="implement", task="fix it", reason=WHY
            ),
        )
    assert engine.sent == []


def test_naming_a_forbidden_model_is_refused_too(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The caller naming the model is the same start, so the same rule."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(review=ReviewPolicy(never_models=["grok-4.7"])),
            reason=WHY,
        ),
    )
    with pytest.raises(PolicyRefused, match="never_models"):
        start_session(
            wired,
            StartSessionParams(project="vogt", model="grok-4.7", reason=WHY),
        )
    assert engine.sent == []


def test_forbidden_text_refuses_a_model_never_models_does_not_list(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1590: `(?i)grok` is the project's rule even when never_models is
    empty, and it applies to the model the purpose filled in."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(forbidden_text=["(?i)grok"]),
            reason=WHY,
        ),
    )
    with pytest.raises(PolicyRefused, match="forbidden_text"):
        start_session(
            wired,
            StartSessionParams(
                work_item="WI-1", purpose="implement", task="fix it", reason=WHY
            ),
        )
    assert engine.sent == []


def test_forbidden_text_refuses_a_model_the_caller_names(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Purpose plus an explicit model is the same check: naming grok-4.8
    over the profile still matches `(?i)grok`."""
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                forbidden_text=["(?i)grok"],
                review=ReviewPolicy(never_models=["something-else"]),
            ),
            reason=WHY,
        ),
    )
    with pytest.raises(PolicyRefused, match="forbidden_text"):
        start_session(
            wired,
            StartSessionParams(
                work_item="WI-1",
                purpose="implement",
                model="grok-4.8",
                reason=WHY,
            ),
        )
    assert engine.sent == []


def test_claude_refuses_a_model_it_does_not_speak(
    wired: AppContext, engine: StandInEngine
) -> None:
    """`template=claude` with a non-Anthropic model id would launch
    `claude --model grok-4.7`. The template's own name resolves the same way
    the engine resolves a tag."""
    with pytest.raises(PolicyRefused, match="Claude Code"):
        start_session(
            wired,
            StartSessionParams(
                project="vogt", template="claude", model="grok-4.7", reason=WHY
            ),
        )
    with pytest.raises(PolicyRefused, match="Claude Code"):
        start_session(
            wired,
            StartSessionParams(
                project="vogt",
                template="Claude Code (protected)",
                model="grok-4.7",
                reason=WHY,
            ),
        )
    assert engine.sent == []


def test_claude_accepts_an_anthropic_model(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A claude model id, and the aliases Claude Code resolves itself, start."""
    start_session(
        wired,
        StartSessionParams(
            project="vogt", template="claude", model="claude-opus-4-5", reason=WHY
        ),
    )
    assert engine.last_spec["model"] == "claude-opus-4-5"
    start_session(
        wired,
        StartSessionParams(project="vogt", template="claude", model="opus", reason=WHY),
    )
    assert engine.last_spec["model"] == "opus"


def test_klaudia_may_run_a_model_claude_refuses(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The refusal is about Claude Code, not about the model. The implement
    profile's own pairing — klaudia with grok-4.7 — keeps starting."""
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", purpose="implement", task="fix it", reason=WHY
        ),
    )
    assert engine.last_spec["template"] == "klaudia"
    assert engine.last_spec["model"] == "grok-4.7"


def test_a_claude_model_on_the_implement_profile_runs_claude_code(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1852: `purpose=implement, model=opus` kept the profile's klaudia
    template, and klaudia answered 400 "Invalid model name" on every turn.
    The caller named a Claude model, so the agent follows it."""
    result = start_session(
        wired,
        StartSessionParams(
            work_item="WI-1",
            purpose="implement",
            model="opus",
            task="fix it",
            reason=WHY,
        ),
    )
    assert engine.last_spec["template"] == "claude"
    assert engine.last_spec["model"] == "opus"
    assert result.session.template == "claude"
    # The caller's template still wins when it names one Claude Code is.
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1",
            purpose="implement",
            model="claude-opus-5-5",
            task="fix it",
            reason=WHY,
        ),
    )
    assert engine.last_spec["template"] == "claude"


def test_a_claude_alias_on_another_agent_template_is_refused(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1852: a bare alias only Claude Code resolves is refused up front
    on klaudia, codex or opencode, rather than failing every turn."""
    for template in ("klaudia", "codex", "opencode"):
        with pytest.raises(PolicyRefused, match="does not resolve"):
            start_session(
                wired,
                StartSessionParams(
                    project="vogt", template=template, model="opus", reason=WHY
                ),
            )
    with pytest.raises(PolicyRefused, match="does not resolve"):
        start_session(
            wired,
            StartSessionParams(
                work_item="WI-1",
                purpose="implement",
                template="klaudia",
                model="sonnet",
                reason=WHY,
            ),
        )
    assert engine.sent == []


def test_a_model_the_project_does_not_forbid_still_starts(
    wired: AppContext, engine: StandInEngine
) -> None:
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(review=ReviewPolicy(never_models=["grok-4.7"])),
            reason=WHY,
        ),
    )
    start_session(
        wired,
        StartSessionParams(work_item="WI-1", purpose="review", reason=WHY),
    )
    assert engine.last_spec["model"] == "opus"


def test_a_project_with_no_review_policy_refuses_nothing(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", purpose="implement", task="fix it", reason=WHY
        ),
    )
    assert engine.last_spec["model"] == "grok-4.7"


def test_purpose_review_resolves_opus_low_forty_turns_read_only(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="review", reason=WHY)
    )
    spec = engine.last_spec
    assert (spec["template"], spec["model"], spec["effort"]) == (
        "claude",
        "opus",
        "low",
    )
    assert spec["budget"] == {"turns": 40}
    assert "autopilot" not in spec
    assert "## Read only" in spec["prompt"]


def test_an_explicit_param_wins_over_the_profile_and_is_audited(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", purpose="implement", model="grok-4.8", reason=WHY
        ),
    )
    assert engine.last_spec["model"] == "grok-4.8"
    assert engine.last_spec["template"] == "klaudia"
    assert _last_started(wired)["overrides"] == ["model"]


def test_no_purpose_leaves_a_start_as_it_was(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "model" not in engine.last_spec
    assert result.session.purpose is None
    assert _last_started(wired)["overrides"] == []


def test_the_deployment_profile_preference_overrides_one_field(
    wired: AppContext, engine: StandInEngine
) -> None:
    from vogt.application.models import PreferenceSetParams
    from vogt.application.services import set_preference

    set_preference(
        wired,
        PreferenceSetParams(
            key="dispatch.profiles",
            value={"implement": {"model": "grok-4.8"}},
            reason=WHY,
        ),
    )
    start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="implement", reason=WHY)
    )
    spec = engine.last_spec
    assert spec["model"] == "grok-4.8"
    assert spec["template"] == "klaudia"
    assert spec["budget"] == {"turns": 120, "wall_clock_minutes": 90}


def test_an_agent_cannot_set_the_dispatch_table(wired: AppContext) -> None:
    from vogt.application.models import PreferenceSetParams
    from vogt.application.services import set_preference
    from vogt.errors import PersonRequired

    with pytest.raises(PersonRequired):
        set_preference(
            _as(wired, f"agent:engine:{ENGINE_SESSION}"),
            PreferenceSetParams(
                key="dispatch.profiles",
                value={"review": {"model": "grok-4.7"}},
                reason=WHY,
            ),
        )


def test_a_profile_cannot_grant_the_oversight_role(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Resolved after the role guard, and no profile field carries a role."""
    from vogt.application.models import PreferenceSetParams
    from vogt.application.services import set_preference
    from vogt.errors import InvalidPreference

    with pytest.raises(InvalidPreference, match="role"):
        set_preference(
            wired,
            PreferenceSetParams(
                key="dispatch.profiles",
                value={"implement": {"role": "oversight"}},
                reason=WHY,
            ),
        )
    agent = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    with pytest.raises(RoleRefused):
        start_session(
            agent,
            StartSessionParams(
                work_item="WI-1", purpose="implement", role="oversight", reason=WHY
            ),
        )
    start_session(
        agent, StartSessionParams(work_item="WI-1", purpose="implement", reason=WHY)
    )
    assert "role" not in engine.last_spec


def test_a_person_start_records_person_and_no_session(
    wired: AppContext,
) -> None:
    result = start_session(
        wired, StartSessionParams(work_item="WI-1", purpose="review", reason=WHY)
    )
    assert result.session.started_by_kind == "person"
    assert result.session.started_by_session is None
    [listed] = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    assert (listed.purpose, listed.started_by_kind) == ("review", "person")


@pytest.mark.parametrize("role", ["oversight", "worker"])
def test_a_session_start_records_the_starters_engine_role(
    wired: AppContext, engine: StandInEngine, role: str
) -> None:
    engine.roles[ENGINE_SESSION] = role
    result = start_session(
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        StartSessionParams(work_item="WI-1", purpose="mechanical", reason=WHY),
    )
    assert result.session.started_by_session == ENGINE_SESSION
    assert result.session.started_by_kind == role
    summary = _last_started(wired)
    assert summary["started_by_kind"] == role
    assert summary["started_by_session"] == ENGINE_SESSION


def test_a_resumed_oversight_on_the_pod_token_keeps_its_starts(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1852: oversight resumed by hand runs on the pod token, so its
    starts had no starter and its ownership filter lost them. The bridge's
    claimed engine session counts once the engine vouches it is oversight."""
    pod = dataclasses.replace(
        _as(wired, "agent:pod:vogt-prod"), claimed_engine_session=ENGINE_SESSION
    )
    engine.roles[ENGINE_SESSION] = "oversight"
    result = start_session(pod, StartSessionParams(work_item="WI-1", reason=WHY))
    assert result.session.started_by_session == ENGINE_SESSION
    assert result.session.started_by_kind == "oversight"
    owned = list_sessions(
        wired, ListSessionsParams(mode="full", started_by_session=ENGINE_SESSION)
    ).sessions
    assert [row.id for row in owned] == [result.session.id]


@pytest.mark.parametrize(
    ("actor", "role"),
    [
        # A worker's claim is not oversight's.
        ("agent:pod:vogt-prod", "worker"),
        # Only the pod token may claim; any other agent has its own identity
        # or none, and naming a session does not lend it one.
        ("agent:someone-else", "oversight"),
    ],
)
def test_a_claimed_engine_session_is_ignored_unless_vouched_for(
    wired: AppContext, engine: StandInEngine, actor: str, role: str
) -> None:
    engine.roles[ENGINE_SESSION] = role
    ctx = dataclasses.replace(_as(wired, actor), claimed_engine_session=ENGINE_SESSION)
    result = start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    assert result.session.started_by_session is None
    assert result.session.started_by_kind is None


def test_an_unknown_claimed_engine_session_is_ignored(
    wired: AppContext, engine: StandInEngine
) -> None:
    ctx = dataclasses.replace(
        _as(wired, "agent:pod:vogt-prod"),
        claimed_engine_session="11111111-2222-4333-8444-555555555555",
    )
    result = start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    assert result.session.started_by_session is None


def test_a_vogt_started_session_is_named_by_its_engine_id(
    wired: AppContext, engine: StandInEngine
) -> None:
    parent = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    child = start_session(
        _as(wired, f"agent:session:{parent.session.id}"),
        StartSessionParams(work_item="WI-1", reason=WHY),
    )
    assert child.session.started_by_session == parent.session.engine_session_id
    assert child.session.started_by_kind == "worker"


def test_session_list_filters_by_who_started_it(wired: AppContext) -> None:
    parent = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    child = start_session(
        _as(wired, f"agent:session:{parent.session.id}"),
        StartSessionParams(work_item="WI-1", reason=WHY),
    )
    other = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    by_engine = list_sessions(
        wired,
        ListSessionsParams(
            mode="full", started_by_session=parent.session.engine_session_id
        ),
    )
    by_vogt = list_sessions(
        wired, ListSessionsParams(mode="full", started_by_session=parent.session.id)
    )
    assert [row.id for row in by_engine.sessions] == [child.session.id]
    assert [row.id for row in by_vogt.sessions] == [child.session.id]
    assert other.session.id not in {row.id for row in by_engine.sessions}


def test_session_list_fields_accept_a_comma_separated_string() -> None:
    params = ListSessionsParams(fields="account,model")  # type: ignore[arg-type]
    assert params.fields == ["account", "model"]


def test_a_purpose_outside_the_six_is_refused() -> None:
    with pytest.raises(ValidationError):
        StartSessionParams(work_item="WI-1", purpose="anything", reason=WHY)  # type: ignore[arg-type]


def _push_only_landing() -> AgentPolicy:
    from vogt.core.entities import LandingPolicy

    return AgentPolicy(
        landing=LandingPolicy(
            base_branch="main",
            worker_lands_by="push_branch",
            merge_by="oversight",
            merge_mode="rebase",
        )
    )


def test_a_task_asking_for_a_pr_on_a_push_only_project_is_overridden(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1832: the policy wins, the brief says so, the start result shows it."""
    update_project(
        wired,
        UpdateProjectParams(slug="vogt", agent_policy=_push_only_landing(), reason=WHY),
    )
    item = native_work_item(wired, title="land me", project="vogt")
    result = start_session(
        wired,
        StartSessionParams(
            work_item=item.ref,
            reason=WHY,
            template="claude",
            task="Fix it, then open a PR against main.",
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "## Policy overrides task" in brief
    assert "Policy overrides task: push only." in brief
    assert brief.index("## Policy overrides task") < brief.index("## Task")
    policy = result.effective_policy
    assert policy is not None
    assert policy.lands_by == "push_branch"
    assert policy.base_branch == "main"
    assert len(policy.overrides) == 1
    assert policy.overrides[0].startswith("Policy overrides task: push only.")


def test_a_task_that_agrees_with_the_policy_adds_no_override(
    wired: AppContext, engine: StandInEngine
) -> None:
    update_project(
        wired,
        UpdateProjectParams(slug="vogt", agent_policy=_push_only_landing(), reason=WHY),
    )
    item = native_work_item(wired, title="land me", project="vogt")
    result = start_session(
        wired,
        StartSessionParams(
            work_item=item.ref,
            reason=WHY,
            template="claude",
            task="Open NO PR, push the branch only. Never merge or deploy.",
        ),
    )
    assert "Policy overrides task" not in engine.last_spec["prompt"]
    assert result.effective_policy is not None
    assert result.effective_policy.overrides == []


def _rust_core_landing(**landing: Any) -> AgentPolicy:
    from vogt.core.entities import LandingPolicy

    return AgentPolicy(
        base_branch="main",
        landing=LandingPolicy(
            base_branch="main",
            worker_lands_by="push_branch",
            merge_by="oversight",
            merge_mode="rebase",
            **landing,
        ),
    )


def test_an_initiative_base_replaces_the_project_base(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1841: the Rust port's items cut from and land onto `rust-core`."""
    from vogt.application.models import CreateInitiativeParams
    from vogt.application.services import create_initiative

    create_initiative(wired, CreateInitiativeParams(title="Rust port", reason=WHY))
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=_rust_core_landing(
                initiative_bases={"rust-port": "rust-core"}
            ),
            reason=WHY,
        ),
    )
    item = native_work_item(
        wired, title="port it", project="vogt", initiative="rust-port"
    )
    result = start_session(
        wired,
        StartSessionParams(
            work_item=item.ref,
            reason=WHY,
            worktree=True,
            template="claude",
            task="Cut a branch from rust-core and land it onto rust-core.",
        ),
    )
    brief = engine.last_spec["prompt"]
    assert "Policy overrides task" not in brief
    assert "**Base** `rust-core`" in brief
    assert engine.last_spec["worktree"]["base_ref"] == "rust-core"
    policy = result.effective_policy
    assert policy is not None
    assert policy.base_branch == "rust-core"
    assert policy.overrides == []


def test_a_task_naming_a_known_long_lived_branch_is_taken_at_its_word(
    wired: AppContext, engine: StandInEngine
) -> None:
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=_rust_core_landing(long_lived_branches=["rust-core"]),
            reason=WHY,
        ),
    )
    item = native_work_item(wired, title="port it", project="vogt")
    result = start_session(
        wired,
        StartSessionParams(
            work_item=item.ref,
            reason=WHY,
            template="claude",
            task="Branch off rust-core and fix it.",
        ),
    )
    assert result.effective_policy is not None
    assert result.effective_policy.base_branch == "rust-core"
    assert result.effective_policy.overrides == []
    assert "**Base** `rust-core`" in engine.last_spec["prompt"]


def test_an_unknown_base_in_a_task_is_still_overridden(
    wired: AppContext, engine: StandInEngine
) -> None:
    update_project(
        wired,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=_rust_core_landing(long_lived_branches=["rust-core"]),
            reason=WHY,
        ),
    )
    item = native_work_item(wired, title="port it", project="vogt")
    result = start_session(
        wired,
        StartSessionParams(
            work_item=item.ref,
            reason=WHY,
            template="claude",
            task="Branch off develop and fix it.",
        ),
    )
    assert result.effective_policy is not None
    assert result.effective_policy.base_branch == "main"
    assert any("base `main`" in line for line in result.effective_policy.overrides)


@pytest.mark.parametrize(
    ("role", "purpose", "overridden"),
    [
        ("oversight", "mechanical", False),
        ("oversight", "implement", True),
        ("worker", "mechanical", True),
    ],
)
def test_oversight_landing_work_is_not_told_not_to_merge(
    wired: AppContext,
    engine: StandInEngine,
    role: str,
    purpose: Literal["mechanical", "implement"],
    overridden: bool,
) -> None:
    """WI-1841: where oversight merges, its own landing task keeps its merge."""
    update_project(
        wired,
        UpdateProjectParams(slug="vogt", agent_policy=_push_only_landing(), reason=WHY),
    )
    item = native_work_item(wired, title="land me", project="vogt")
    engine.roles[ENGINE_SESSION] = role
    result = start_session(
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        StartSessionParams(
            work_item=item.ref,
            purpose=purpose,
            reason=WHY,
            task="Rebase the branch, then merge it into main.",
        ),
    )
    assert result.effective_policy is not None
    merged = any("do not merge" in line for line in result.effective_policy.overrides)
    assert merged is overridden


# -- dispatch enforcement (WI-1329) ---------------------------------------


def _enforce(ctx: AppContext, **scalars: object) -> None:
    from vogt.application.models import PreferenceSetParams
    from vogt.application.services import set_preference

    for key, value in {"enforce": True, **scalars}.items():
        set_preference(
            ctx,
            PreferenceSetParams(
                key=f"dispatch.{key}", value={"value": value}, reason=WHY
            ),
        )


def test_dispatch_is_not_enforced_by_default_but_noted(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.roles[ENGINE_SESSION] = "worker"
    start_session(
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        StartSessionParams(work_item="WI-1", purpose="review", reason=WHY),
    )
    notes = _last_started(wired)["dispatch_notes"]
    assert any("review_by_oversight_only" in note for note in notes)


def test_a_worker_cannot_start_a_reviewer(
    wired: AppContext, engine: StandInEngine
) -> None:
    _enforce(wired)
    engine.roles[ENGINE_SESSION] = "worker"
    posts = sum(row["method"] == "POST" for row in engine.sent)
    with pytest.raises(InvalidRequest, match="review_by_oversight_only"):
        start_session(
            _as(wired, f"agent:engine:{ENGINE_SESSION}"),
            StartSessionParams(work_item="WI-1", purpose="review", reason=WHY),
        )
    # Refused before the engine is asked: no terminal is started.
    assert sum(row["method"] == "POST" for row in engine.sent) == posts


def test_an_agent_is_refused_the_wrong_model_and_a_person_is_noted(
    wired: AppContext, engine: StandInEngine
) -> None:
    _enforce(wired)
    engine.roles[ENGINE_SESSION] = "oversight"
    params = StartSessionParams(
        work_item="WI-1", purpose="implement", model="grok-9", reason=WHY
    )
    with pytest.raises(InvalidRequest, match=r"profile_model|grok-4\.7"):
        start_session(_as(wired, f"agent:engine:{ENGINE_SESSION}"), params)
    start_session(wired, params)
    summary = _last_started(wired)
    assert summary["started_by_kind"] == "person"
    assert any("grok-9" in note for note in summary["dispatch_notes"])


@pytest.mark.parametrize(
    ("purpose", "model", "expected"),
    [
        (None, "grok-9", "implement"),
        ("implement-careful", "sonnet", "implement-careful"),
    ],
)
def test_the_model_rule_holds_without_a_purpose_and_for_careful(
    wired: AppContext,
    engine: StandInEngine,
    purpose: str | None,
    model: str,
    expected: str,
) -> None:
    _enforce(wired)
    engine.roles[ENGINE_SESSION] = "oversight"
    with pytest.raises(InvalidRequest, match=f"purpose={expected}"):
        start_session(
            _as(wired, f"agent:engine:{ENGINE_SESSION}"),
            StartSessionParams(
                work_item="WI-1",
                purpose=purpose,  # type: ignore[arg-type]
                model=model,
                reason=WHY,
            ),
        )


def test_the_sixth_worker_from_one_overseer_is_refused(
    wired: AppContext, engine: StandInEngine
) -> None:
    _enforce(wired)
    engine.roles[ENGINE_SESSION] = "oversight"
    overseer = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    for _ in range(5):
        start_session(overseer, StartSessionParams(work_item="WI-1", reason=WHY))
    with pytest.raises(InvalidRequest, match="max_workers_per_overseer=5 reached"):
        start_session(overseer, StartSessionParams(work_item="WI-1", reason=WHY))


def test_an_override_from_a_worker_is_refused(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.roles[ENGINE_SESSION] = "worker"
    with pytest.raises(InvalidRequest, match="only from an oversight session"):
        start_session(
            _as(wired, f"agent:engine:{ENGINE_SESSION}"),
            StartSessionParams(work_item="WI-1", override="need one", reason=WHY),
        )


def test_an_override_from_oversight_is_audited_with_an_inbox_row(
    wired: AppContext, engine: StandInEngine
) -> None:
    from vogt.application.models import InboxListParams
    from vogt.application.services import list_inbox

    _enforce(wired, max_workers_per_overseer=1)
    engine.roles[ENGINE_SESSION] = "oversight"
    overseer = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    start_session(overseer, StartSessionParams(work_item="WI-1", reason=WHY))
    result = start_session(
        overseer,
        StartSessionParams(work_item="WI-1", override="urgent prod fix", reason=WHY),
    )
    assert _last_started(wired)["override_reason"] == "urgent prod fix"
    with wired.declared.read() as view:
        [record] = view.list_audit(operation="dispatch.override", limit=10)
    assert record.reason == "urgent prod fix"
    assert record.entity_id == result.session.id
    entries = list_inbox(wired, InboxListParams()).entries
    [row] = [e for e in entries if e.kind == "dispatch.override"]
    assert row.summary == "urgent prod fix"


@pytest.mark.parametrize("purpose", ["implement", None])
def test_an_item_that_failed_review_escalates_to_implement_careful(
    wired: AppContext, engine: StandInEngine, purpose: str | None
) -> None:
    from vogt.application.models import CommentParams
    from vogt.application.services import comment_work
    from vogt.core.entities import VerdictFields

    _enforce(wired)
    for _ in range(2):
        comment_work(
            wired,
            CommentParams(
                ref="WI-1",
                body="blocked",
                kind="verdict",
                verdict=VerdictFields(
                    head_sha="abc", verdict="blockers", blockers=["x"]
                ),
                reason=WHY,
            ),
        )
    engine.roles[ENGINE_SESSION] = "oversight"
    result = start_session(
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        StartSessionParams(work_item="WI-1", purpose=purpose, reason=WHY),  # type: ignore[arg-type]
    )
    assert result.escalated_to == "implement-careful"
    assert engine.last_spec["model"] == "opus"


def test_a_session_start_names_its_supervisor_to_the_engine_wi1521(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1521: the starter is sent as the engine's `supervisor` and shown on
    the digest; a person's start sends none, and no `wake_on` by default."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert "supervisor" not in engine.last_spec
    assert "wake_on" not in engine.last_spec
    engine.roles[ENGINE_SESSION] = "oversight"
    start_session(
        _as(wired, f"agent:engine:{ENGINE_SESSION}"),
        StartSessionParams(work_item="WI-1", purpose="mechanical", reason=WHY),
    )
    assert engine.last_spec["supervisor"] == ENGINE_SESSION
    from vogt.application.services.sessions import _digest_of

    [full] = list_sessions(
        wired, ListSessionsParams(mode="full", started_by_session=ENGINE_SESSION)
    ).sessions
    assert _digest_of(full).supervisor == ENGINE_SESSION


def test_wake_on_is_sent_to_the_engine_and_checked_wi1521(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(
        wired,
        StartSessionParams(
            work_item="WI-1", role="oversight", wake_on=["done", "blocked"], reason=WHY
        ),
    )
    assert engine.last_spec["wake_on"] == ["done", "blocked"]
    with pytest.raises(ValueError):
        StartSessionParams(work_item="WI-1", wake_on=["ready"], reason=WHY)  # type: ignore[list-item]


def test_supervised_by_narrows_changes_and_wait_any_to_the_starters_sessions_wi1521(
    wired: AppContext, engine: StandInEngine
) -> None:
    """WI-1521: `supervised_by` (an id, or `me` for a session caller) becomes
    the engine ids that starter started, intersected with `ids`; a filter
    nothing matches sends the nil id rather than reading every session."""
    from vogt.application.services.sessions import _engine_ids

    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.roles[ENGINE_SESSION] = "oversight"
    caller = _as(wired, f"agent:engine:{ENGINE_SESSION}")
    child = start_session(
        caller,
        StartSessionParams(work_item="WI-1", purpose="mechanical", reason=WHY),
    ).session
    assert _engine_ids(wired, [], ENGINE_SESSION) == [child.engine_session_id]
    assert _engine_ids(caller, [], "me") == [child.engine_session_id]
    assert _engine_ids(wired, [child.id], ENGINE_SESSION) == [child.engine_session_id]
    nil = "00000000-0000-0000-0000-000000000000"
    assert _engine_ids(wired, [], "11111111-2222-4333-8444-555566667777") == [nil]
    assert _engine_ids(wired, [], None) is None
    with pytest.raises(InvalidRequest, match="supervised_by=me"):
        _engine_ids(wired, [], "me")
