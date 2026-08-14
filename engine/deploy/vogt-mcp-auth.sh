#!/usr/bin/env bash
# Launch the remote Vogt stdio bridge with an ephemeral brokered token.
# This is intended as the MCP command for Claude Code and OpenCode.
#
# Mirrors deploy/cadastre-mcp-auth.sh, and exists for the same reason: the
# client registration records this command and the endpoint, never a bearer
# value, so no token is written into ~/.claude.json or opencode.json. Codex
# takes the URL directly with --bearer-token-env-var and does not need this.
set -euo pipefail

readonly VOGT_URL_DEFAULT="https://winrarhost.tailc7d3c.ts.net:18094"

# Inside a coding session, the session already holds a credential of its own
# — one Vogt minted for this session's actor so that what the agent writes
# is attributable to *this* session (FR-S10). Brokering here would replace
# it with the container-wide token and file every session's work under one
# identity, which fails silently: the agent still writes, the audit log is
# just wrong about who. So a session's token is used as it stands, and the
# broker is only asked when there is nothing to use.
if [[ -n "${VOGT_SESSION_ID:-}" && -n "${VOGT_HTTP_TOKEN:-}" ]]; then
    exec env \
        VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
        vogt-mcp-remote "$@"
fi

exec /usr/local/bin/mydevenv2-agent-auth run -- env \
    VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
    vogt-mcp-remote "$@"
