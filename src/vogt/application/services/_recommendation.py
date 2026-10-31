"""The overseer's recorded pick on a permission dialog (WI-996 E0b).

Shared by the Inbox's approval entry and the oversight sweep's approval
row, so both show the same recommendation beside the person's buttons.
"""

from __future__ import annotations

from vogt.adapters.engine.client import EngineApproval
from vogt.core.escalation import (
    SESSION_ANSWER_RECOMMENDED,
    dialog_instance,
    dialog_key,
)
from vogt.storage.interface import ReadView

#: The most recommendation events read for one dialog.
_RECOMMENDATION_SCAN = 50


def dialog_recommendation(
    view: ReadView, session_id: str, approval: EngineApproval
) -> dict[str, object] | None:
    """The newest recommendation recorded on this dialog (WI-996 E0b).

    An agent whose answer the engine refused has its pick recorded under the
    dialog's escalation key. It is shown beside the person's buttons, never
    chosen for them, and only on the same showing of the dialog (detection
    time and command text) while it still names an option the dialog shows
    with the same label. A pick made on an earlier dialog asking the same
    question is never carried over.
    """
    key = dialog_key(session_id, approval.key, approval.question)
    events = view.list_events(after=0, limit=_RECOMMENDATION_SCAN, entity_id=key)
    instance = dialog_instance(approval.detected_at, approval.command_excerpt)
    shown = {number: text for number, text, _selected in approval.options}
    for event in reversed(events):
        if event.kind != SESSION_ANSWER_RECOMMENDED:
            continue
        option = event.summary.get("option")
        label = event.summary.get("label")
        if event.summary.get("dialog") != instance:
            continue
        if not isinstance(option, int) or shown.get(option) != label:
            continue
        return {
            "option": option,
            "label": label,
            "by": str(event.summary.get("by", "an agent")),
            "at": event.at.isoformat(),
            "escalation_key": key,
        }
    return None
