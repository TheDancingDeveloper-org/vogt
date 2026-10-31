"""project.brief digest: a derived status digest, computed on read (WI-2747)."""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    CreateInitiativeParams,
    CreateWorkParams,
    ProjectBriefParams,
    TransitionWorkParams,
)
from vogt.application.services import (
    brief_project,
    create_initiative,
    create_work,
    report_session,
    transition_work,
)

from tests.test_session_reports import (
    WHY,
    _as_session,
    _result,
    _start,
    engine,
    wired,
)

__all__ = ["engine", "wired"]


def test_an_empty_project_has_an_empty_digest(wired: AppContext) -> None:
    digest = brief_project(wired, ProjectBriefParams(slug="vogt")).digest

    assert digest.initiatives == []
    assert digest.in_progress == []
    assert digest.recent_results == []
    assert digest.last_merge is None
    assert digest.last_deploy is None


def test_the_digest_ranks_initiatives_shows_holders_and_results(
    wired: AppContext,
) -> None:
    for title, weight in (("Light", 5), ("Heavy", 40)):
        create_initiative(
            wired, CreateInitiativeParams(title=title, weight=weight, reason=WHY)
        )
    for slug in ("light", "heavy"):
        create_work(
            wired,
            CreateWorkParams(
                kind="feature",
                title=f"Under {slug}",
                project="vogt",
                initiative=slug,
                local_only=True,
                reason=WHY,
            ),
        )
    transition_work(
        wired, TransitionWorkParams(ref="WI-1", to_state="in_progress", reason=WHY)
    )
    session_id, _ = _start(wired)
    for n in range(5):
        report_session(
            _as_session(wired, session_id),
            _result(summary=f"result {n}\nsecond line", idempotency_key=f"k{n}"),
        )

    digest = brief_project(wired, ProjectBriefParams(slug="vogt")).digest

    assert [line.split(" ")[0] for line in digest.initiatives] == ["heavy", "light"]
    assert digest.in_progress == [f"WI-1 [session {session_id}]: Reports"]
    assert len(digest.recent_results) == 3, "capped at three"
    assert all("\n" not in line for line in digest.recent_results)
    assert digest.recent_results[0].endswith("WI-1: result 4 second line")


def test_digest_lines_are_bounded(wired: AppContext) -> None:
    session_id, _ = _start(wired)
    report_session(_as_session(wired, session_id), _result(summary="x" * 1500))

    digest = brief_project(wired, ProjectBriefParams(slug="vogt")).digest

    assert len(digest.recent_results[0]) <= 160
