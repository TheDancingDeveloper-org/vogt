#!/usr/bin/env bash
# Start the Vogt stdio bridge against Cadastre's HTTP MCP endpoint (WI-4288).
#
#   cadastre-mcp
#
# This is the command `vogt-mcp-bootstrap` registers as the `cadastre` MCP
# server. A header that names `${CADASTRE_HTTP_TOKEN}` only connects while
# that variable is exported, and a brokered session declares the token
# `ondemand`, so it never is: a hand-started agent got no cadastre. The token
# is resolved here, at spawn, and the stored registration holds neither it nor
# the URL.
#
#   CADASTRE_MCP_URL          the endpoint (https://cadastre.example/mcp); the
#                             trailing /mcp is optional
#   CADASTRE_HTTP_TOKEN       the token, when the session holds it
#   CADASTRE_HTTP_TOKEN_FILE  a file holding it, when the session holds that
#
# With neither, the token is fetched from the engine's secret broker
# (`vogt-agent-auth fetch CADASTRE_HTTP_TOKEN`). The value goes into this
# process's environment, never argv or a file.
#
# This session's own Vogt token, session id and engine session id belong to the
# local deployment, so they are dropped: the bridge would present them in place
# of the Cadastre token.
#
# stdout is the MCP transport. Nothing but the bridge may write to it; every
# message here goes to stderr.
set -euo pipefail

die() {
    printf 'cadastre-mcp: %s\n' "$*" >&2
    exit 64
}

[[ -n "${CADASTRE_MCP_URL:-}" ]] || die "CADASTRE_MCP_URL is not set in this session; the cadastre MCP server is not available"

token="${CADASTRE_HTTP_TOKEN:-}"
if [[ -z "$token" && -n "${CADASTRE_HTTP_TOKEN_FILE:-}" ]]; then
    [[ -r "$CADASTRE_HTTP_TOKEN_FILE" ]] || die "CADASTRE_HTTP_TOKEN_FILE is set but not readable"
    token="$(<"$CADASTRE_HTTP_TOKEN_FILE")"
fi
if [[ -z "$token" ]]; then
    token="$("${VOGT_AGENT_AUTH:-/usr/local/bin/vogt-agent-auth}" fetch CADASTRE_HTTP_TOKEN)" \
        || die "could not fetch CADASTRE_HTTP_TOKEN from the secret broker"
fi
token="${token//[$'\r\n\t ']/}"
[[ -n "$token" ]] || die "CADASTRE_HTTP_TOKEN is empty"

base="${CADASTRE_MCP_URL%/}"
base="${base%/mcp}"

unset VOGT_SESSION_ID VOGT_TOKEN_FILE VOGT_ENGINE_SESSION_ID CADASTRE_HTTP_TOKEN CADASTRE_HTTP_TOKEN_FILE
export VOGT_URL="$base"
export VOGT_HTTP_TOKEN="$token"
unset token
exec vogt-mcp-remote
