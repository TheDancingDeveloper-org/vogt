"""What a session's agent is told it is working on (FR-E4).

The brief is markdown, assembled from what Vogt already knows and nothing
else. Two rules shaped it:

- **It says only what is recorded.** No summarising, no inferring what the
  item "really" means, no instructions about how to do the work. Everything
  here can be pointed at a row. An agent that is told something Vogt cannot
  show it the source of has been given an opinion dressed as a fact.
- **It ends by saying how to answer back.** The session carries a token
  scoped to `work.write` (FR-S10), so the agent can record what it found —
  and a brief that describes the work without mentioning that leaves the
  capability undiscovered, which is the same as not having it.
"""

from __future__ import annotations

from vogt.core.entities import WorkItem
from vogt.storage.interface import ReadView


def brief_for_work_item(view: ReadView, item: WorkItem, session_id: str) -> str:
    """The work item, as a page an agent can read before it starts."""
    lines: list[str] = [f"# {item.ref} — {item.title}", ""]

    facts = [
        f"**Kind** {item.kind}",
        f"**State** {item.state}",
        f"**Priority** {item.priority}",
    ]
    if item.effort:
        facts.append(f"**Effort** {item.effort}")
    if item.project_slug:
        facts.append(f"**Project** {item.project_slug}")
    if item.labels:
        facts.append("**Labels** " + ", ".join(item.labels))
    lines += [" · ".join(facts), ""]

    if item.body.strip():
        lines += ["## Description", "", item.body.strip(), ""]

    if item.relations:
        lines += ["## Relations", ""]
        for relation in item.relations:
            related = view.work_item_by_id(relation.related_id)
            # A relation to an item that has been deleted still says
            # something; showing the id beats dropping the row silently.
            label = (
                relation.related_id
                if related is None
                else f"{related.ref} — {related.title}"
            )
            lines.append(f"- {relation.kind.replace('_', ' ')} {label}")
        lines.append("")

    comments = view.comments_for(item.id, limit=20)
    if comments:
        lines += ["## Comments", ""]
        for comment in comments:
            lines.append(f"- {comment.body.strip()}")
        lines.append("")

    lines += [
        "## Recording what you find",
        "",
        f"This session is `{session_id}` and holds a token bound to its own "
        "actor, so anything it writes to Vogt is attributed to this session "
        "rather than to whoever started it.",
        "",
        "Vogt is reachable over MCP at the URL in `VOGT_URL`, with the token "
        "in `VOGT_HTTP_TOKEN`. The token may read, and may write work items "
        "and comments — nothing else. Every write needs a reason you have "
        "actually got: it is stored, and it is what somebody reads later when "
        "they ask why this changed.",
    ]
    return "\n".join(lines) + "\n"


def brief_for_project(view: ReadView, project_slug: str, session_id: str) -> str:
    """A terminal opened on a project, which is a plain shell with context.

    Deliberately thinner than the work-item brief. Nobody asked for anything
    in particular to be done here, and inventing a task for the agent —
    "have a look at the backlog" — would be Vogt deciding to start work,
    which is the half of the reversed non-goal that stayed refused (r9).
    """
    del view
    return (
        f"# {project_slug}\n"
        "\n"
        "A terminal opened on this project. No work item is attached, so "
        "there is no task here beyond what you were asked for directly.\n"
        "\n"
        f"This session is `{session_id}`. Vogt is at `VOGT_URL` with the "
        "token in `VOGT_HTTP_TOKEN`, scoped to read and to write work items.\n"
    )


__all__ = ["brief_for_project", "brief_for_work_item"]
