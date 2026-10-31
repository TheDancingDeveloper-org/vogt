"""The dialog key is the engine's when it sends one (WI-996)."""

from __future__ import annotations

from vogt.adapters.engine.client import EngineApproval
from vogt.core.escalation import approval_key, dialog_key


def test_the_engine_key_wins_and_python_only_computes_it_for_an_older_engine() -> None:
    sent = EngineApproval.from_payload(
        {
            "question": "go?\x1f",
            "command_excerpt": "",
            "detected_at": "t",
            "key": "e:permission_prompt:abc",
        }
    )
    assert sent is not None
    assert dialog_key("e", sent.key, sent.question) == "e:permission_prompt:abc"
    older = EngineApproval.from_payload(
        {"question": " go? ", "command_excerpt": "", "detected_at": "t"}
    )
    assert older is not None
    assert older.key == ""
    assert dialog_key("e", older.key, older.question) == approval_key("e", "go?")
