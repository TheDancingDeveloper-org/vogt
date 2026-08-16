"""The GitHub module: consolidation, forge drift, and write-back (FR-B1–B5).

Includes the M5 demo. From `ROADMAP.md`:

    Enable the module for the rustnzb org and verify — via the GitHub audit
    log and `updated_at` timestamps — that onboarding changed nothing
    upstream, while every existing issue and PR appears in the global views
    with labels intact. Then close an issue on GitHub that a declared item
    links → drift proposal; accept → the item closes with provenance.

"Changed nothing upstream" is asserted here by recording every HTTP request
the adapter makes and failing on any method that is not a GET. That is a
stronger check than reading an audit log afterwards: it cannot pass by
accident, and it fails at the moment somebody adds a mutation.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.github.writeback import PERMITTED, permits
from vogt.application.context import AppContext
from vogt.application.models import (
    AdoptParams,
    BacklogParams,
    BugsParams,
    CommentParams,
    DriftDetectParams,
    DriftListParams,
    DriftResolveParams,
    GetWorkParams,
    OnboardParams,
    ProjectBriefParams,
    RegisterProjectParams,
    SetWriteBackParams,
    SweepParams,
    TransitionWorkParams,
    WriteBackListParams,
)
from vogt.application.services import (
    adopt,
    backlog,
    brief_project,
    bugs,
    comment_work,
    detect_drift,
    get_work,
    list_drift,
    list_write_backs,
    onboard,
    register_project,
    resolve_drift,
    set_write_back,
    sweep,
    transition_work,
)
from vogt.core.drift import FORGE_STATE_MISMATCH, UPDATE_AUTOMATION_GAP

WHY = "forge module test"
REPO = "https://github.com/TheDancingDeveloper-org/rustnzb"


class Forge:
    """A fake GitHub that records every request, method included."""

    def __init__(self, *, issues: list[dict[str, Any]] | None = None) -> None:
        self.issues = issues if issues is not None else []
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
        if body:
            self.bodies.append(json.loads(body.decode("utf-8")))

        if method != "GET":
            return 200, json.dumps(
                {"number": 12, "html_url": f"{REPO}/issues/12"}
            ).encode("utf-8")
        if "/issues" in url and "/comments" not in url:
            return 200, json.dumps(self.issues).encode("utf-8")
        if "/labels" in url:
            return 200, json.dumps([{"name": "bug", "color": "d73a4a"}]).encode("utf-8")
        if "/releases" in url:
            return 200, b"[]"
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
        if "/pulls" in url:
            return 200, b"[]"
        if "/contents/" in url:
            return 404, b""
        if "/vulnerability-alerts" in url or "/automated-security-fixes" in url:
            return 404, b""
        return 200, b"[]"

    @property
    def mutations(self) -> list[tuple[str, str]]:
        return [(m, u) for m, u in self.requests if m != "GET"]


@pytest.fixture
def forge(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> Forge:
    """A registered project wired to a fake, request-recording GitHub."""
    fake = Forge(
        issues=[
            {
                "number": 12,
                "title": "Segment fetch retries forever",
                "state": "open",
                "labels": [{"name": "bug"}, {"name": "p1"}],
                "html_url": f"{REPO}/issues/12",
                "updated_at": "2026-08-01T00:00:00Z",
            },
            {
                "number": 5,
                "title": "Ancient closed thing",
                "state": "closed",
                "labels": [],
                "closed_at": "2025-01-01T00:00:00Z",
            },
        ]
    )
    # The adapter is "configured" by making its client factory answer,
    # which is what a token file does in production. Patching the factory
    # rather than the config keeps the fake transport in one place and
    # leaves the frozen context alone.
    from vogt.adapters.github.client import GitHubClient

    def configured(path: Any, *, transport: Any = None) -> GitHubClient:
        del path, transport
        return GitHubClient(token="ghp_fake", transport=fake)

    monkeypatch.setattr(GitHubClient, "from_token_file", staticmethod(configured))

    register_project(
        instance,
        RegisterProjectParams(
            name="rustnzb",
            root_path=str(tmp_path / "rustnzb"),
            repo_url=REPO,
            reason=WHY,
        ),
    )
    return fake


# -- consolidation is read-only (FR-B3) -----------------------------------


def test_onboarding_reads_history_and_mutates_nothing(
    instance: AppContext, forge: Forge
) -> None:
    result = onboard(instance, OnboardParams(project="rustnzb", reason=WHY))

    assert result.issues == 2, "open *and* closed — history is the point"
    assert result.labels == 1
    assert result.mutations == 0
    assert forge.mutations == [], (
        f"onboarding must change nothing upstream; it sent {forge.mutations}"
    )
    assert forge.bodies == [], "and it sent no request bodies at all"


def test_closed_history_does_not_flood_the_backlog(
    instance: AppContext, forge: Forge
) -> None:
    """Years of finished work must not land in the ranked view on day one."""
    onboard(instance, OnboardParams(project="rustnzb", reason=WHY))
    titles = [entry.title for entry in bugs(instance, BugsParams(limit=50)).items]
    assert any("Segment fetch retries" in title for title in titles)
    assert not any("Ancient closed thing" in title for title in titles)


def test_onboarding_without_the_adapter_says_not_collected(
    instance: AppContext, tmp_path: Path
) -> None:
    """No `forge` fixture here: the adapter is genuinely unconfigured."""
    register_project(
        instance,
        RegisterProjectParams(
            name="Unwired", root_path=str(tmp_path), repo_url=REPO, reason=WHY
        ),
    )
    result = onboard(instance, OnboardParams(project="unwired", reason=WHY))
    assert result.issues == 0
    assert result.detail is not None and "not collected" in result.detail


def test_a_consolidation_is_audited_with_the_reason_it_was_given(
    instance: AppContext, forge: Forge
) -> None:
    """FR-S1: the largest read of an import leaves a row saying who asked.

    It did not. `forge onboard` is mutating and reason-required, and
    it discarded the reason and wrote nothing — so a consolidation that ran and
    one that was never run were indistinguishable in `audit list`. That is not
    hypothetical: two of a five-project batch were missed for an hour because
    of it (`vogt-onboarding/reviews/pingrag.md`).
    """
    why = "Consolidate rustnzb's existing history as playbook phase 5."
    onboard(instance, OnboardParams(project="rustnzb", reason=why))

    with instance.declared.read() as view:
        rows = [r for r in view.list_audit(limit=100) if r.operation == "forge.onboard"]
    assert len(rows) == 1, "one run, one row"
    assert rows[0].reason == why, "the reason is stored, not merely demanded"
    assert rows[0].entity_kind == "project"


def test_a_consolidation_that_read_nothing_is_audited_too(
    instance: AppContext, tmp_path: Path
) -> None:
    """The un-run zero and the honest zero must differ *somewhere*.

    They still look alike in the result — that is `WI-9`'s problem, and it is
    fixed by the adapter declaring what it can read. What this pins is the
    other half: whichever zero it was, the run itself is on the record.
    """
    register_project(
        instance,
        RegisterProjectParams(
            name="Unwired", root_path=str(tmp_path), repo_url=REPO, reason=WHY
        ),
    )
    onboard(instance, OnboardParams(project="unwired", reason="Check upstream."))

    with instance.declared.read() as view:
        rows = [r for r in view.list_audit(limit=100) if r.operation == "forge.onboard"]
    assert [r.reason for r in rows] == ["Check upstream."]


def test_a_consolidated_project_does_not_brief_as_empty(
    instance: AppContext, forge: Forge
) -> None:
    """brief, backlog and bugs must agree about one project.

    They did not. `brief` read the declared store alone while `backlog` and
    `bugs` merged declared with observed, so a project whose entire backlog
    arrived through `forge onboard` reported `open_work: 0` with its issues
    ranked in `backlog --project` at the same moment. That is the first
    surface an import's owner reads, and it said the import had found nothing
    (`vogt-onboarding/reviews/pingrag.md`).
    """
    onboard(instance, OnboardParams(project="rustnzb", reason=WHY))

    brief = brief_project(instance, ProjectBriefParams(slug="rustnzb"))
    ranked = backlog(instance, BacklogParams(project="rustnzb", limit=50))

    assert ranked.total_considered > 0, "the fixture consolidates something"
    assert brief.open_work == ranked.total_considered
    assert brief.top_backlog, "and it is not an empty list next to a count"
    assert [entry.ref for entry in brief.top_backlog] == [
        entry.ref for entry in ranked.items[: len(brief.top_backlog)]
    ], "same ordering, not merely the same size"


def test_the_brief_says_which_population_it_counted(
    instance: AppContext, forge: Forge
) -> None:
    """A total that does not name its halves is not an answer.

    Nine observed and nothing declared, and nine declared and nothing
    observed, are different situations for an owner reading a brief — one is
    a repository nobody has triaged, the other is a project nobody has
    collected for.
    """
    before = brief_project(instance, ProjectBriefParams(slug="rustnzb"))
    assert (before.declared_work, before.observed_work) == (0, 0)

    onboard(instance, OnboardParams(project="rustnzb", reason=WHY))

    after = brief_project(instance, ProjectBriefParams(slug="rustnzb"))
    assert after.observed_work > 0, "consolidation is visible as observed"
    assert after.declared_work == 0, "and is not miscounted as declared"
    assert after.open_work == after.declared_work + after.observed_work


def test_by_state_stays_declared_only_and_says_so(
    instance: AppContext, forge: Forge
) -> None:
    """Observed subjects have no workflow state, so they cannot be counted here.

    Keeping `by_state` declared-only is deliberate rather than an oversight:
    giving an observed subject a state would imply it had been through a
    machine it has never entered (DESIGN §3.6).
    """
    onboard(instance, OnboardParams(project="rustnzb", reason=WHY))
    brief = brief_project(instance, ProjectBriefParams(slug="rustnzb"))
    assert brief.by_state == {}, "nothing declared, so nothing to break down"
    assert brief.open_work > 0, "which is not the same as nothing outstanding"


# -- write-back policy (FR-B1, B4) ----------------------------------------


def test_the_default_policy_is_none(instance: AppContext, forge: Forge) -> None:
    """A tool holding somebody's token speaks only where it was told to."""
    del forge
    from vogt.application.models import GetProjectParams
    from vogt.application.services import get_project

    project = get_project(instance, GetProjectParams(slug="rustnzb")).project
    assert project.write_back == "none"


