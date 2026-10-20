"""Cache-aware compaction on a cold wake (WI-2037).

A provider keeps a conversation's prompt prefix in its cache for a few
minutes (Anthropic: 5 minutes, or an hour on the longer tier; DeepSeek: about
5 minutes). A hibernated session woken after that window pays the full input
price to re-read its whole transcript on the first turn. Past a size, a short
structured summary — task, state, decisions, files, branch, next step — is
cheaper to start from than the transcript it replaces.

This module decides whether a wake should compact and renders the summary.
It is pure: the caller supplies the clock, the session's last reply and the
work item's hand-off. The summary needs no model call; it is assembled from
what the session and its work item already say.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Literal

from vogt.core.entities import Comment

#: The agent CLIs a compacted wake can start fresh under a new conversation
#: id. opencode mints its own id and is not resumed by id, so it is left out.
COMPACTABLE_AGENTS = frozenset({"claude", "klaudia", "codex"})

#: The agent CLI's default provider, when the model name does not say.
_AGENT_PROVIDER = {"claude": "anthropic", "klaudia": "anthropic", "codex": "openai"}

#: Model-name fragments that name a provider, checked in order.
_MODEL_PROVIDER = (
    ("deepseek", "deepseek"),
    ("claude", "anthropic"),
    ("opus", "anthropic"),
    ("sonnet", "anthropic"),
    ("haiku", "anthropic"),
    ("fable", "anthropic"),
    ("gemini", "google"),
    ("gpt", "openai"),
    ("grok", "xai"),
    ("glm", "zhipu"),
    ("qwen", "qwen"),
)

#: How much of the last reply and the hand-off body the summary carries.
REPLY_CAP = 6_000
HANDOFF_CAP = 4_000

Outcome = Literal[
    "compact",
    "disabled",
    "opted-out",
    "agent",
    "provider",
    "warm",
    "small",
    "unmeasured",
]


@dataclass(frozen=True)
class Decision:
    """Whether a wake compacts, and the facts the audit record keeps."""

    outcome: Outcome
    provider: str | None = None
    ttl_seconds: int | None = None
    idle_seconds: int | None = None
    context_tokens: int | None = None
    threshold_tokens: int | None = None

    @property
    def compact(self) -> bool:
        return self.outcome == "compact"

    def audit(self) -> dict[str, object]:
        return {
            "outcome": self.outcome,
            "provider": self.provider,
            "ttl_seconds": self.ttl_seconds,
            "idle_seconds": self.idle_seconds,
            "context_tokens": self.context_tokens,
            "threshold_tokens": self.threshold_tokens,
        }


def provider_of(agent: str | None, model: str | None) -> str | None:
    """The provider whose cache a session's conversation sits in."""
    name = (model or "").lower()
    for fragment, provider in _MODEL_PROVIDER:
        if fragment in name:
            return provider
    return _AGENT_PROVIDER.get(agent or "")


def decide(
    *,
    enabled: bool,
    opted_out: bool,
    agent: str | None,
    model: str | None,
    hibernated_at: datetime | None,
    now: datetime,
    context_tokens: int | None,
    ttl_seconds: dict[str, int],
    threshold_tokens: int,
) -> Decision:
    """Whether waking this session should start fresh from a summary.

    Compacts only when every condition holds: the feature is on, the session
    has not opted out, its agent can start a fresh conversation, its
    provider's cache lifetime is configured and has passed since it was
    hibernated, and its last measured context is at or over the threshold.
    The idle time is measured from the hibernation: the agent did nothing
    after it, so the cache has been cold at least that long.
    """
    if not enabled:
        return Decision("disabled")
    if opted_out:
        return Decision("opted-out")
    if agent not in COMPACTABLE_AGENTS:
        return Decision("agent")
    provider = provider_of(agent, model)
    ttl = None if provider is None else ttl_seconds.get(provider)
    if ttl is None:
        return Decision("provider", provider=provider)
    idle = None if hibernated_at is None else int((now - hibernated_at).total_seconds())
    facts = {
        "provider": provider,
        "ttl_seconds": ttl,
        "idle_seconds": idle,
        "context_tokens": context_tokens,
        "threshold_tokens": threshold_tokens,
    }
    if idle is None or idle <= ttl:
        return Decision("warm", **facts)  # type: ignore[arg-type]
    if context_tokens is None:
        return Decision("unmeasured", **facts)  # type: ignore[arg-type]
    if context_tokens < threshold_tokens:
        return Decision("small", **facts)  # type: ignore[arg-type]
    return Decision("compact", **facts)  # type: ignore[arg-type]


