#!/usr/bin/env bash
# Launch the remote Cadastre stdio bridge with an ephemeral brokered token.
# This is intended as the MCP command for Claude Code and OpenCode.
set -euo pipefail

exec /usr/local/bin/mydevenv2-agent-auth run -- env \
    CADASTRE_MCP_URL="${CADASTRE_MCP_URL:-https://winrarhost.tailc7d3c.ts.net:18081/mcp}" \
    CADASTRE_REMOTE_ONLY=1 \
    cadastre-mcp-remote "$@"