@pytest.mark.parametrize(
    ("policy", "action", "allowed"),
    [
        ("none", "comment", False),
        ("none", "close", False),
        ("comment_only", "comment", True),
        ("comment_only", "close", False),
        ("comment_only", "create", False),
        ("full", "comment", True),
        ("full", "close", True),
        ("full", "create", True),
        ("full", "label", True),
    ],
)
def test_the_policy_table(policy: str, action: str, allowed: bool) -> None:
    assert permits(policy, action) is allowed


def test_there_is_no_destructive_capability_at_all() -> None:
    """FR-B4: not disabled, not gated — absent."""
    every_action = set().union(*PERMITTED.values())
    assert every_action == {"create", "comment", "label", "close", "reopen"}
    for forbidden in ("delete", "force", "rewrite", "purge", "merge"):
        assert forbidden not in every_action


def test_the_client_refuses_a_non_additive_method() -> None:
    from vogt.adapters.github.client import GitHubClient, GitHubUnavailable

    client = GitHubClient(token="x", transport=Forge())
    with pytest.raises(GitHubUnavailable, match="forward-only"):
        client.send("/repos/o/r/issues/1", {}, method="DELETE")


# -- write-back in practice (FR-B2, B5) -----------------------------------


def _adopted(instance: AppContext, forge: Forge) -> str:
    onboard(instance, OnboardParams(project="rustnzb", reason=WHY))
    adopted = adopt(
        instance,
        AdoptParams(
            subject="gh:TheDancingDeveloper-org/rustnzb#12", reason="taking this on"
        ),
    )
    forge.requests.clear()
    forge.bodies.clear()
    return adopted.item.ref


