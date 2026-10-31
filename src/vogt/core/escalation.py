"""Escalation keys and the overseer's recommendation on a dialog (WI-996).

A permission dialog's escalation key is `<engine id>:permission_prompt:` and
the first 16 hex characters of sha256 of its question, the shape a blocked
report's key has. The engine's refusal of an agent's answer (WI-983) names it,
and the Inbox reads the agent's recorded pick under it (E0b).
"""

from __future__ import annotations

import hashlib

#: Audited when an agent's answer to a permission prompt is refused: its pick
#: becomes a recommendation shown beside the person's buttons, never chosen.
SESSION_ANSWER_RECOMMENDED = "session.answer_recommended"
#: The event's entity kind. Its entity id is the dialog's escalation key, so
#: the Inbox reads a dialog's recommendations by that key alone.
RECOMMENDATION_ENTITY = "escalation"


def dialog_key(engine_session_id: str, engine_key: str, question: str) -> str:
    """A dialog's escalation key: the engine's own when it sent one.

    The engine computes the key in one place (`ApprovalPrompt.key`), so the
    trim rule cannot drift between Rust and Python. `approval_key` is only
    the fallback for an older engine that sends none.
    """
    return engine_key or approval_key(engine_session_id, question)


def approval_key(engine_session_id: str, question: str) -> str:
    """The escalation key of the dialog `question` on that engine session,
    computed here only for an engine that does not send its own."""
    digest = hashlib.sha256(question.strip().encode()).hexdigest()[:16]
    return f"{engine_session_id}:permission_prompt:{digest}"
