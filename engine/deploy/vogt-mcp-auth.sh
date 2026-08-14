#!/usr/bin/env bash
# Launch the remote Vogt stdio bridge with an ephemeral brokered token.
# This is intended as the MCP command for Claude Code and OpenCode.
#
# Mirrors deploy/cadastre-mcp-auth.sh, and exists for the same reason: the
# client registration records this command and the endpoint, never a bearer
# value, so no token is written into ~/.claude.json or opencode.json. Codex
# takes the URL directly with --bearer-token-env-var and does not need this.
set -euo pipefail

exec /usr/local/bin/mydevenv2-agent-auth run -- env \
    VOGT_URL="${VOGT_URL:-https://winrarhost.tailc7d3c.ts.net:18094}" \
    vogt-mcp-remote "$@"
