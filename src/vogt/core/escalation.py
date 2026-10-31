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


def approval_key(engine_session_id: str, question: str) -> str:
    """The escalation key of the dialog `question` on that engine session."""
    digest = hashlib.sha256(question.strip().encode()).hexdigest()[:16]
    return f"{engine_session_id}:permission_prompt:{digest}"
