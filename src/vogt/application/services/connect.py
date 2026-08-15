"""Connecting a client (FR-A8), and the document `DEPLOYMENT.md` §4.3 wanted.

The gap this closes is narrow and was invisible for a long time. `/health`,
`/version` and `/connection-info` all shipped at M4 and are tested, and
between them they report every fact about this instance except the one a
client needs first: **where it is**. `/connection-info` returns paths.

The server cannot supply the missing half by inference. It binds
`0.0.0.0:8000` inside a container and is published at a tailnet address on
another port entirely; the address a client should use is a fact only the
operator holds. So it is configuration (`public_url`), it is an *exposure*
value and therefore carries no default (NFR-D2), and when it is unset this
says so rather than guessing. A URL the server invented would be wrong in
precisely the deployment the field exists for, and from a client a wrong URL
and an unreachable one look identical.

§4.3 asked for a generated `CONNECTING.md`. It is generated here, as an
operation rather than as a committed file: a file in the repository is a
copy that drifts from the instance it describes, which is the failure §4.3
was written to prevent. `vogt connect --format markdown > CONNECTING.md`
produces the file for anyone who wants one, from the running instance.
"""

from __future__ import annotations

import json

from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS
from vogt.application.context import AppContext
from vogt.application.identity import DEFAULT_API_PATH, DEFAULT_MCP_PATH
from vogt.application.models import ConnectParams, ConnectResult

#: The prose an unconfigured instance answers with. Long because it is the
#: whole answer: what is missing, why nothing can be inferred, and the one
#: line that fixes it.
NOT_CONFIGURED = (
    "no public_url is configured, so this instance cannot say where to reach "
    "it. That is not something a server can work out: it binds a container "
    "port and is published somewhere else. Set VOGT_PUBLIC_URL (or "
    "public_url) to the address clients actually use — an exposure value, so "
    "it is never defaulted (NFR-D2)"
)

#: What this process serves at, and what it reports when it is the door.
#: Behind a front door these are the door's to state, not this module's —
#: `identity.py` carries the same values as its fallback, which is where the
#: unfronted answer now comes from.
API_PATH = DEFAULT_API_PATH
MCP_PATH = DEFAULT_MCP_PATH


def connect(ctx: AppContext, params: ConnectParams) -> ConnectResult:
    """State how to reach this instance, and render a client config.

    The address comes from `ctx.public_identity` rather than from the config
    directly: behind a front door the address a client uses is the door's, and
    the door states it per request (`identity.py`). Everything else here —
    which client, which format, the prose and the JSON — is Vogt's own content
    and is rendered in this one place whichever shape it is deployed in.
    """
    identity = ctx.public_identity
    url = identity.url
    mcp_url = identity.mcp_url
    versions = list(SUPPORTED_PROTOCOL_VERSIONS)

    if params.client == "bridge":
        configuration = _bridge_config(url, params.format)
        requires_install = True
    else:
        configuration = _http_config(mcp_url, params.format, versions)
        requires_install = False

    return ConnectResult(
        url=url,
        api_path=identity.api_path,
        mcp_path=identity.mcp_path,
        mcp_url=mcp_url,
        supported_mcp_protocol_versions=versions,
        client=params.client,
        requires_install=requires_install,
        configuration=configuration,
        detail=None if url else NOT_CONFIGURED,
    )


def _http_config(mcp_url: str | None, fmt: str, versions: list[str]) -> str:
    """The recommended path: nothing installed, nothing to keep in step.

    A client speaking streamable HTTP holds no copy of Vogt's code, so there
    is no version to skew (FR-A6 exists because the other path has one) and
    no second place to upgrade.
    """
    target = mcp_url or "<set public_url first>"
    if fmt == "markdown":
        return "\n".join(
            [
                "# Connecting to Vogt",
                "",
                f"- MCP endpoint: `{target}`",
                "- Transport: streamable HTTP",
                f"- Protocol versions: {', '.join(versions)}",
                "- Authentication: `Authorization: Bearer <token>`",
                "",
                "Nothing needs installing. Issue a token with "
                "`vogt token issue`, and send it as a header — never in the "
                "URL, which ends up in logs, proxies and history (FR-S7).",
                "",
                "```console",
                f'$ claude mcp add --transport http vogt "{target}" \\',
                '    --header "Authorization: Bearer $VOGT_TOKEN"',
                "```",
            ]
        )
    return json.dumps(
        {
            "mcpServers": {
                "vogt": {
                    "type": "http",
                    "url": target,
                    "headers": {"Authorization": "Bearer ${VOGT_TOKEN}"},
                }
            }
        },
        indent=2,
    )


def _bridge_config(url: str | None, fmt: str) -> str:
    """For clients that can only spawn a local process.

    This one *does* need Vogt installed, which is the cost of the transport
    rather than a packaging oversight: the bridge is Vogt's code running on
    the client's machine. `vogt-mcp-remote` discovers the remote's paths and
    protocol versions from `/connection-info` at startup (FR-A5), so the only
    thing it is told is the URL and where its token lives.
    """
    target = url or "<set public_url first>"
    if fmt == "markdown":
        return "\n".join(
            [
                "# Connecting to Vogt (stdio bridge)",
                "",
                "For clients that cannot speak streamable HTTP. Prefer the "
                "`http` client where you have the choice: it installs "
                "nothing and cannot skew against the server.",
                "",
                f"- Remote: `{target}`",
                "- Command: `vogt-mcp-remote`",
                "- Token: `VOGT_TOKEN_FILE`, a file path — never argv, never "
                "a URL (FR-S7).",
            ]
        )
    return json.dumps(
        {
            "mcpServers": {
                "vogt": {
                    "command": "vogt-mcp-remote",
                    "env": {
                        "VOGT_URL": target,
                        "VOGT_TOKEN_FILE": "/path/to/token",
                    },
                }
            }
        },
        indent=2,
    )


__all__ = ["NOT_CONFIGURED", "connect"]
