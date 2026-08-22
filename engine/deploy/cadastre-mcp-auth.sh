#!/usr/bin/env bash
# Launch the remote Cadastre stdio bridge with an ephemeral brokered token.
# This is intended as the MCP command for Claude Code and OpenCode.
#
# Cadastre is a separate, optional integration (NFR-O5). There is no built-in
# endpoint: set CADASTRE_MCP_URL to the bridge you run, or leave it unset and
# this wrapper skips cleanly — an agent then simply has no Cadastre MCP server.
set -euo pipefail

if [[ -z "${CADASTRE_MCP_URL:-}" ]]; then
    printf 'cadastre-mcp: CADASTRE_MCP_URL is not set; Cadastre MCP is not configured, skipping\n' >&2
    exit 0
fi

exec /usr/local/bin/mydevenv2-agent-auth run -- env \
    CADASTRE_MCP_URL="${CADASTRE_MCP_URL}" \
    CADASTRE_REMOTE_ONLY=1 \
    cadastre-mcp-remote "$@"
