#!/usr/bin/env python3
"""Record the session brief goldens from Python's `_brief.py`.

The Rust core's brief (`engine/core/src/application/brief.rs`) is checked
byte for byte against these files. Run from the repository root:

    uv run python scripts/gen_brief_golden.py

Writes `engine/core/src/application/brief_golden/`. The scenarios below are
mirrored, by hand, in the Rust tests of `brief.rs`: change one side and the
other must change with it, or the golden test fails.
"""

from __future__ import annotations

import sys
from datetime import UTC, datetime
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "src"))

from vogt.application.models import ContributionView, WhyResult  # noqa: E402
from vogt.application.services import _brief  # noqa: E402
from vogt.application.services._brief import ComposedBrief  # noqa: E402
from vogt.application.services.sessions import READ_ONLY  # noqa: E402
from vogt.core.entities import (  # noqa: E402
    AgentPolicy,
    Comment,
    HandoffFields,
    LandingPolicy,
    Project,
    Relation,
    WorkItem,
)

OUT = REPO_ROOT / "engine" / "core" / "src" / "application" / "brief_golden"
NOW = datetime(2026, 10, 17, 9, 0, tzinfo=UTC)
SESSION = "ses_brief01"


class FakeView:
    def __init__(self, items: list[WorkItem], comments: list[Comment]) -> None:
        self._items = {item.id: item for item in items}
        self._comments = comments

    def work_item_by_id(self, work_item_id: str) -> WorkItem | None:
        return self._items.get(work_item_id)

    def comments_for(
        self, work_item_id: str, *, limit: int, order: str = "oldest"
    ) -> list[Comment]:
        # The scenarios list comments newest first, as the store returns them.
        return [c for c in self._comments if c.work_item_id == work_item_id][:limit]


def item(**fields: object) -> WorkItem:
    base: dict[str, object] = {
        "id": "wrk_1058",
        "ref": "WI-1058",
        "kind": "feature",
        "title": "Port the brief",
        "body": "",
        "state": "in_progress",
        "priority": "p1",
        "project_id": "prj_vogt",
        "project_slug": "vogt",
        "created_at": NOW,
        "updated_at": NOW,
    }
    base.update(fields)
    return WorkItem(**base)


def comment(
    cid: str,
    body: str,
    kind: str = "note",
    handoff: HandoffFields | None = None,
    item_id: str = "wrk_1058",
) -> Comment:
    return Comment(
        id=cid,
        work_item_id=item_id,
        actor_id="act_1",
        actor_display_name="Session ses_a",
        body=body,
        kind=kind,
        handoff=handoff,
        created_at=NOW,
    )


def project(slug: str, policy: AgentPolicy) -> Project:
    return Project(
        id=f"prj_{slug}",
        slug=slug,
        name=slug.title(),
        root_path=f"/work/{slug}",
        agent_policy=policy,
        created_at=NOW,
        updated_at=NOW,
    )