def _cap(text: str, limit: int) -> str:
    text = text.strip()
    if len(text) <= limit:
        return text
    return text[:limit].rstrip() + "\n\n[truncated]"


def _bullets(items: list[str]) -> str:
    return "\n".join(f"- {item}" for item in items)


def render_summary(
    *,
    session_id: str,
    work_item: str | None,
    work_item_title: str | None,
    reason: str | None,
    last_reply: str | None,
    handoff: Comment | None,
    decision: Decision,
) -> str:
    """The structured summary a compacted wake starts from.

    Markdown, in the order a fresh agent needs it: what the task is, where it
    stood, what was decided, the files and branch, then the next step. Each
    section says plainly when nothing was recorded, rather than leaving the
    agent to guess. The engine appends the session directory's git facts and
    the original brief after it.
    """
    fields = None if handoff is None else handoff.handoff
    out = [
        "# Resumed from a summary (cold wake)",
        "",
        f"This session ({session_id}) was hibernated for "
        f"{decision.idle_seconds}s, past its {decision.provider} prompt-cache "
        f"lifetime ({decision.ttl_seconds}s), with about "
        f"{decision.context_tokens} tokens of context. Rather than re-read that "
        "transcript at full price, Vogt started a fresh conversation from this "
        "summary. The earlier transcript is not in your context. Re-read files "
        "before you change them; do not assume what they hold.",
        "",
        "## Task",
        "",
    ]
    if work_item:
        title = f" — {work_item_title}" if work_item_title else ""
        out.append(f"Work item {work_item}{title}.")
    if reason:
        out.append(f"Session started for: {reason.strip()}")
    if not work_item and not reason:
        out.append("(no task recorded; see the original brief below)")
    out += ["", "## State (your last reply before hibernation)", ""]
    has_reply = last_reply is not None and last_reply.strip() != ""
    no_reply = "(no reply recorded)"
    out.append(_cap(last_reply or "", REPLY_CAP) if has_reply else no_reply)
    out += ["", "## Decisions and hand-off", ""]
    if handoff is not None:
        written = handoff.created_at.isoformat()
        out.append(f"Latest hand-off on the work item ({written}):")
        out.append("")
        body = handoff.body.strip()
        out.append(_cap(body, HANDOFF_CAP) if body else "(no body)")
        if fields is not None and fields.done:
            out += ["", "Done:", _bullets(fields.done)]
        if fields is not None and fields.blockers:
            out += ["", "Blockers:", _bullets(fields.blockers)]
    else:
        out.append("(no hand-off recorded on the work item)")
    out += ["", "## Files and branch", ""]
    facts: list[str] = []
    if fields is not None:
        if fields.branch:
            facts.append(f"branch: {fields.branch}")
        if fields.head_sha:
            facts.append(f"head: {fields.head_sha}")
        if fields.base:
            facts.append(f"base: {fields.base}")
        facts += [f"touches: {p}" for p in fields.touch_points]
    out.append(
        _bullets(facts)
        if facts
        else "See the git facts of the session directory below."
    )
    out += ["", "## Next step", ""]
    if fields is not None and fields.remaining:
        out.append(_bullets(fields.remaining))
    else:
        out.append(
            "Continue from the state above. If it is unclear, re-read the "
            "original brief below and check the branch before acting."
        )
    out.append("")
    return "\n".join(out)