def test_a_comment_posts_upstream_under_comment_only(
    instance: AppContext, forge: Forge
) -> None:
    ref = _adopted(instance, forge)
    set_write_back(
        instance,
        SetWriteBackParams(project="rustnzb", policy="comment_only", reason=WHY),
    )

    result = comment_work(
        instance, CommentParams(ref=ref, body="Looking at this now", reason=WHY)
    )
    assert result.write_back == "succeeded"
    assert [m for m, _ in forge.mutations] == ["POST"]
    assert "Looking at this now" in forge.bodies[0]["body"]
    assert ref in forge.bodies[0]["body"], (
        "the upstream comment says where it came from"
    )


def test_inbound_comments_are_never_copied_into_ours(
    instance: AppContext, forge: Forge
) -> None:
    """FR-B5: comments flow outbound only.

    An inbound forge comment stays an observation shown against the linked
    item. Copying it into `comments` would need forge-author identity
    mapping and loop suppression to tell our own echo from somebody else's
    remark — and the observation view already answers the question.
    """
    ref = _adopted(instance, forge)
    work = get_work(instance, GetWorkParams(ref=ref))
    assert work.comments == [], "nothing upstream has been copied in"


def test_nothing_is_sent_under_the_default_policy(
    instance: AppContext, forge: Forge
) -> None:
    ref = _adopted(instance, forge)
    result = comment_work(
        instance, CommentParams(ref=ref, body="Local only", reason=WHY)
    )
    assert result.write_back == "skipped"
    assert forge.mutations == []

    ledger = list_write_backs(instance, WriteBackListParams()).actions
    assert ledger[0].outcome == "skipped"
    assert "write-back is off" in (ledger[0].detail or "")


