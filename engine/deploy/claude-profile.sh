#!/usr/bin/env bash
# Prepare a named Claude Code account profile: a config directory with its own
# login that shares the main account's settings, skills and agents and has the
# same MCP servers registered. Installed as `claude-profile`.
#
#   claude-profile prepare <name> [--dir DIR] [--no-mcp]
#   claude-profile path    <name>
#   claude-profile env     <name>     # an `export CLAUDE_CONFIG_DIR=...` line
#   claude-profile list
#   claude-profile check   <name>     # can this login run an interactive session?
#
# A profile is only a directory. Select it for a shell with
# `export CLAUDE_CONFIG_DIR=$(claude-profile path <name>)`, or start an engine
# session with `account: <name>`. `prepare` writes the marker file the engine
# looks for, so a profile under the default root is usable at once, with no
# restart; one made with --dir elsewhere must be listed in
# ENGINE_AGENT_CLAUDE_ACCOUNTS.
#
# Credentials are never copied. Claude Code's OAuth refresh token rotates on
# use, so two copies of one login fight and one of them is signed out. Each
# profile logs in once, itself (`/login`).
#
# Environment:
#   CLAUDE_ACCOUNTS_ROOT      where profiles live (default ~/.claude-accounts)
#   CLAUDE_PROFILE_SHARE_FROM the directory whose settings are shared into a
#                             profile (default ~/.claude)
#   CLAUDE_PROFILE_BOOTSTRAP  the MCP bootstrap to run for a profile (default
#                             vogt-mcp-bootstrap on PATH)
#   CLAUDE_PROFILE_CHECK_MODEL the model `check` asks (default claude-haiku-5-5)
#   ANTHROPIC_BASE_URL        the API `check` calls (default
#                             https://api.anthropic.com)
set -euo pipefail

readonly ROOT="${CLAUDE_ACCOUNTS_ROOT:-$HOME/.claude-accounts}"
# The file that marks a directory under ROOT as a prepared profile, for the
# engine's account discovery. Keep in step with PROFILE_MARKER in
# engine/server/src/claude_config.rs.
readonly MARKER=.vogt-claude-profile
readonly SHARE_FROM="${CLAUDE_PROFILE_SHARE_FROM:-$HOME/.claude}"
# Settings a profile shares with the main account, by symlink. Everything else
# in a config directory is the account's own: its login, its history, its
# transcripts, its caches.
readonly SHARED=(settings.json CLAUDE.md skills agents commands rules)

die() {
    printf 'claude-profile: %s\n' "$*" >&2
    exit 1
}

valid_name() {
    [[ "$1" =~ ^[a-z0-9][a-z0-9_-]{0,31}$ ]]
}

profile_dir() {
    local name="$1"
    valid_name "$name" || die "invalid profile name '$name' (use [a-z0-9][a-z0-9_-]{0,31})"
    printf '%s/%s\n' "$ROOT" "$name"
}

# Link the shared settings in without touching anything the profile already
# has: an existing file or link wins, so a profile can diverge on purpose.
link_shared() {
    local dir="$1" item
    [[ -d "$SHARE_FROM" ]] || return 0
    [[ "$(cd "$SHARE_FROM" && pwd -P)" != "$(cd "$dir" && pwd -P)" ]] || return 0
    for item in "${SHARED[@]}"; do
        [[ -e "$SHARE_FROM/$item" ]] || continue
        [[ -e "$dir/$item" || -L "$dir/$item" ]] && continue
        ln -s "$SHARE_FROM/$item" "$dir/$item"
    done
}

# Claude Code's "Teach auto mode about your environment?" dialog has no
# environment switch; answering it writes this flag. The image's entrypoint
# sets it for the default config; a profile needs the same, or a driven
# session in it waits at a modal. Best effort, like the entrypoint's.
dismiss_auto_mode() {
    local config="$1/.claude.json"
    command -v python3 >/dev/null 2>&1 || return 0
    python3 - "$config" <<'PY' || printf 'claude-profile: could not update %s; continuing\n' "$config" >&2
import json, os, sys, tempfile
path = sys.argv[1]
try:
    with open(path, encoding="utf-8") as fh:
        config = json.load(fh)
except FileNotFoundError:
    config = {}
if not isinstance(config, dict):
    sys.exit(0)
setup = config.get("autoModeEnvSetup")
setup = dict(setup) if isinstance(setup, dict) else {}
if setup.get("dismissed") is True:
    sys.exit(0)
setup["dismissed"] = True
config["autoModeEnvSetup"] = setup
directory = os.path.dirname(os.path.abspath(path))
fd, tmp = tempfile.mkstemp(dir=directory, prefix=".claude.json.")
with os.fdopen(fd, "w", encoding="utf-8") as fh:
    json.dump(config, fh, indent=2)
if os.path.exists(path):
    os.chmod(tmp, os.stat(path).st_mode & 0o777)
os.replace(tmp, path)
PY
}

register_mcp() {
    local dir="$1" bootstrap="${CLAUDE_PROFILE_BOOTSTRAP:-vogt-mcp-bootstrap}"
    if ! command -v "$bootstrap" >/dev/null 2>&1 && [[ ! -x "$bootstrap" ]]; then
        printf 'claude-profile: %s not found; MCP servers not registered in %s\n' \
            "$bootstrap" "$dir" >&2
        return 0
    fi
    CLAUDE_CONFIG_DIR="$dir" "$bootstrap"
}

