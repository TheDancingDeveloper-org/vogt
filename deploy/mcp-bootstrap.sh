#!/usr/bin/env bash
# Idempotently register Cadastre for clients present in the image.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

readonly CADASTRE_URL="${CADASTRE_MCP_URL:-https://winrarhost.tailc7d3c.ts.net:18081/mcp}"
readonly CADASTRE_WRAPPER="/usr/local/bin/mydevenv2-cadastre-mcp"

install_codex() {
    command -v codex >/dev/null 2>&1 || return 0
    if codex mcp get cadastre >/dev/null 2>&1; then
        return 0
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

install_codex
install_claude
install_opencode
printf 'Cadastre MCP client registrations are ready\n'