def test_finishing_an_item_closes_the_linked_issue_under_full(
    instance: AppContext, forge: Forge
) -> None:
    ref = _adopted(instance, forge)
    set_write_back(
        instance, SetWriteBackParams(project="rustnzb", policy="full", reason=WHY)
    )
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state=state, reason=WHY)
        )

    assert [m for m, _ in forge.mutations] == ["PATCH"]
    assert forge.bodies[-1] == {"state": "closed"}

    ledger = list_write_backs(instance, WriteBackListParams(outcome="succeeded"))
    assert [a.action for a in ledger.actions] == ["close"]


def test_every_write_back_is_recorded_with_its_reason(
    instance: AppContext, forge: Forge
) -> None:
    """FR-B2: audited locally, and re-observed on the next sweep."""
    ref = _adopted(instance, forge)
    set_write_back(
        instance,
        SetWriteBackParams(project="rustnzb", policy="comment_only", reason=WHY),
    )
    comment_work(
        instance, CommentParams(ref=ref, body="x", reason="because a human asked")
    )
    ledger = list_write_backs(instance, WriteBackListParams()).actions
    posted = next(a for a in ledger if a.action == "comment")
    assert posted.reason == "because a human asked"
    assert posted.source_url is not None


# -- forge drift ----------------------------------------------------------


def test_closing_upstream_raises_a_state_mismatch(
    instance: AppContext, forge: Forge
) -> None:
    """The M5 demo's second half, minus the acceptance."""
    ref = _adopted(instance, forge)
    forge.issues[0]["state"] = "closed"
    sweep(instance, SweepParams(project="rustnzb", reason="look again"))

    raised = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    proposal = next(p for p in raised.raised if p.kind == FORGE_STATE_MISMATCH)
    assert "closed upstream" in proposal.summary or "is closed" in proposal.summary
    assert proposal.proposed_change["work_ref"] == ref


