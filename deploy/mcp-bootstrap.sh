#!/usr/bin/env bash
# Idempotently register Cadastre for clients present in the image, and make
# sure the stdio bridge they point at actually exists.
# Registration stores only the endpoint and wrapper command; no bearer value.
set -euo pipefail

readonly CADASTRE_URL="${CADASTRE_MCP_URL:-https://winrarhost.tailc7d3c.ts.net:18092/mcp}"
readonly CADASTRE_WRAPPER="/usr/local/bin/mydevenv2-cadastre-mcp"
readonly CADASTRE_SRC="${MYDEVENV2_CADASTRE_SRC:-$HOME/Working/Active/cadastre}"

readonly VOGT_URL="${VOGT_MCP_URL:-https://winrarhost.tailc7d3c.ts.net:18094/mcp}"
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
        if codex mcp get vogt 2>/dev/null | grep -qF "$VOGT_URL"; then
            return 0
        fi
        codex mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    codex mcp add vogt --url "$VOGT_URL" \
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
    opencode mcp add vogt \
        --env "VOGT_URL=${VOGT_URL%/mcp}" \
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

install_bridge
install_codex
install_claude
install_opencode

# Vogt registrations are best-effort in the same way: a failure here must not
# cost an agent its git/gh credentials.
install_vogt_bridge
install_vogt_codex
install_vogt_claude
install_vogt_opencode

printf 'Cadastre and Vogt MCP client registrations are ready\n'
