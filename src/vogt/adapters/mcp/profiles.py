"""Tool profiles: how much of the tool list a session is shown (WI-2740).

Every tool definition rides on every request a model makes, and the full
Vogt list is about 150 KB of JSON. A worker or review session uses a dozen
of those tools, so it asks for the `worker` profile and `tools/list` returns
only them. A profile trims the listing, not the grant: `tools/call` still
checks the caller's scopes and nothing else, so a session that names a
tool outside its profile is served as before.

The profile is asked for with the `X-Vogt-Tool-Profile` header (the stdio
bridge sets it from `VOGT_MCP_TOOL_PROFILE`) or a `profile` query parameter
on the MCP URL (for clients such as Codex that take the URL directly). An
unknown or empty value is the full list.
"""

from __future__ import annotations

from collections.abc import Mapping

PROFILE_HEADER = "X-Vogt-Tool-Profile"
PROFILE_QUERY = "profile"
PROFILE_ENV = "VOGT_MCP_TOOL_PROFILE"

FULL = "full"
WORKER = "worker"

#: What an implement or review session needs: read and update its work item,
#: report progress or a block, ask a decision, and read the project brief.
WORKER_TOOLS: frozenset[str] = frozenset(
    {
        "auth_whoami",
        "charter_get",
        "decision_ask",
        "project_brief",
        "session_bind_work",
        "session_report",
        "session_report_blocked",
        "session_report_unblocked",
        "status",
        "why",
        "work_bind_branch",
        "work_comment",
        "work_create",
        "work_get",
        "work_get_many",
        "work_list",
        "work_relate",
        "work_transition",
    }
)

_PROFILES: dict[str, frozenset[str]] = {WORKER: WORKER_TOOLS}


def resolve_profile(headers: Mapping[str, str], query: Mapping[str, str]) -> str:
    """The profile a request asks for; the header wins over the query."""
    raw = headers.get(PROFILE_HEADER) or query.get(PROFILE_QUERY) or ""
    name = raw.strip().lower()
    return name if name in _PROFILES else FULL


def shows(profile: str, tool_name: str) -> bool:
    """True when `tool_name` is listed under `profile`."""
    allowed = _PROFILES.get(profile)
    return allowed is None or tool_name in allowed
