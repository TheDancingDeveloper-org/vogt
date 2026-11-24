"""Group 2 — authentication and authorisation on the engine's routes."""

from __future__ import annotations

from ..harness import Case, Check, Context, failed, passed, skipped

#: Protected routes that exist on every deployment and must not answer without
#: a bearer. A route that has moved is a finding here, not a reason to weaken
#: the list: the runbook's A5 gate treats an open route as a failure.
PROTECTED_GET = (
    "/api/status",
    "/api/auth/check",
    "/api/sessions",
    "/api/session-templates",
    "/api/workspaces",
    "/api/agent-clis",
    "/api/git/status",
    "/api/dir",
    "/api/tree",
    "/api/search",
    "/api/history/sessions",
    "/api/vogt/status",
    "/api/vogt/projects",
)


def no_token_is_401(ctx: Context) -> Check:
    wrong: list[str] = []
    for path in PROTECTED_GET:
        response = ctx.client.http.request("GET", path, token=False)
        if response.status != 401:
            wrong.append(f"{path} -> {response.status}")
    response = ctx.client.http.request(
        "POST",
        "/mcp",
        body={"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}},
        token=False,
    )
    if response.status != 401:
        wrong.append(f"/mcp -> {response.status}")
    if wrong:
        return failed("routes answered without a bearer: " + "; ".join(wrong))
    return passed(
        f"{len(PROTECTED_GET)} routes + /mcp all answered 401 without a bearer"
    )


def bad_token_is_401(ctx: Context) -> Check:
    from ..harness import Token

    bogus = Token("acceptance-bogus-" + "0" * 24)
    wrong: list[str] = []
    for path in ("/api/status", "/api/vogt/status", "/api/sessions"):
        response = ctx.client.http.request("GET", path, token=bogus)
        if response.status != 401:
            wrong.append(f"{path} -> {response.status}")
    response = ctx.client.http.request(
        "POST",
        "/mcp",
        body={"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}},
        token=bogus,
    )
    if response.status != 401:
        wrong.append(f"/mcp -> {response.status}")
    if wrong:
        return failed("a bogus bearer was not refused: " + "; ".join(wrong))
    return passed("a bogus bearer gets 401 on the engine, the front door and /mcp")


def read_token_cannot_write(ctx: Context) -> Check:
    read_token = ctx.options.read_token
    if read_token is None:
        return skipped(
            "--read-token-file was not given; only the write-capable run token "
            "is available"
        )
    result, error = _try_mcp_with(ctx, read_token)
    if result is not None:
        return failed("a read-only token created a work item through MCP")
    if not any(
        word in (error or "").lower()
        for word in ("forbidden", "scope", "permission", "denied")
    ):
        return failed(f"the refusal does not name authorisation: {error!r}")
    response = ctx.client.http.request(
        "POST",
        "/api/vogt/work",
        body={
            "kind": "chore",
            "title": "acceptance read-token probe",
            "reason": "acceptance: prove a read token cannot write",
        },
        token=read_token,
    )
    if response.status not in (401, 403):
        return failed(
            f"REST write with a read token answered {response.status}, not 401/403"
        )
    return passed(f"MCP refused with {error!r}; REST answered {response.status}")


def _try_mcp_with(
    ctx: Context, token: object
) -> tuple[dict[str, object] | None, str | None]:
    from ..client import HttpClient, McpClient
    from ..harness import Token

    assert isinstance(token, Token)
    http = HttpClient(
        ctx.options.base_url or "", token=token, timeout=ctx.options.timeout
    )
    mcp = McpClient(http)
    try:
        return mcp.tool(
            "work_create",
            {
                "kind": "chore",
                "title": "acceptance read-token probe",
                "reason": "acceptance: prove a read token cannot write",
            },
        ), None
    except Exception as exc:  # the refusal is the answer we want
        return None, str(exc)


def identity_is_reported(ctx: Context) -> Check:
    response = ctx.client.http.request("GET", "/api/auth/check")
    if response.status != 200:
        return failed(f"/api/auth/check -> {response.status}")
    payload = response.json()
    identity = payload.get("identity") if isinstance(payload, dict) else None
    if not isinstance(identity, dict) or not identity.get("name"):
        return failed(f"/api/auth/check returned no identity: {payload!r}")
    return passed(
        f"/api/auth/check identity={identity.get('name')} "
        f"scopes={identity.get('scopes')}"
    )


CASES = [
    Case(
        "A1",
        "auth",
        "R2",
        "critical",
        "every protected route refuses a missing bearer",
        no_token_is_401,
    ),
    Case(
        "A2",
        "auth",
        "R2",
        "critical",
        "a bogus bearer is refused everywhere",
        bad_token_is_401,
    ),
    Case(
        "A3",
        "auth",
        "R2",
        "high",
        "a read-only token cannot write",
        read_token_cannot_write,
    ),
    Case(
        "A4",
        "auth",
        "O1",
        "medium",
        "the token resolves to a named identity",
        identity_is_reported,
    ),
]
