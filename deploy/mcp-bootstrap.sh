#!/usr/bin/env bash
# Idempotently register Cadastre for clients present in the image, and make
# sure the stdio bridge they point at actually exists.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

readonly CADASTRE_URL="${CADASTRE_MCP_URL:-https://winrarhost.tailc7d3c.ts.net:18092/mcp}"
readonly CADASTRE_WRAPPER="/usr/local/bin/mydevenv2-cadastre-mcp"
readonly CADASTRE_SRC="${MYDEVENV2_CADASTRE_SRC:-$HOME/Working/Active/cadastre}"

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

install_bridge
install_codex
install_claude
install_opencode
printf 'Cadastre MCP client registrations are ready\n'
