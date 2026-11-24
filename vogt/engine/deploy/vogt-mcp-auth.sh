#!/usr/bin/env bash
# Launch the remote Vogt stdio bridge with an ephemeral brokered token.
# This is intended as the MCP command for Claude Code and OpenCode.
#
# The client registration records this command and the endpoint, never a bearer
# value, so no token is written into ~/.claude.json or opencode.json. Codex
# takes the URL directly with --bearer-token-env-var and does not need this.
set -euo pipefail

# The front door on loopback, for the reason `mcp-bootstrap.sh` gives at
# length: in the merged stack the engine is the only published port
# and this wrapper runs inside that container, so loopback needs no
# DNS and no certificate. It is only a fallback — a session exports its own
# `VOGT_URL` and that wins — but a fallback naming a specific deployment stops
# working the day that deployment is retired, so the fallback here is the front
# door on loopback, which belongs to whatever deployment this session is part
# of, rather than any named host.
readonly VOGT_URL_DEFAULT="http://127.0.0.1:8910"

# Inside a coding session, the session already holds a credential of its own
# — one Vogt minted for this session's actor so that what the agent writes
# is attributable to *this* session. Brokering here would replace
# it with the container-wide token and file every session's work under one
# identity, which fails silently: the agent still writes, the audit log is
# just wrong about who. So a session's token is used as it stands, and the
# broker is only asked when there is nothing to use.
if [[ -n "${VOGT_SESSION_ID:-}" && -n "${VOGT_HTTP_TOKEN:-}" ]]; then
    exec env \
        VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
        vogt-mcp-remote "$@"
fi

# Defensive fast-path. The pair above is the invariant `_session_env()`
# promises — token and session id set together — but a deployment can split it:
# v0.5.1 provisioned coding sessions with `VOGT_HTTP_TOKEN` (and the brokered
# token *file*) yet no `VOGT_SESSION_ID`, so the guard above missed and the
# broker below ran. In a coding session there are no secrets-manager credentials — brokering
# there is designed not to work — so the wrapper exited 1 and every client saw
# CONNECTION_CLOSED, despite holding a token that connects. When a usable
# credential is already in the environment, run the bridge directly and let its
# `resolve_token()` decide which source wins; only broker when there is nothing
# to use.
if [[ -n "${VOGT_HTTP_TOKEN:-}" || -s "${VOGT_TOKEN_FILE:-}" ]]; then
    exec env \
        VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
        vogt-mcp-remote "$@"
fi

readonly AGENT_AUTH="${VOGT_AGENT_AUTH:-/usr/local/bin/vogt-agent-auth}"

die() {
    printf 'vogt-mcp: %s\n' "$*" >&2
    exit 64
}

# No token in hand. An engine session can still ask the engine's secret broker
# on loopback (WI-4310): a `claude` started by hand in a shell that lost the
# session's token — a sub-shell under `env -i`, a sudo, a tmux server started
# elsewhere — still carries the broker's address and grant, and the broker
# needs no secrets manager. This is how `vogt-prod-mcp` gets its token. The
# value goes into this process's environment, never argv or a file.
broker_asked=0
if [[ -n "${VOGT_ENGINE_BROKER_URL:-${VOGT_ENGINE_URL:-}}" \
    && -n "${VOGT_ENGINE_BROKER_TOKEN:-}${VOGT_ENGINE_BROKER_TOKEN_FILE:-}${VOGT_SESSION_TOKEN_FILE:-}" ]]; then
    broker_asked=1
    if token="$("$AGENT_AUTH" fetch VOGT_HTTP_TOKEN)"; then
        token="${token//[$'\r\n\t ']/}"
        if [[ -n "$token" ]]; then
            export VOGT_HTTP_TOKEN="$token"
            unset token
            exec env \
                VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
                vogt-mcp-remote "$@"
        fi
    fi
    printf 'vogt-mcp: the engine broker has no VOGT_HTTP_TOKEN for this session\n' >&2
fi

# Last resort: a container that holds the secrets-manager identity mints the
# token itself. A brokered deployment strips that identity by design, so
# without it there is nothing left to try; say what is missing rather than
# letting the broker fail on an unset INFISICAL_API_URL.
if [[ -z "${INFISICAL_API_URL:-}" || -z "${INFISICAL_CLIENT_ID:-}" || -z "${INFISICAL_CLIENT_SECRET:-}" ]]; then
    if [[ "$broker_asked" == 1 ]]; then
        die "no Vogt token: the engine broker has no VOGT_HTTP_TOKEN for this session, VOGT_HTTP_TOKEN and VOGT_TOKEN_FILE are unset, and there is no secrets-manager identity. Start the agent from the session's own shell, which exports its token, or set VOGT_TOKEN_FILE to a Vogt token file."
    fi
    die "no Vogt token: VOGT_HTTP_TOKEN and VOGT_TOKEN_FILE are unset and this shell holds no engine broker credential, so it cannot act as a Vogt session. Start the agent from the session's own shell, which exports its token, or set VOGT_TOKEN_FILE to a Vogt token file."
fi

exec "$AGENT_AUTH" run -- env \
    VOGT_URL="${VOGT_URL:-$VOGT_URL_DEFAULT}" \
    vogt-mcp-remote "$@"