cmd_prepare() {
    local name="${1:-}" dir="" mcp=yes
    [[ -n "$name" ]] || die "usage: claude-profile prepare <name> [--dir DIR] [--no-mcp]"
    shift
    dir="$(profile_dir "$name")"
    while (($#)); do
        case "$1" in
            --dir)
                [[ -n "${2:-}" ]] || die "--dir needs a directory"
                [[ "$2" = /* ]] || die "--dir must be an absolute path"
                dir="$2"
                shift 2
                ;;
            --no-mcp)
                mcp=no
                shift
                ;;
            *) die "unknown option '$1'" ;;
        esac
    done
    mkdir -p "$dir"
    chmod 700 "$dir"
    link_shared "$dir"
    dismiss_auto_mode "$dir"
    [[ "$mcp" == no ]] || register_mcp "$dir"
    [[ -f "$dir/$MARKER" ]] || : >"$dir/$MARKER"
    printf 'profile %s ready: %s\n' "$name" "$dir"
    if [[ -f "$dir/.credentials.json" ]]; then
        printf 'login: present\n'
    else
        printf 'login: none yet. Run once:  CLAUDE_CONFIG_DIR=%q claude   then /login\n' "$dir"
    fi
    if [[ "$dir" == "$ROOT/$name" ]]; then
        printf 'the engine finds it as account %s (if its accounts root is %s)\n' "$name" "$ROOT"
    else
        printf 'to let the engine use it, add  %s=%s  to ENGINE_AGENT_CLAUDE_ACCOUNTS\n' "$name" "$dir"
    fi
}

cmd_list() {
    local dir
    [[ -d "$ROOT" ]] || return 0
    for dir in "$ROOT"/*/; do
        [[ -d "$dir" ]] || continue
        dir="${dir%/}"
        if [[ -f "$dir/.credentials.json" ]]; then
            printf '%s\t%s\tlogged-in\n' "$(basename "$dir")" "$dir"
        else
            printf '%s\t%s\tno-login\n' "$(basename "$dir")" "$dir"
        fi
    done
}

# An API-key profile (`apiKeyHelper` in its settings) can pass every
# `claude -p` test and still fail each interactive session on its first turn
# with "Credit balance too low" (WI-2741). The API bills a Claude Code request
# by the entry point in its attribution line: `sdk-cli` (print mode, the Agent
# SDK) and plain API calls are accepted, `cli` (an interactive session, which
# is what the engine runs) is refused when the organisation has no API credit
# for it. So the check sends one minimal request shaped as an interactive
# session's and reports a refusal. The key goes to curl on stdin, never argv.
cmd_check() {
    local name="${1:-}" dir settings helper key base model version body out code
    [[ -n "$name" ]] || die "usage: claude-profile check <name>"
    dir="$(profile_dir "$name")"
    [[ -d "$dir" ]] || die "no profile '$name' at $dir"
    settings="$dir/settings.json"
    helper=""
    [[ -f "$settings" ]] && helper="$(jq -r '.apiKeyHelper // empty' "$settings" 2>/dev/null || true)"
    if [[ -z "$helper" ]]; then
        printf 'profile %s: no apiKeyHelper; a subscription login is not checked here\n' "$name"
        return 0
    fi
    key="$(CLAUDE_CONFIG_DIR="$dir" bash -c "$helper" 2>/dev/null || true)"
    [[ -n "$key" ]] || die "profile $name: apiKeyHelper printed no key"
    base="${ANTHROPIC_BASE_URL:-https://api.anthropic.com}"
    model="${CLAUDE_PROFILE_CHECK_MODEL:-claude-haiku-5-5}"
    # The API only accepts the attribution line whole, version first.
    version="$(claude --version 2>/dev/null | awk '{print $1; exit}')"
    body="$(jq -nc --arg model "$model" --arg v "${version:-0.0.0}" '{model: $model, max_tokens: 1,
        system: [{type: "text", text: "x-anthropic-billing-header: cc_version=\($v); cc_entrypoint=cli;"}],
        messages: [{role: "user", content: "Reply OK"}]}')"
    out="$(mktemp)"
    code="$(printf 'x-api-key: %s\n' "$key" | curl -sS -o "$out" -w '%{http_code}' \
        -H @- -H 'anthropic-version: 2023-06-01' -H 'content-type: application/json' \
        -H 'anthropic-beta: claude-code-20250219' \
        --data-binary "$body" "$base/v1/messages?beta=true" || true)"
    key=""
    if [[ "$code" == 200 ]]; then
        rm -f "$out"
        printf 'profile %s: ok, the API key can run interactive Claude Code sessions\n' "$name"
        return 0
    fi
    if grep -qi 'credit balance' "$out"; then
        rm -f "$out"
        printf 'profile %s: the API refuses interactive Claude Code requests: credit balance too low.\n' "$name" >&2
        printf '  claude -p and plain API calls with this key still work; engine sessions do not.\n' >&2
        printf '  Add API credit to the key'"'"'s organisation (Console, Plans & Billing), then check again.\n' >&2
        return 1
    fi
    printf 'profile %s: check request failed (HTTP %s): %s\n' "$name" "${code:-none}" \
        "$(jq -r '.error.message // empty' "$out" 2>/dev/null | head -c 200)" >&2
    rm -f "$out"
    return 1
}

case "${1:-}" in
    check)
        cmd_check "${2:-}"
        ;;
    prepare)
        shift
        cmd_prepare "$@"
        ;;
    path)
        profile_dir "${2:-}"
        ;;
    env)
        printf 'export CLAUDE_CONFIG_DIR=%q\n' "$(profile_dir "${2:-}")"
        ;;
    list)
        cmd_list
        ;;
    *)
        sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//' >&2
        exit 2
        ;;
esac
