#!/usr/bin/env bash
# Idempotently register Vogt for clients present in the image.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

# Where an agent in this session should reach Vogt, in the order of what
# actually knows the answer:
#
#   1. `VOGT_MCP_URL`, an explicit override. Unchanged.
#   2. The session's own `VOGT_URL`. `vogt session start` exports the endpoint
#      the operator configured for clients, and it is the only thing
#      here that knows which deployment this session belongs to. Ignoring it
#      is what this script used to do.
#   3. The front door on loopback. In the merged stack the engine is the only
#      published port and the agent runs in this container, so
#      loopback needs no DNS and no certificate — and it cannot go on naming
#      a deployment after that deployment is retired, which is what a default
#      pointing at a specific host would do. The loopback front door belongs to
#      whatever deployment this session is part of.
_vogt_endpoint="${VOGT_MCP_URL:-}"
if [[ -z "$_vogt_endpoint" && -n "${VOGT_URL:-}" ]]; then
    _vogt_endpoint="${VOGT_URL%/}/mcp"
fi
readonly VOGT_ENDPOINT="${_vogt_endpoint:-http://127.0.0.1:8910/mcp}"
unset _vogt_endpoint
readonly VOGT_WRAPPER="/usr/local/bin/vogt-mcp"
readonly VOGT_SRC="${VOGT_SRC:-$HOME/Working/Active/apps/vogt}"

install_vogt_bridge() {
    # Vogt is not on PyPI, so the image cannot install the bridge at build
    # time. The workspace checkout is the only source. When vogt goes public
    # this moves into the Dockerfile and this becomes a no-op fallback.
    command -v vogt-mcp-remote >/dev/null 2>&1 && return 0
    if [[ ! -f "$VOGT_SRC/pyproject.toml" ]]; then
        printf 'mcp-bootstrap: no vogt checkout at %s; Vogt bridge unavailable\n' \
            "$VOGT_SRC" >&2
        return 0
    fi
    if ! pip3 install --user --break-system-packages --no-cache-dir --quiet \
        -e "$VOGT_SRC"; then
        printf 'mcp-bootstrap: failed to install vogt-mcp-remote from %s\n' \
            "$VOGT_SRC" >&2
        return 0
    fi
    command -v vogt-mcp-remote >/dev/null 2>&1 || printf \
        'mcp-bootstrap: vogt-mcp-remote still not on PATH after install\n' >&2
}

install_vogt_codex() {
    command -v codex >/dev/null 2>&1 || return 0
    # Reconcile the URL rather than checking the key exists: a presence-only
    # check pins whatever URL was current when the client was first registered,
    # so a moved endpoint keeps failing to hand-shake and re-running changes
    # nothing.
    if codex mcp get vogt >/dev/null 2>&1; then
        if codex mcp get vogt 2>/dev/null | grep -qF "$VOGT_ENDPOINT"; then
            return 0
        fi
        codex mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    codex mcp add vogt --url "$VOGT_ENDPOINT" \
        --bearer-token-env-var VOGT_HTTP_TOKEN >/dev/null
}

install_vogt_claude() {
    command -v claude >/dev/null 2>&1 || return 0
    if rg -q '"vogt"' \
        "$HOME/.claude.json" \
        "$HOME/.claude/.mcp.json" \
        "$PWD/.mcp.json" 2>/dev/null; then
        return 0
    fi
    claude mcp add --scope user vogt -- "$VOGT_WRAPPER" >/dev/null
}

install_vogt_opencode() {
    command -v opencode >/dev/null 2>&1 || return 0
    if rg -q '"vogt"' \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.json" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.jsonc" \
        "$PWD/opencode.json" \
        "$PWD/opencode.jsonc" 2>/dev/null; then
        return 0
    fi
    # No `--env VOGT_URL`: this registration is written once and reused by every
    # later session, so pinning a URL here freezes whichever deployment happened
    # to be current when a client was first registered — and it would *override*
    # the session's own `VOGT_URL`, the one value that knows where this session's
    # Vogt is. The wrapper reads it at spawn instead.
    opencode mcp add vogt \
        -- "$VOGT_WRAPPER" >/dev/null
}

# Vogt registrations are best-effort: a failure here must not cost an agent its
# git/gh credentials.
install_vogt_bridge
install_vogt_codex
install_vogt_claude
install_vogt_opencode

# stderr, like every other message in this script, because of who calls it.
#
# `vogt-agent-auth run` invokes this bootstrap on every launch, and one of
# the things it launches is a stdio MCP server — `vogt-mcp`. For a
# stdio MCP server, stdout *is* the transport, so a line here is not a banner:
# it is the first frame of the protocol, and it does not parse. `tests/test_bridge.py`
# already forbids this of the bridge — "a diagnostic on stdout corrupts framing
# and looks like a client bug" — and the bridge obeys it. The launcher above it
# did not, so the rule held in the one place that was tested and broke in the
# layer that wraps it.
#
# Everything an operator wants to see is still shown: an interactive shell
# prints stderr too. The only reader that notices the difference is the one
# that must.
printf 'mcp-bootstrap: Vogt MCP client registrations written; endpoint was not probed — run `vogt-agent-auth check` for that\n' >&2
