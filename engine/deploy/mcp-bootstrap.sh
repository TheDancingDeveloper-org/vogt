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

# Whether Claude Code already runs `name` with `command` (and `arg`, when
# given), read straight from its config (WI-927). `claude mcp get` answers the
# same question but starts node and health-checks the server, ~0.7 s a call,
# and this script asks it several times on every session launch. Only a "yes"
# is trusted: anything else (no jq, no file, a project-scope .mcp.json, a stale
# entry) falls through to the CLI path below, which is unchanged.
claude_runs() {
    local name="$1" command="$2" arg="${3:-}" config="$HOME/.claude.json"
    command -v jq >/dev/null 2>&1 && [[ -f "$config" ]] || return 1
    jq -e --arg n "$name" --arg c "$command" --arg a "$arg" --arg cwd "$PWD" '
        [.mcpServers[$n]?, .projects[$cwd].mcpServers[$n]?]
        | map(select(. != null))
        | length > 0 and all(.command == $c
            and ($a == "" or ((.args // []) | index($a)) != null))' \
        "$config" >/dev/null 2>&1
}

# Whether `name` appears in no Claude Code config this session can see: user
# or local scope in ~/.claude.json, or the project's .mcp.json. Lets an
# unwanted read-only server be skipped without asking the CLI to look for it.
claude_lacks() {
    local name="$1" config="$HOME/.claude.json"
    command -v jq >/dev/null 2>&1 && [[ -f "$config" ]] || return 1
    jq -e --arg n "$name" --arg cwd "$PWD" '
        (.mcpServers[$n]? == null) and (.projects[$cwd].mcpServers[$n]? == null)' \
        "$config" >/dev/null 2>&1 || return 1
    [[ ! -f "$PWD/.mcp.json" ]] || ! grep -qF "\"$name\"" "$PWD/.mcp.json"
}

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
    local registered
    if registered="$(codex mcp get vogt 2>/dev/null)"; then
        if grep -qF "$VOGT_ENDPOINT" <<<"$registered"; then
            return 0
        fi
        codex mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    codex mcp add vogt --url "$VOGT_ENDPOINT" \
        --bearer-token-env-var VOGT_HTTP_TOKEN >/dev/null
}

install_vogt_claude() {
    command -v claude >/dev/null 2>&1 || return 0
    # Reconcile the COMMAND, not mere name-presence. A name-only guard pins
    # whatever launcher path was stored when the client was first registered, so
    # a renamed or relocated wrapper (e.g. an older `mydevenv2-*` path carried
    # into a new image) leaves a stale `command` that Claude fails to spawn
    # (`ENOENT`), and re-running never heals it. Mirror the codex branch:
    # replace the registration unless its command already is the current wrapper.
    claude_runs vogt "$VOGT_WRAPPER" && return 0
    if claude mcp get vogt >/dev/null 2>&1; then
        if claude mcp get vogt 2>/dev/null | grep -qF "$VOGT_WRAPPER"; then
            return 0
        fi
        # No --scope: remove wherever the stale entry lives.
        claude mcp remove vogt >/dev/null 2>&1 || return 0
    fi
    claude mcp add --scope user vogt -- "$VOGT_WRAPPER" >/dev/null
}

install_vogt_opencode() {
    command -v opencode >/dev/null 2>&1 || return 0
    # Reconcile the COMMAND, not mere name-presence (same stale-launcher bug as
    # the claude branch). opencode has no `mcp remove`, but `mcp add` upserts,
    # so re-adding overwrites a stale command with the current wrapper. Skip only
    # when the current wrapper is already the registered command.
    if rg -qF "$VOGT_WRAPPER" \
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

# Optional read-only MCP servers (docs/ENGINE.md §4). Each is registered only
# while this session holds its token, and unregistered when it does not, so a
# deployment opts in by adding the token to its agent-auth manifest and opts
# out by removing it. The registration names `vogt-readonly-mcp <server>` and
# nothing else: the token stays in the session's environment, read at spawn,
# and the read-only switches live in the wrapper where a session cannot edit
# them. Names end in `-ro` so a server an operator registered by hand under
# the plain name is never replaced or removed.
# Overridable only so the registration logic can be exercised outside the image.
readonly READONLY_WRAPPER="${VOGT_READONLY_MCP_WRAPPER:-/usr/local/bin/vogt-readonly-mcp}"
# name | wrapper argument | upstream binary | variables that enable it (all
# required) and that Codex must pass through to the server (Codex starts stdio
# servers with a minimal environment unless told which variables to forward).
readonly READONLY_SERVERS=(
    "github-ro|github|github-mcp-server|GITHUB_MCP_TOKEN|VOGT_GITHUB_MCP_TOOLSETS"
    "grafana-ro|grafana|mcp-grafana|GRAFANA_URL GRAFANA_SERVICE_ACCOUNT_TOKEN|"
    "forgejo-ro|gitea|gitea-mcp|GITEA_HOST GITEA_MCP_TOKEN|"
)

readonly_wanted() {
    local var
    for var in $1; do
        [[ -n "${!var:-}" ]] || return 1
    done
    return 0
}

readonly_codex() {
    local name="$1" arg="$2" required="$3" optional="$4" config vars var
    command -v codex >/dev/null 2>&1 || return 0
    config="${CODEX_HOME:-$HOME/.codex}/config.toml"
    if [[ "$5" != "yes" ]]; then
        # Remove only what this script wrote: an entry running the wrapper.
        if codex mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
            codex mcp remove "$name" >/dev/null 2>&1 || true
        fi
        return 0
    fi
    vars=""
    for var in $required $optional; do
        vars+="${vars:+, }\"$var\""
    done
    if codex mcp get "$name" --json 2>/dev/null | tr -d ' \n' \
        | grep -qF "\"env_vars\":[${vars// /}]"; then
        codex mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER" && return 0
    fi
    codex mcp remove "$name" >/dev/null 2>&1 || true
    # `codex mcp add` cannot set `env_vars`, so the table is written whole. A
    # new table appended at the end of the file is valid TOML wherever the
    # file's other tables are.
    mkdir -p "$(dirname "$config")"
    printf '\n[mcp_servers.%s]\ncommand = "%s"\nargs = ["%s"]\nenv_vars = [%s]\n' \
        "$name" "$READONLY_WRAPPER" "$arg" "$vars" >>"$config"
}

readonly_claude() {
    local name="$1" arg="$2"
    command -v claude >/dev/null 2>&1 || return 0
    if [[ "$3" != "yes" ]]; then
        claude_lacks "$name" && return 0
        if claude mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
            claude mcp remove --scope user "$name" >/dev/null 2>&1 || true
        fi
        return 0
    fi
    # Claude Code hands a stdio server the session's own environment, so the
    # registration needs no `-e`: an `-e TOKEN=...` would store the value.
    claude_runs "$name" "$READONLY_WRAPPER" "$arg" && return 0
    if claude mcp get "$name" 2>/dev/null | grep -qF "$READONLY_WRAPPER"; then
        return 0
    fi
    claude mcp remove --scope user "$name" >/dev/null 2>&1 || true
    claude mcp add --scope user "$name" -- "$READONLY_WRAPPER" "$arg" >/dev/null
}

install_readonly_servers() {
    local entry name arg bin required optional wanted
    for entry in "${READONLY_SERVERS[@]}"; do
        IFS='|' read -r name arg bin required optional <<<"$entry"
        wanted=no
        if readonly_wanted "$required"; then
            if [[ -x "$READONLY_WRAPPER" ]] && command -v "$bin" >/dev/null 2>&1; then
                wanted=yes
            else
                printf 'mcp-bootstrap: %s token present but %s or %s is not installed; not registered\n' \
                    "$name" "$READONLY_WRAPPER" "$bin" >&2
            fi
        fi
        readonly_codex "$name" "$arg" "$required" "$optional" "$wanted" \
            || printf 'mcp-bootstrap: codex registration of %s failed\n' "$name" >&2
        readonly_claude "$name" "$arg" "$wanted" \
            || printf 'mcp-bootstrap: claude registration of %s failed\n' "$name" >&2
    done
}

# Vogt registrations are best-effort: a failure here must not cost an agent its
# git/gh credentials.
install_vogt_bridge
install_vogt_codex
install_vogt_claude
install_vogt_opencode
install_readonly_servers

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
