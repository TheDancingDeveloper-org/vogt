#!/usr/bin/env bash
# Fail loudly when the agent CLI a session would run is not the one the pod
# says it runs.
#
# Two places may legitimately hold a CLI: the image (`/usr/local`, the baked
# baseline from engine/agent-versions.env) and the runtime prefix that
# `vogt-agent-cli-install` manages under $VOGT_AGENT_CLI_ROOT (#590). The
# question asked here is "does the active CLI match the manifest?" — the
# runtime manifest when a pin has been applied, the image's resolved versions
# otherwise. A copy anywhere else — above all a persisted `~/.npm-global` that
# would shadow both — is not sanctioned: the runtime prefix is the only home
# for a non-image copy, and the CLI's own updater stays off
# (`DISABLE_UPDATES=1`, #196), so `vogt-agent-cli-install` is the only writer.
#
# What happens to such a stray is VOGT_AGENT_SHADOW_POLICY:
#   quarantine (default)  move it aside (`<path>.shadowed-<epoch>`), warn, and
#                         boot — the managed copy stays the one that runs, and
#                         a stray can no longer keep the whole pod (core, live
#                         terminals, everything) from starting. Refusing to
#                         boot was what a `codex` "Update now" into the
#                         persisted home did to vogt-dev on 2026-09-05.
#   fail                  the strict gate: report and exit 78, refuse to boot.
#   warn / allow          leave it, warn (or say nothing): a deliberate
#                         user-local override the operator has acknowledged.
# A stray that cannot be moved is still an error under quarantine — integrity
# cannot be guaranteed, so the pod refuses as `fail` would.

set -euo pipefail

versions="${VOGT_AGENT_CLI_BAKED_MANIFEST:-/usr/local/share/vogt/agent-versions.resolved}"
root="${VOGT_AGENT_CLI_ROOT:-/opt/vogt/agent-clis}"
manifest="$root/manifest"
# Where the image's copies live and where a persisted home would put a stray
# one. Overridable so the script can be exercised outside the image.
image_bin="${VOGT_AGENT_CLI_IMAGE_BIN:-/usr/local/bin}"
home_bin="${VOGT_AGENT_CLI_HOME_BIN:-${HOME:-/nonexistent}/.npm-global/bin}"
tools_table="${VOGT_AGENT_CLI_TOOLS:-/usr/local/share/vogt/agent-clis.tools}"
[[ -r "$versions" || -r "$manifest" ]] || exit 0

policy="${VOGT_AGENT_SHADOW_POLICY:-quarantine}"
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

# The runtime manifest wins when it names the tool; the image's resolved pin
# is the answer when no runtime pin has ever been applied.
expected_version() {
    local value=""
    if [[ -r "$manifest" ]]; then
        value="$(sed -n "s/^$1=//p" "$manifest" | head -n 1)"
    fi
    if [[ -z "$value" && -r "$versions" ]]; then
        value="$(sed -n "s/^$1=//p" "$versions" | head -n 1)"
    fi
    printf '%s' "$value"
}

# The npm prefix whose copy a session would run: the runtime `current` when
# it exists, else the image's /usr/local.
active_prefix() {
    local current="$root/$1/current"
    if [[ -L "$current" && -d "$current" ]]; then
        readlink -f "$current"
    else
        dirname "$image_bin"
    fi
}

check_package() {
    package="$1"
    expected="$2"
    tool="$3"
    binary="$4"
    [ -n "$expected" ] || return 0
    prefix="$(active_prefix "$tool")"
    [ -x "$prefix/bin/$binary" ] || return 0
    actual="$(npm list --global --prefix="$prefix" --depth=0 --json "$package" 2>/dev/null \
        | python3 -c 'import json,sys; data=json.load(sys.stdin); package=sys.argv[1]; print(data.get("dependencies",{}).get(package,{}).get("version", ""))' "$package" \
        || true)"
    [ "$actual" = "$expected" ] || report "$package is $actual at $prefix, expected pin $expected"
}

check_tool() {
    local name="$1" tool="$2" system="$3" home="$4"
    local runtime="$root/$tool/current/bin/$name"
    [[ -e "$system" || -e "$runtime" ]] || return 0
    local selected
    selected="$(command -v "$name" || true)"
    if [[ -n "$selected" ]]; then
        local resolved
        resolved="$(readlink -f "$selected")"
        if [[ "$resolved" != "$(readlink -f "$system")" ]] \
            && { [[ ! -e "$runtime" ]] || [[ "$resolved" != "$(readlink -f "$runtime")" ]]; }; then
            report "$name resolves to $selected, outside image-managed $system and the runtime prefix $root"
        fi
    fi
    if [[ -e "$home" && "$(readlink -f "$home")" != "$(readlink -f "$system")" ]]; then
        if [[ "$policy" == "quarantine" ]]; then
            quarantine_shadow "$home" "$system"
        else
            report "persisted home copy $home would shadow image-managed $system"
        fi
    fi
}

# Move a stray persisted-home copy out of the way and carry on. The managed
# copy is what runs either way (PATH prefers the runtime prefix and /usr/local);
# quarantining keeps that true without making the pod's boot depend on it.
quarantine_shadow() {
    local home="$1" system="$2" moved
    moved="$home.shadowed-$(date +%s)"
    if mv -f "$home" "$moved" 2>/dev/null; then
        echo "vogt-agent-clis: warning: persisted home copy $home would shadow image-managed $system; quarantined to $moved (#196)" >&2
        return 0
    fi
    report "persisted home copy $home would shadow image-managed $system and could not be quarantined"
}

# Every tool the image knows (`agent-clis.tools`: tool, package, binary, env
# var), so adding one to the image adds it here without an edit.
if [[ -r "$tools_table" ]]; then
    while IFS=$'\t' read -r tool package binary _; do
        [[ -n "$tool" && "$tool" != \#* ]] || continue
        check_tool "$binary" "$tool" "$image_bin/$binary" "$home_bin/$binary"
        check_package "$package" "$(expected_version "$tool")" "$tool" "$binary"
    done < "$tools_table"
else
    # An image built before the table existed: the two CLIs it always carried.
    check_tool codex codex "$image_bin/codex" "$home_bin/codex"
    check_tool claude claude-code "$image_bin/claude" "$home_bin/claude"
    check_package @openai/codex "$(expected_version codex)" codex codex
    check_package @anthropic-ai/claude-code "$(expected_version claude-code)" claude-code claude
fi

if (( failures )); then
    echo "vogt-agent-clis: set VOGT_AGENT_SHADOW_POLICY=warn to acknowledge a deliberate user-local override (the default quarantines a stray copy and boots)" >&2
    exit 78
fi