def test_accepting_a_state_mismatch_closes_the_item_with_provenance(
    instance: AppContext, forge: Forge
) -> None:
    ref = _adopted(instance, forge)
    forge.issues[0]["state"] = "closed"
    sweep(instance, SweepParams(project="rustnzb", reason="look again"))
    proposal = next(
        p
        for p in detect_drift(
            instance, DriftDetectParams(auto_accept=False, reason=WHY)
        ).raised
        if p.kind == FORGE_STATE_MISMATCH
    )

    resolve_drift(
        instance,
        DriftResolveParams(
            id=proposal.id,
            resolution="accepted",
            reason="it was fixed upstream and closed there",
        ),
    )
    assert get_work(instance, GetWorkParams(ref=ref)).item.state == "done"

    with instance.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "drift.resolve"
    assert "fixed upstream" in record.reason


def test_a_missing_automation_toggle_names_which_one(
    instance: AppContext, forge: Forge
) -> None:
    """FR-D6: three facts, and the gap says which of them is missing."""
    sweep(instance, SweepParams(project="rustnzb", reason=WHY))
    raised = detect_drift(instance, DriftDetectParams(auto_accept=False, reason=WHY))
    gap = next(p for p in raised.raised if p.kind == UPDATE_AUTOMATION_GAP)

    missing = gap.proposed_change["missing"]
    assert isinstance(missing, list)
    assert set(missing) == {
        "version updates",
        "vulnerability alerts",
        "automated security fixes",
    }, "all three named individually, because they are set in three places"

    listed = list_drift(instance, DriftListParams())
    assert UPDATE_AUTOMATION_GAP in listed.human_gated


def test_posture_is_three_facts_not_one_boolean(
    instance: AppContext, forge: Forge
) -> None:
    sweep(instance, SweepParams(project="rustnzb", reason=WHY))
    posture = instance.observed.latest(kinds=("forge.posture",), limit=1)[0]
    assert set(posture.payload) >= {
        "version_updates",
        "vulnerability_alerts",
        "automated_security_fixes",
    }


# -- the M5 demo -----------------------------------------------------------


def test_m5_demo(instance: AppContext, forge: Forge) -> None:
    # Onboarding changes nothing upstream, and everything appears with
    # labels intact.
    result = onboard(instance, OnboardParams(project="rustnzb", reason=WHY))
    assert forge.mutations == [], "not one non-GET request"
    assert result.mutations == 0
    assert result.issues == 2

    open_bug = next(
        entry
        for entry in bugs(instance, BugsParams(limit=50)).items
        if "Segment fetch retries" in entry.title
    )
    assert "bug" in open_bug.labels, "labels intact"
    assert "p1" in open_bug.labels

    # Adopt it, then close the issue upstream.
    adopted = adopt(
        instance, AdoptParams(subject=open_bug.ref, reason="we are fixing this")
    )
    forge.issues[0]["state"] = "closed"
    sweep(instance, SweepParams(project="rustnzb", reason="see what moved"))

    proposal = next(
        p
        for p in detect_drift(
            instance, DriftDetectParams(auto_accept=False, reason=WHY)
        ).raised
        if p.kind == FORGE_STATE_MISMATCH
    )
    assert proposal.evidence_snapshot["subject_key"] == open_bug.ref

    # Accept: the item closes, with provenance.
    resolve_drift(
        instance,
        DriftResolveParams(
            id=proposal.id,
            resolution="accepted",
            reason="closed upstream by the maintainer",
        ),
    )
    assert get_work(instance, GetWorkParams(ref=adopted.item.ref)).item.state == "done"

    with instance.declared.read() as view:
        trail = [r.operation for r in view.list_audit(limit=20)]
    assert "drift.resolve" in trail
    assert "work.adopt" in trail