ITEM_FULL = item(
    body="Port the brief.\n\nKeep it exact.",
    effort="l",
    labels=["rust", "brief"],
    relations=[
        Relation(
            kind="depends_on",
            related_id="wrk_1053",
            related_ref="WI-1053",
            related_title="Pure domain III",
            related_state="done",
        ),
        Relation(
            kind="relates_to",
            related_id="wrk_gone",
            related_ref="WI-9",
            related_title="Gone",
            related_state="open",
        ),
    ],
)
RELATED_FULL = [
    item(id="wrk_1053", ref="WI-1053", title="Pure domain III", state="done")
]
RANKING_FULL = WhyResult(
    ref="WI-1058",
    title="Port the brief",
    total=84.4969,
    contributions=[
        ContributionView(
            input="priority", detail="p1", value=1, weight=55, contribution=55
        ),
        ContributionView(
            input="blocking_fan_out",
            detail="2 item(s) depend on this one",
            value=2,
            weight=8,
            contribution=16,
        ),
        ContributionView(
            input="staleness",
            detail="8.0 days since last change",
            value=7.994,
            weight=0.5,
            contribution=3.9969,
        ),
        ContributionView(
            input="trust_penalty",
            detail="trust state unverified",
            value=1,
            weight=-3,
            contribution=-3,
        ),
    ],
    inputs_not_yet_available={
        "ci_red_boost": "arrives with the CI check observations it reads"
    },
)
HANDOFF_FULL = HandoffFields(
    branch="wi-1058-brief",
    head_sha="abc1234",
    base="rust-core",
    session="ses_a",
    done=["policy section"],
    remaining=["review brief"],
    touch_points=["brief.rs"],
    gates_run=["cargo test"],
    blockers=["none yet"],
)
COMMENTS_FULL = [
    comment("cmt_3", "Remark after the hand-off."),
    comment(
        "cmt_2",
        "Policy section ported; review brief next.",
        kind="handoff",
        handoff=HANDOFF_FULL,
    ),
    comment("cmt_1", "Remark before the hand-off."),
]
POLICY_FULL = AgentPolicy(
    forbidden_text=["secret-[0-9]+"],
    merge_deploys_prod=True,
    main_branches=["main"],
    commit_identity="sprooty <sprooty@sprooty.com>",
    test_recipe="cargo test -p vogt-core",
    deployed_setup="Engine and PWA run as separate services.",
    rules=["Keep the lockfile."],
    landing=LandingPolicy(
        base_branch="rust-core",
        worker_lands_by="push_branch",
        merge_by="oversight",
        merge_mode="rebase",
    ),
)
PROJECT_FULL = project("vogt", POLICY_FULL)
PROJECT_BARE = project("vogt", AgentPolicy())
PLAIN = item(id="wrk_2", ref="WI-2", kind="bug", title="Plain", state="open")


def write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8", newline="\n")


def scenario(name: str, composed: ComposedBrief) -> None:
    write(OUT / f"{name}.md", composed.text)
    write(OUT / f"{name}.companion.md", composed.companion)


def main() -> None:
    # The constants the Rust brief includes verbatim.
    text = OUT / "text"
    write(text / "deployment_policy.md", _brief._DEPLOYMENT_POLICY)
    write(text / "driving_other_sessions.md", _brief.DRIVING_OTHER_SESSIONS)
    write(text / "when_blocked.md", _brief.WHEN_BLOCKED)
    write(text / "autopilot.md", _brief.AUTOPILOT)
    write(text / "read_only.md", READ_ONLY)
    write(text / "review.md", _brief._REVIEW)
    write(text / "review_verdict.md", _brief._REVIEW_VERDICT)

    full = FakeView([*RELATED_FULL, ITEM_FULL], COMMENTS_FULL)
    scenario(
        "worker_full",
        _brief.brief_for_work_item(
            full, ITEM_FULL, SESSION, RANKING_FULL, project=PROJECT_FULL
        ),
    )
    scenario(
        "review_full",
        _brief.review_brief_for_work_item(
            full, ITEM_FULL, SESSION, project=PROJECT_FULL
        ),
    )

    plain = FakeView([PLAIN], [])
    scenario("worker_plain", _brief.brief_for_work_item(plain, PLAIN, SESSION))
    scenario(
        "review_none",
        _brief.review_brief_for_work_item(plain, PLAIN, SESSION),
    )

    branch_only = FakeView(
        [PLAIN],
        [
            comment(
                "cmt_4",
                "",
                kind="handoff",
                handoff=HandoffFields(branch="wi-2-x", done=["a"], blockers=["b"]),
                item_id="wrk_2",
            )
        ],
    )
    scenario(
        "review_no_base",
        _brief.review_brief_for_work_item(branch_only, PLAIN, SESSION),
    )

    no_head = FakeView(
        [PLAIN],
        [
            comment(
                "cmt_5",
                "Ported the first half.",
                kind="handoff",
                handoff=HandoffFields(
                    branch="wi-2-x", base="main", gates_run=["pytest"]
                ),
                item_id="wrk_2",
            )
        ],
    )
    scenario(
        "review_no_head",
        _brief.review_brief_for_work_item(
            no_head, PLAIN, SESSION, project=PROJECT_BARE
        ),
    )

    scenario(
        "project_full",
        _brief.brief_for_project(plain, "vogt", SESSION, project=PROJECT_FULL),
    )
    scenario(
        "project_bare",
        _brief.brief_for_project(plain, "vogt", SESSION, project=PROJECT_BARE),
    )
    print(f"wrote goldens under {OUT.relative_to(REPO_ROOT)}")


if __name__ == "__main__":
    main()
