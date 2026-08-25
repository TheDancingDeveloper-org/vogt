#!/usr/bin/env bash
# Fail loudly when a persisted home volume contains a second agent CLI.
# Image-managed tools are immutable; a user-local copy is an explicit
# deployment choice, never an accidental shadow of the signed image.

set -euo pipefail

versions=/usr/local/share/vogt/agent-versions.resolved
[[ -r "$versions" ]] || exit 0

policy="${VOGT_AGENT_SHADOW_POLICY:-fail}"
failures=0

report() {
    local message="$1"
    if [[ "$policy" == "warn" || "$policy" == "allow" ]]; then
        echo "vogt-agent-clis: warning: $message" >&2
    else
        echo "vogt-agent-clis: error: $message" >&2
        failures=1
    fi
}

expected_version() {
    sed -n "s/^$1=//p" "$versions" | head -n 1
}

check_package() {
    package="$1"
    expected="$2"
    [ -n "$expected" ] || return 0
    [ -x /usr/local/bin/"$3" ] || return 0
    actual="$(npm list --global --prefix=/usr/local --depth=0 --json "$package" 2>/dev/null \
        | python3 -c 'import json,sys; data=json.load(sys.stdin); package=sys.argv[1]; print(data.get("dependencies",{}).get(package,{}).get("version", ""))' "$package" \
        || true)"
    [ "$actual" = "$expected" ] || report "$package is $actual, expected image pin $expected"
}

check_tool() {
    local name="$1" system="$2" home="$3"
    [[ -e "$system" ]] || return 0
    local selected
    selected="$(command -v "$name" || true)"
    if [[ -n "$selected" && "$(readlink -f "$selected")" != "$(readlink -f "$system")" ]]; then
        report "$name resolves to $selected, outside image-managed $system"
    fi
    if [[ -e "$home" && "$(readlink -f "$home")" != "$(readlink -f "$system")" ]]; then
        report "persisted home copy $home would shadow image-managed $system"
    fi
}

check_tool codex /usr/local/bin/codex /home/sprooty/.npm-global/bin/codex
check_tool claude /usr/local/bin/claude /home/sprooty/.npm-global/bin/claude
check_package @openai/codex "$(expected_version codex)" codex
check_package @anthropic-ai/claude-code "$(expected_version claude-code)" claude

if (( failures )); then
    echo "vogt-agent-clis: set VOGT_AGENT_SHADOW_POLICY=warn to acknowledge a deliberate user-local override" >&2
    exit 78
fi
