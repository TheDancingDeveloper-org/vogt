"""The session tools' MCP descriptions promise what the code does.

An agent decides whether it can drive a session from these strings alone —
the 2026-10-04 drive test concluded "not possible" from descriptions that
under-sold the tools. Each promise asserted here is backed by behaviour
tested elsewhere:

- a `task` reaches the agent as its first prompt: `tests/test_sessions.py`
  (the brief carries a `## Task` section) and the engine's
  `an_agent_started_with_a_brief_is_told_to_read_it` integration test (the
  first prompt names the brief file);
- either id form: `tests/test_session_drive.py`;
- input audited without the text: `tests/test_session_drive.py`.
"""

from __future__ import annotations

from typing import Any

import pytest

from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext


@pytest.fixture
def tools(instance: AppContext) -> dict[str, Any]:
    surface = McpSurface(context_factory=lambda: instance)
    return {tool.name: tool for tool in surface.list_tools()}


def _param(tool: Any, name: str) -> str:
    return str(tool.input_schema["properties"][name].get("description", ""))


def test_session_start_says_a_task_is_the_agents_first_prompt(
    tools: dict[str, Any],
) -> None:
    start = tools["session_start"]
    assert "first prompt" in start.description
    assert "template" in start.description
    assert "session_screen" in start.description
    task = _param(start, "task")
    assert "first prompt" in task
    assert "Task section" in task
    assert "VOGT_ENGINE_AGENT_TASK_PROMPT_FILE" in task


def test_session_start_documents_resume(tools: dict[str, Any]) -> None:
    resume = _param(tools["session_start"], "resume")
    assert "--resume" in resume
    assert "engine_session_id" in resume
    assert "Requires `template`" in resume


@pytest.mark.parametrize(
    "name",
    ["session_input", "session_screen", "session_log_tail", "session_stop"],
)
def test_every_driving_tool_takes_either_id(tools: dict[str, Any], name: str) -> None:
    tool = tools[name]
    assert "ses_" in tool.description
    assert "UUID" in tool.description
    id_doc = _param(tool, "id")
    assert "ses_" in id_doc
    assert "engine_session_id" in id_doc


def test_session_list_names_both_ids_and_the_states(tools: dict[str, Any]) -> None:
    description = tools["session_list"].description
    assert "engine_session_id" in description
    for state in ("running", "idle", "waiting-for-input", "exited", "errored"):
        assert state in description


def test_session_input_warns_against_a_blind_enter(tools: dict[str, Any]) -> None:
    description = tools["session_input"].description
    assert "session_screen" in description
    assert "blind Enter" in description
    assert "never the text" in description
    assert tools["session_input"].scope == "work.write"


def test_session_screen_explains_ready(tools: dict[str, Any]) -> None:
    description = tools["session_screen"].description
    assert "`ready`" in description
    assert tools["session_screen"].scope == "read"


def test_session_log_tail_is_not_the_screen(tools: dict[str, Any]) -> None:
    assert "session_screen" in tools["session_log_tail"].description
