#!/usr/bin/env bash
# Idempotently register Vogt for clients present in the image. Cadastre is a
# separate, optional private-stack integration and is registered only when
# CADASTRE_MCP_ENABLED=1.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

# Cadastre is opt-in and has no built-in endpoint: the operator sets
# CADASTRE_MCP_URL, or the registration below is skipped.
readonly CADASTRE_URL="${CADASTRE_MCP_URL:-}"
readonly CADASTRE_WRAPPER="/usr/local/bin/mydevenv2-cadastre-mcp"
readonly CADASTRE_SRC="${MYDEVENV2_CADASTRE_SRC:-$HOME/Working/Active/cadastre}"
readonly CADASTRE_MCP_ENABLED="${CADASTRE_MCP_ENABLED:-0}"

# Where an agent in this session should reach Vogt, in the order of what
# actually knows the answer:
#
#   1. `VOGT_MCP_URL`, an explicit override. Unchanged.
#   2. The session's own `VOGT_URL`. `vogt session start` exports the endpoint
#      the operator configured for clients (FR-E5), and it is the only thing
#      here that knows which deployment this session belongs to. Ignoring it
#      is what this script used to do.
#   3. The front door on loopback. In the merged stack the engine is the only
#      published port (NFR-D11) and the agent runs in this container, so
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
readonly VOGT_WRAPPER="/usr/local/bin/mydevenv2-vogt-mcp"
readonly VOGT_SRC="${MYDEVENV2_VOGT_SRC:-$HOME/Working/Active/apps/vogt}"

install_bridge() {
    # The wrapper execs `cadastre-mcp-remote`, a console script of the cadastre
    # package. The image cannot install it at build time — the package lives in
    # the mounted workspace, not the build context — so install it here, on the
    # first authenticated session. Editable install so the bridge tracks the
    # checkout; --user lands the shim in ~/.local/bin, which is on PATH.
    command -v cadastre-mcp-remote >/dev/null 2>&1 && return 0
    if [[ ! -f "$CADASTRE_SRC/pyproject.toml" ]]; then
        printf 'mcp-bootstrap: no cadastre checkout at %s; Claude Code/OpenCode bridge unavailable\n' \
            "$CADASTRE_SRC" >&2
        return 0
    fi
    # Non-fatal: agent auth must keep working for git/gh even if this fails.
    if ! pip3 install --user --break-system-packages --no-cache-dir --quiet \
        -e "${CADASTRE_SRC}[mcp-client]"; then
        printf 'mcp-bootstrap: failed to install cadastre-mcp-remote from %s\n' \
            "$CADASTRE_SRC" >&2
        return 0
    fi
    command -v cadastre-mcp-remote >/dev/null 2>&1 || printf \
        'mcp-bootstrap: cadastre-mcp-remote still not on PATH after install\n' >&2
}

install_vogt_bridge() {
    # Vogt is private, so unlike cadastre it is not on PyPI and the image
    # cannot install it at build time. The workspace checkout is the only
    # source, which is exactly the case install_bridge above was originally
    # written for. When vogt goes public this moves into the Dockerfile and
    # this becomes the same no-op fallback.
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
    # Reconcile the URL rather than checking the key exists — same lesson the
    # cadastre :18081 -> :18092 move taught below.
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
    # No `--env VOGT_URL`, unlike the cadastre registration below. This
    # registration is written once and reused by every later session, so
    # pinning a URL here freezes whichever deployment happened to be current
    # when a client was first registered — and it would *override* the
    # session's own `VOGT_URL`, which is the one value that knows where this
    # session's Vogt is. The wrapper reads it at spawn instead.
    opencode mcp add vogt \
        -- "$VOGT_WRAPPER" >/dev/null
}

install_codex() {
    command -v codex >/dev/null 2>&1 || return 0
    # Codex stores the ENDPOINT in its own config, unlike the Claude Code and
    # OpenCode registrations which point at the wrapper and pick the URL up at
    # spawn. A presence-only check therefore pins whatever URL was current when
    # the client was first registered: when the stack moved from :18081 to
    # :18092 every already-registered Codex kept failing to hand-shake, and
    # re-running this script changed nothing. Reconcile the value, don't just
    # check that a key exists.
    if codex mcp get cadastre >/dev/null 2>&1; then
        if codex mcp get cadastre 2>/dev/null | grep -qF "$CADASTRE_URL"; then
            return 0
        fi
        codex mcp remove cadastre >/dev/null 2>&1 || return 0
    fi
    codex mcp add cadastre --url "$CADASTRE_URL" \
        --bearer-token-env-var CADASTRE_HTTP_TOKEN >/dev/null
}

install_claude() {
    command -v claude >/dev/null 2>&1 || return 0
    if rg -q '"cadastre"' \
        "$HOME/.claude.json" \
        "$HOME/.claude/.mcp.json" \
        "$PWD/.mcp.json" 2>/dev/null; then
        return 0
    fi
    claude mcp add --scope user cadastre -- "$CADASTRE_WRAPPER" >/dev/null
}

install_opencode() {
    command -v opencode >/dev/null 2>&1 || return 0
    if rg -q '"cadastre"' \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.json" \
        "${XDG_CONFIG_HOME:-$HOME/.config}/opencode/opencode.jsonc" \
        "$PWD/opencode.json" \
        "$PWD/opencode.jsonc" 2>/dev/null; then
        return 0
    fi
    opencode mcp add cadastre \
        --env "CADASTRE_MCP_URL=$CADASTRE_URL" \
        --env 'CADASTRE_REMOTE_ONLY=1' \
        -- "$CADASTRE_WRAPPER" >/dev/null
}

if [[ "$CADASTRE_MCP_ENABLED" == "1" && -n "$CADASTRE_URL" ]]; then
    install_bridge
    install_codex
    install_claude
    install_opencode
elif [[ "$CADASTRE_MCP_ENABLED" == "1" ]]; then
    printf 'mcp-bootstrap: CADASTRE_MCP_ENABLED=1 but CADASTRE_MCP_URL is not set; skipping Cadastre registration\n' >&2
fi

# Vogt registrations are best-effort in the same way: a failure here must not
# cost an agent its git/gh credentials.
install_vogt_bridge
install_vogt_codex
install_vogt_claude
install_vogt_opencode

# stderr, like every other message in this script, because of who calls it.
#
# `mydevenv2-agent-auth run` invokes this bootstrap on every launch, and two
# of the things it launches are stdio MCP servers — `mydevenv2-vogt-mcp` and
# `mydevenv2-cadastre-mcp`. For a stdio MCP server, stdout *is* the transport,
# so a line here is not a banner: it is the first frame of the protocol, and
# it does not parse. `tests/test_bridge.py` already forbids this of the bridge
# — "a diagnostic on stdout corrupts framing and looks like a client bug" —
# and the bridge obeys it. The launcher above it did not, so the rule held in
# the one place that was tested and broke in the layer that wraps it.
#
# Everything an operator wants to see is still shown: an interactive shell
# prints stderr too. The only reader that notices the difference is the one
# that must.
if [[ "$CADASTRE_MCP_ENABLED" == "1" && -n "$CADASTRE_URL" ]]; then
    printf 'mcp-bootstrap: Vogt and optional Cadastre MCP client registrations written; endpoints were not probed — run `mydevenv2-agent-auth check` for that\n' >&2
else
    printf 'mcp-bootstrap: Vogt MCP client registrations written; endpoint was not probed — run `mydevenv2-agent-auth check` for that\n' >&2
fi
