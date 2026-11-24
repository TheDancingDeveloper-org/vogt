#!/usr/bin/env bash
# Start the Vogt stdio bridge against a second, remote Vogt (WI-4289).
#
#   vogt-prod-mcp
#
# This is the command `vogt-mcp-bootstrap` registers as the `vogt-prod` MCP
# server, so a dev session can list, read and drive prod sessions with a
# token scoped for that and nothing more. The stored registration holds no
# token and no URL a session could edit into a broader one: both are read here
# at spawn.
#
#   VOGT_PROD_MCP_URL     the remote server's MCP endpoint
#                         (https://vogt.example/mcp); the trailing /mcp is
#                         optional
#   VOGT_PROD_TOKEN       the remote token, when the session holds it
#   VOGT_PROD_TOKEN_FILE  a file holding it, when the session holds that
#
# With neither, the token is fetched from the engine's secret broker
# (`vogt-agent-auth fetch VOGT_PROD_TOKEN`), which is how a manifest line
# flagged `ondemand` reaches a session: declared, never exported at launch.
# The value goes into this process's environment, never argv or a file.
#
# Every credential the bridge would otherwise prefer is dropped first: this
# session's own Vogt token and session id belong to the local deployment and
# mean nothing to the remote one, and the bridge would present them in place
# of the remote token if they were left set.
#
# stdout is the MCP transport. Nothing but the bridge may write to it; every
# message here goes to stderr.
set -euo pipefail

die() {
    printf 'vogt-prod-mcp: %s\n' "$*" >&2
    exit 64
}

[[ -n "${VOGT_PROD_MCP_URL:-}" ]] || die "VOGT_PROD_MCP_URL is not set in this session; the vogt-prod MCP server is not available"

token="${VOGT_PROD_TOKEN:-}"
if [[ -z "$token" && -n "${VOGT_PROD_TOKEN_FILE:-}" ]]; then
    [[ -r "$VOGT_PROD_TOKEN_FILE" ]] || die "VOGT_PROD_TOKEN_FILE is set but not readable"
    token="$(<"$VOGT_PROD_TOKEN_FILE")"
fi
if [[ -z "$token" ]]; then
    token="$("${VOGT_AGENT_AUTH:-/usr/local/bin/vogt-agent-auth}" fetch VOGT_PROD_TOKEN)" \
        || die "could not fetch VOGT_PROD_TOKEN from the secret broker"
fi
token="${token//[$'\r\n\t ']/}"
[[ -n "$token" ]] || die "VOGT_PROD_TOKEN is empty"

base="${VOGT_PROD_MCP_URL%/}"
base="${base%/mcp}"

unset VOGT_SESSION_ID VOGT_TOKEN_FILE VOGT_ENGINE_SESSION_ID VOGT_PROD_TOKEN VOGT_PROD_TOKEN_FILE
export VOGT_URL="$base"
export VOGT_HTTP_TOKEN="$token"
unset token
exec vogt-mcp-remote
