"""Which agent, model and effort a session is running, and how we know (WI-919).

Three sources, best first, and every answer says which it came from:

- `transcript`: what the agent CLI recorded for its latest turn — the model
  actually used, whatever the session was asked for or the CLI defaulted to;
- `command`: a flag on the session's command line (`--model`, `--effort`,
  `-m`, `-c model_reasoning_effort=`);
- `asked`: what `session.start` was asked for.

None of them is a default the CLI might pick: an unknown is null, not a
guess. Pure: the transcript's reading is passed in.
"""

from __future__ import annotations

import re
import shlex
from dataclasses import dataclass

AGENTS = ("claude", "codex", "opencode", "klaudia")

_EFFORT_OVERRIDE = re.compile(r"^model_reasoning_effort=(.+)$")


@dataclass(frozen=True)
class CommandRuntime:
    agent: str | None
    model: str | None
    effort: str | None


def from_command(command: str | None) -> CommandRuntime:
    """The agent CLI a command runs and the model/effort flags it carries."""
    if not command:
        return CommandRuntime(None, None, None)
    try:
        words = shlex.split(command)
    except ValueError:
        words = command.split()
    agent: str | None = None
    model: str | None = None
    effort: str | None = None
    for index, word in enumerate(words):
        name = word.rsplit("/", 1)[-1]
        if agent is None and name in AGENTS:
            agent = name
        following = words[index + 1] if index + 1 < len(words) else None
        if word in ("--model", "-m") and following:
            model = following
        elif word.startswith("--model="):
            model = word.split("=", 1)[1]
        elif word == "--effort" and following:
            effort = following
        elif word.startswith("--effort="):
            effort = word.split("=", 1)[1]
        elif word == "-c" and following:
            match = _EFFORT_OVERRIDE.match(following)
            if match:
                effort = match.group(1).strip("'\"")
    return CommandRuntime(agent, model, effort)


@dataclass(frozen=True)
class Resolved:
    agent: str | None
    model: str | None
    model_basis: str | None
    effort: str | None
    effort_basis: str | None


def resolve(
    *,
    command: str | None,
    conversation_agent: str | None,
    transcript_model: str | None,
    transcript_effort: str | None,
    asked_model: str | None,
    asked_effort: str | None,
) -> Resolved:
    """The best answer for each of model and effort, with its source."""
    flags = from_command(command)

    def pick(*candidates: tuple[str | None, str]) -> tuple[str | None, str | None]:
        for value, basis in candidates:
            if value:
                return value, basis
        return None, None

    model, model_basis = pick(
        (transcript_model, "transcript"),
        (flags.model, "command"),
        (asked_model, "asked"),
    )
    effort, effort_basis = pick(
        (transcript_effort, "transcript"),
        (flags.effort, "command"),
        (asked_effort, "asked"),
    )
    return Resolved(
        agent=flags.agent or conversation_agent,
        model=model,
        model_basis=model_basis,
        effort=effort,
        effort_basis=effort_basis,
    )
