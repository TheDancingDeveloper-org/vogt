"""`initiative.publish` — an initiative as a forge tracking issue (#286).

An initiative is vogt-local by decision (#178 dec.3), so an epic is invisible
to anyone with only the forge open. This makes the "one tracking issue with a
`- [ ] #nnn` task list" workaround a *supported*, additive, forward-only
projection: for each forge-linked project the initiative spans, Vogt creates or
adopts one tracking issue labelled ``initiative:<slug>`` and keeps a managed
task list of its member work items current.

## The shape, and why every part of it is bounded

- **Create or adopt, never duplicate.** A hidden marker in the managed region
  (`marker_for`) lets a re-run recognise the issue Vogt already opened and edit
  it in place. Adoption is by that marker, so a person can even paste the
  markers into an existing issue to hand it to Vogt.
- **Only the managed region moves.** The re-render replaces the span between
  the markers and copies every other byte of the body through
  (`splice_managed_region`), so a human's own notes on the tracking issue
  survive untouched — the whole reason the projection can be forward-only.
- **Never a close, ever.** Vogt does not close the tracking issue. A *closed*
  initiative raises a drift proposal to close each tracking issue and leaves
  the write to a person (deliverable 4). Closing is somebody's call.
- **The tracking issue is observed like anything else.** A human ticking a box
  upstream is a body edit a sweep sees, and the mismatch against the member's
  workflow state surfaces as `initiative_checkbox_drift` — not a silent
  overwrite in either direction (deliverable 5).
- **One issue per repo, cross-referenced.** A cross-project initiative gets one
  tracking issue in each repo it spans, each linking the others (deliverable 6).

The verb records through `audited_action`: the effect lands upstream, not in
the declared store, so — like `forge.onboard` — the audit row is written after
the fact and says what the run actually produced.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from vogt.adapters.forge import ForgeProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.writeback import permits
from vogt.application.context import AppContext
from vogt.application.models import (
    InitiativeTrackingIssue,
    PublishInitiativeParams,
    PublishInitiativeResult,
)
from vogt.application.services import _resolve, upstream
from vogt.application.services.writeback import _writer_provider
from vogt.application.writes import WriteOutcome, audited_action, audited_write
from vogt.core.drift import INITIATIVE_TRACKING_CLOSE
from vogt.core.entities import Actor, DriftProposal, Initiative, Project, WorkItem
from vogt.core.initiative_projection import (
    TaskLine,
    body_has_marker,
    render_managed_region,
    splice_managed_region,
)
from vogt.storage.interface import ReadView, WriteTxn

INITIATIVE_PUBLISH = "initiative.publish"
INITIATIVE_PROJECTED_EVENT = "initiative.projected"
INITIATIVE_REPROJECTED_EVENT = "initiative.reprojected"

#: Far above any plausible initiative membership; the storage default of 100
#: would silently strand the 101st member, dropping it from the task list.
_MEMBER_LIMIT = 10_000


def _label_for(slug: str) -> str:
    """The label every tracking issue carries, so a person filtering the forge
    by label sees exactly the initiatives that were projected."""
    return f"initiative:{slug}"


@dataclass(frozen=True)
class _Target:
    """One repo the initiative spans, resolved and ready to project onto."""

    project: Project
    provider: ForgeProvider
    identity: str
    repo: RepoRef
    members: list[WorkItem]


@dataclass(frozen=True)
class _Placed:
    """A tracking issue after phase 1 (find-or-create), before cross-refs."""

    target: _Target
    number: int
    source_url: str | None
    current_body: str
    action: Literal["created", "adopted"]


def publish_initiative(
    ctx: AppContext, params: PublishInitiativeParams
) -> PublishInitiativeResult:
    """Project an initiative onto one tracking issue per forge-linked repo."""
    with ctx.declared.read() as view:
        initiative = _resolve.initiative(view, params.slug)
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        targets, skipped = _targets(ctx, view, actor, initiative)

    placed: list[_Placed] = []
    for target in targets:
        placed.append(_find_or_create(target, initiative))

    rows = _reproject_placed(ctx, initiative, placed, propose_close=True)
    rows.extend(skipped)

    result = PublishInitiativeResult(
        slug=initiative.slug,
        state=initiative.state,
        tracking_issues=rows,
    )
    audited_action(
        ctx,
        operation=INITIATIVE_PUBLISH,
        reason=params.reason,
        entity_kind="initiative",
        entity_id=initiative.id,
        outcome=result.model_dump(mode="json"),
        event_kind=INITIATIVE_PROJECTED_EVENT,
    )
    return result


def reproject_initiative(ctx: AppContext, initiative_id: str, *, reason: str) -> None:
    """Re-render existing tracking issues after a membership change (#286 d.2).

    Adopt-only: it never opens a tracking issue, so a membership change on an
    initiative nobody has published yet is a no-op. Best-effort by design — the
    local `work.update` has already committed, and a projection that could not
    be refreshed is not a reason to unwind it. Anything that could not be
    reached simply waits for the next explicit `initiative.publish`.
    """
    with ctx.declared.read() as view:
        initiative = view.initiative_by_id(initiative_id)
        if initiative is None:
            return
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        targets, _ = _targets(ctx, view, actor, initiative)

    placed = [
        found
        for target in targets
        if (found := _find_existing(target, initiative)) is not None
    ]
    if not placed:
        return

    rows = _reproject_placed(ctx, initiative, placed, propose_close=False)
    audited_action(
        ctx,
        operation=INITIATIVE_PUBLISH,
        reason=reason,
        entity_kind="initiative",
        entity_id=initiative.id,
        outcome=PublishInitiativeResult(
            slug=initiative.slug, state=initiative.state, tracking_issues=rows
        ).model_dump(mode="json"),
        event_kind=INITIATIVE_REPROJECTED_EVENT,
    )


# -- resolution ------------------------------------------------------------


def _members_in(
    ctx: AppContext, view: ReadView, project: Project, initiative: Initiative
) -> list[WorkItem]:
    """This project's member work items of the initiative, done ones included.

    Read the upstream-truth way (#181): the members are the observed issues
    joined to the overlay, not declared rows — there are none on a linked
    project. `include_closed` keeps a *done* member in the list so its box
    renders checked rather than vanishing when the issue closes upstream.
    """
    items = upstream.upstream_items(
        ctx, view, project, include_closed=True, limit=_MEMBER_LIMIT
    )
    return [item for item in items if item.initiative_id == initiative.id]


def _targets(
    ctx: AppContext,
    view: ReadView,
    actor: Actor | None,
    initiative: Initiative,
) -> tuple[list[_Target], list[InitiativeTrackingIssue]]:
    """The forge-linked projects the initiative spans, ready to project onto.

    A project is a target when it is linked, holds at least one member of the
    initiative, its `repo_url` parses for a resolved credential, and its
    write-back policy permits `create` (a tracking issue is a create). A linked
    project the initiative touches but which cannot be written is reported as a
    `skipped` row, so the result explains the gap rather than dropping it.
    """
    targets: list[_Target] = []
    skipped: list[InitiativeTrackingIssue] = []
    for project in upstream.linked_projects(view, None):
        members = _members_in(ctx, view, project, initiative)
        if not members:
            continue
        if not permits(project.write_back, "create"):
            skipped.append(
                _skip(
                    project,
                    f"write-back policy {project.write_back!r} does not permit "
                    "opening a tracking issue — set it with `forge writeback`",
                )
            )
            continue
        provider, identity = _writer_provider(ctx, actor, project.repo_url)
        repo = None if provider is None else provider.parse(project.repo_url)
        if provider is None or repo is None:
            skipped.append(
                _skip(project, "no usable forge credential for its repository")
            )
            continue
        # Only members that carry a forge number can appear as `- [ ] #n`. On a
        # linked project the item ref *is* the subject key, so this is every
        # member; anything without a numeric tail is quietly left off the list.
        numbered = [
            item for item in members if provider.number_of(item.ref) is not None
        ]
        targets.append(
            _Target(
                project=project,
                provider=provider,
                identity=identity,
                repo=repo,
                members=numbered,
            )
        )
    return targets, skipped


def _skip(project: Project, detail: str) -> InitiativeTrackingIssue:
    return InitiativeTrackingIssue(
        project_slug=project.slug,
        repo_url=project.repo_url,
        action="skipped",
        detail=detail,
    )


def _task_lines(target: _Target) -> list[TaskLine]:
    lines: list[TaskLine] = []
    for item in target.members:
        number = target.provider.number_of(item.ref)
        assert number is not None  # _targets kept only numbered members
        lines.append(
            TaskLine.from_state(number=number, title=item.title, state=item.state)
        )
    return sorted(lines, key=lambda line: line.number)


# -- phase 1: find or create ------------------------------------------------


def _find_existing(target: _Target, initiative: Initiative) -> _Placed | None:
    """The tracking issue Vogt already owns in this repo, adopted by marker."""
    for issue in target.provider.issues_updated_since(target.repo, since=None):
        if body_has_marker(issue.body, initiative.slug):
            return _Placed(
                target=target,
                number=issue.number,
                source_url=issue.source_url,
                current_body=issue.body or "",
                action="adopted",
            )
    return None


def _find_or_create(target: _Target, initiative: Initiative) -> _Placed:
    """Adopt the marked tracking issue, or open one if there is none yet."""
    existing = _find_existing(target, initiative)
    if existing is not None:
        return existing
    # A first, sibling-free render so the created issue is valid immediately;
    # phase 2 folds in cross-references and only re-writes if they change it.
    region = render_managed_region(
        slug=initiative.slug,
        body=initiative.body,
        tasks=_task_lines(target),
    )
    sent = target.provider.create_issue(
        target.repo,
        title=initiative.title,
        body=region,
        labels=[_label_for(initiative.slug)],
    )
    number = target.provider.number_of(sent.subject_key)
    if sent.outcome != "succeeded" or number is None:
        raise _projection_failed(target, sent.detail)
    return _Placed(
        target=target,
        number=number,
        source_url=sent.source_url,
        current_body=region,
        action="created",
    )


# -- phase 2: render with cross-refs, splice, update ------------------------


def _reproject_placed(
    ctx: AppContext,
    initiative: Initiative,
    placed: list[_Placed],
    *,
    propose_close: bool,
) -> list[InitiativeTrackingIssue]:
    """Render each managed region with the full sibling set and update in place.

    Two passes only meet here: phase 1 handed back where every tracking issue
    lives, so the cross-reference block can name all the siblings, and the
    splice keeps each repo's human prose. A body that comes out identical to
    what is already upstream is left alone — the common single-repo create needs
    no second write.
    """
    siblings = {p.target.project.slug: p.source_url for p in placed if p.source_url}
    rows: list[InitiativeTrackingIssue] = []
    close_targets: list[_Placed] = []
    for item in placed:
        others = [
            (slug, url)
            for slug, url in siblings.items()
            if slug != item.target.project.slug
        ]
        region = render_managed_region(
            slug=initiative.slug,
            body=initiative.body,
            tasks=_task_lines(item.target),
            siblings=sorted(others),
        )
        new_body = splice_managed_region(item.current_body, region)
        detail = None
        if new_body != item.current_body:
            sent = item.target.provider.update_issue_body(
                item.target.repo, item.number, body=new_body
            )
            if sent.outcome != "succeeded":
                raise _projection_failed(item.target, sent.detail)
            detail = f"as {item.target.identity}"
        close_here = propose_close and initiative.state == "closed"
        if close_here:
            close_targets.append(item)
        rows.append(
            InitiativeTrackingIssue(
                project_slug=item.target.project.slug,
                repo_url=item.target.project.repo_url,
                number=item.number,
                source_url=item.source_url,
                action=item.action,
                members=len(item.target.members),
                detail=detail,
                close_proposed=close_here,
            )
        )
    if close_targets:
        _propose_closes(ctx, initiative, close_targets)
    return rows


# -- deliverable 4: closing the initiative PROPOSES, never writes -----------


def _propose_closes(
    ctx: AppContext, initiative: Initiative, placed: list[_Placed]
) -> None:
    """Raise one close proposal per tracking issue; write no upstream close.

    Deduped against the open proposals already standing for this initiative, so
    re-publishing a closed initiative does not pile up a fresh proposal per run.
    """
    with ctx.declared.read() as view:
        open_now = view.list_drift(
            status="open", kind=INITIATIVE_TRACKING_CLOSE, limit=_MEMBER_LIMIT
        )
    already = {
        str(p.proposed_change.get("subject_key"))
        for p in open_now
        if p.subject_id == initiative.id
    }
    for item in placed:
        subject_key = item.target.provider.subject_key(item.target.repo, item.number)
        if subject_key in already:
            continue
        _raise_close_proposal(ctx, initiative, item, subject_key)


def _raise_close_proposal(
    ctx: AppContext, initiative: Initiative, item: _Placed, subject_key: str
) -> None:
    proposal = DriftProposal(
        id=ctx.id_factory("dft"),
        kind=INITIATIVE_TRACKING_CLOSE,
        subject_kind="initiative",
        subject_id=initiative.id,
        project_id=item.target.project.id,
        project_slug=item.target.project.slug,
        summary=(
            f"initiative {initiative.slug!r} is closed; its tracking issue "
            f"#{item.number} in {item.target.project.slug!r} is still open "
            "upstream — Vogt proposes closing it, never writes the close"
        ),
        proposed_change={
            "entity": "tracking_issue",
            "action": "close",
            "initiative": initiative.slug,
            "subject_key": subject_key,
            "number": item.number,
            "source_url": item.source_url,
        },
        opened_at=ctx.clock(),
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DriftProposal]:
        del actor
        txn.insert_drift(proposal)
        return WriteOutcome(
            result=proposal,
            entity_kind="drift_proposal",
            entity_id=proposal.id,
            payload=proposal.model_dump(mode="json"),
            event_kind="drift.raised",
            summary={"kind": proposal.kind, "initiative": initiative.slug},
        )

    audited_write(
        ctx,
        operation=INITIATIVE_PUBLISH,
        reason=f"initiative {initiative.slug!r} closed: propose tracking-issue close",
        body=body,
    )


def _projection_failed(target: _Target, detail: str | None) -> Exception:
    from vogt.errors import UpstreamWriteFailed

    return UpstreamWriteFailed(
        f"projecting the initiative onto {target.project.slug!r} failed: "
        f"{detail or 'no detail from the provider'} — nothing was closed or "
        "deleted, and re-running `initiative publish` resumes"
    )


__all__ = [
    "INITIATIVE_PROJECTED_EVENT",
    "INITIATIVE_PUBLISH",
    "INITIATIVE_REPROJECTED_EVENT",
    "publish_initiative",
    "reproject_initiative",
]
