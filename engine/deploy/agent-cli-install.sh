#!/usr/bin/env bash
# Runtime-pinned agent CLIs (#590).
#
# The engine image bakes the agent CLIs (Claude Code, Codex, ...) at the versions in
# `engine/agent-versions.env`, and every update path inside the running pod is
# deliberately closed (`DISABLE_UPDATES=1`, #196). That made a new CLI version
# cost a Renovate PR, an image build, a promotion and a release cut — with
# upstream shipping roughly one release per working day. This script moves the
# pin from build time to deploy time without giving up "a pin is the contract":
#
#   vogt-agent-cli-install <tool> <version>
#
# installs `<version>` of `<tool>` (claude-code, codex, ...) into a
# versioned prefix under a volume-backed root and flips a `current` symlink
# that PATH prefers. The image's /usr/local copy is untouched and remains the
# fallback: `<version>` = `image` (or the baked version itself) points the pod
# back at it with no network access, and a failed install leaves `current`
# exactly where it was.
#
# Layout under $VOGT_AGENT_CLI_ROOT (default /opt/vogt/agent-clis):
#
#   claude-code/2.1.261/        one `npm --prefix` install per version
#   claude-code/current -> 2.1.261
#   codex/0.149.1/  codex/current -> 0.149.1
#   bin/claude -> ../claude-code/current/bin/claude   (PATH-first)
#   manifest                    active version per tool, `<tool>=<version>`
#
# Which tools exist, which npm package each is and what its binary is called
# comes from a table the image writes beside its resolved versions
# (`agent-clis.tools`, tab-separated: tool, package, binary, env var) — so
# this script names no tool and a deployment that bakes a different set needs
# no change here. Codex is the one exception in *shape*: it gets no `bin/`
# link because `/usr/local/bin/codex` is the codex-full-access wrapper, which
# itself prefers `codex/current/bin/codex` when it exists, so the image's entry
# point and PATH lookup agree without a second copy of the bypass flags.
#
# Versions are exact by default. The npm dist-tags `latest` and `stable` are
# accepted only with VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1, so the human gate that
# caught 2.1.237 stays the default and floating is a deliberate opt-in.
#
# Exit status: 0 on success (including "already current"), 1 when the requested
# version could not be installed or failed its smoke check, 64 (EX_USAGE) for a
# bad tool or version string. Only `vogt-verify-agent-clis` decides whether the
# pod may start; this script only changes what `current` points at.

set -euo pipefail

root="${VOGT_AGENT_CLI_ROOT:-/opt/vogt/agent-clis}"
baked_manifest="${VOGT_AGENT_CLI_BAKED_MANIFEST:-/usr/local/share/vogt/agent-versions.resolved}"
tools_table="${VOGT_AGENT_CLI_TOOLS:-/usr/local/share/vogt/agent-clis.tools}"
keep="${VOGT_AGENT_CLI_KEEP:-3}"
allow_dist_tags="${VOGT_AGENT_CLI_ALLOW_DIST_TAGS:-0}"

log() { echo "vogt-agent-cli-install: $*" >&2; }
die() { log "error: $1"; exit "${2:-1}"; }

usage() {
    cat >&2 <<'USAGE'
usage: vogt-agent-cli-install <tool> <version>
  tool     a tool named in the image's agent CLI table (claude-code, codex, ...)
  version  an exact npm version (2.1.261), `image` for the baked copy, or —
           only with VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1 — the dist-tag `latest`
           or `stable`
USAGE
    exit 64
}

# One column of the tools table for a tool: 2 = npm package, 3 = binary.
table_field() {
    local tool="$1" column="$2"
    [[ -r "$tools_table" ]] || return 1
    awk -F'\t' -v tool="$tool" -v column="$column" \
        '$1 == tool { print $column; found = 1; exit } END { exit !found }' "$tools_table"
}
package_for() { table_field "$1" 2; }
binary_for() { table_field "$1" 3; }
all_tools() {
    [[ -r "$tools_table" ]] || return 0
    awk -F'\t' 'NF >= 4 && $1 !~ /^#/ { print $1 }' "$tools_table"
}

baked_version() {
    [[ -r "$baked_manifest" ]] || return 0
    sed -n "s/^$1=//p" "$baked_manifest" | head -n 1
}

# The version `current` points at, or nothing when the image copy is active.
active_version() {
    local link="$root/$1/current"
    [[ -L "$link" ]] || return 0
    basename "$(readlink "$link")"
}

# Rewrite `manifest` from what the filesystem says, never from what this run
# thinks it did — so a crash between two steps leaves a manifest that is
# still true.
write_manifest() {
    local tool version tmp
    tmp="$root/manifest.tmp.$$"
    for tool in $(all_tools); do
        version="$(active_version "$tool")"
        [[ -n "$version" ]] || version="$(baked_version "$tool")"
        printf '%s=%s\n' "$tool" "${version:-}"
    done > "$tmp"
    mv -f "$tmp" "$root/manifest"
}

# Point `current` (and the PATH-first bin link) at a versioned prefix.
# `ln -sfn` onto a temp name plus `mv -T` so a reader never sees a half-state.
flip_current() {
    local tool="$1" version="$2" binary
    binary="$(binary_for "$tool")"
    ln -sfn "$version" "$root/$tool/current.tmp.$$"
    mv -fT "$root/$tool/current.tmp.$$" "$root/$tool/current"
    if [[ "$binary" != "codex" ]]; then
        mkdir -p "$root/bin"
        ln -sfn "../$tool/current/bin/$binary" "$root/bin/$binary.tmp.$$"
        mv -fT "$root/bin/$binary.tmp.$$" "$root/bin/$binary"
    fi
}

# Back to the image copy: no `current`, no bin link, nothing on PATH ahead of
# /usr/local/bin. Version directories are kept so a later flip is offline.
reset_to_image() {
    local tool="$1" binary
    binary="$(binary_for "$tool")"
    rm -f "$root/$tool/current" "$root/bin/$binary"
}

# Keep the newest $keep version directories (by mtime) plus whatever `current`
# names; delete the rest. Running sessions hold open files from the prefix
# they started with, so a version that is still in use is not removed — a
# prefix is deleted only when it has been superseded $keep times over.
prune() {
    local tool="$1" active dir count=0
    active="$(active_version "$tool")"
    (( keep > 0 )) || return 0
    while IFS= read -r dir; do
        [[ -n "$dir" ]] || continue
        [[ "$(basename "$dir")" == "$active" ]] && continue
        count=$(( count + 1 ))
        if (( count > keep )); then
            log "pruning $tool $(basename "$dir")"
            rm -rf "$dir"
        fi
    done < <(find "$root/$tool" -mindepth 1 -maxdepth 1 -type d ! -name '.tmp-*' \
                -printf '%T@ %p\n' 2>/dev/null | sort -rn | cut -d' ' -f2-)
}

# The install must prove itself the way the Dockerfile's does (#505): `npm
# install -g` exiting 0 is not evidence that the binary runs — 2.1.237 shipped
# `bin/claude.exe` as a shell stub. `--version` has to succeed, and when it
# prints a version at all it has to be the one asked for; a CLI that prints
# nothing version-shaped is held to exit status alone.
smoke_check() {
    local tool="$1" version="$2" prefix="$3" binary output
    binary="$(binary_for "$tool")"
    [[ -x "$prefix/bin/$binary" ]] || { log "$prefix/bin/$binary is missing or not executable"; return 1; }
    if ! output="$("$prefix/bin/$binary" --version 2>&1)"; then
        log "$binary --version failed: ${output:-<no output>}"
        return 1
    fi
    if [[ "$output" =~ [0-9]+\.[0-9]+\.[0-9]+ && "$output" != *"$version"* ]]; then
        log "$binary --version says '${output}', not $version — a stub install (#147/#148, #505)"
        return 1
    fi
}

main() {
    (( $# == 2 )) || usage
    local tool="$1" requested="$2" package binary version baked
    package="$(package_for "$tool")" || die "'$tool' is not a tool this image knows (see $tools_table)" 64
    binary="$(binary_for "$tool")"
    baked="$(baked_version "$tool")"

    mkdir -p "$root/$tool" "$root/bin"

    # Resolve the request to an exact version, or to the image copy.
    case "$requested" in
        ""|image)
            version=""
            ;;
        latest|stable)
            if [[ "$allow_dist_tags" != "1" ]]; then
                die "'$requested' is an npm dist-tag; the pin is exact by default. Set VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1 to float on purpose (the 2.1.237 gate is what you are opting out of)" 64
            fi
            version="$(npm view "$package@$requested" version 2>/dev/null | tail -n 1 | tr -d '[:space:]')"
            [[ -n "$version" ]] || die "could not resolve $package@$requested against npm"
            log "$tool $requested resolves to $version"
            ;;
        *)
            if [[ ! "$requested" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]]; then
                die "'$requested' is not an exact version (like 2.1.261), 'image', or an allowed dist-tag" 64
            fi
            version="$requested"
            ;;
    esac

    # The baked version is the image copy: nothing to download, nothing on
    # PATH ahead of /usr/local/bin. This is what makes "set it back" offline.
    if [[ -z "$version" || ( -n "$baked" && "$version" == "$baked" ) ]]; then
        reset_to_image "$tool"
        write_manifest
        log "$tool: image copy${baked:+ ($baked)} is current"
        exit 0
    fi

    local current
    current="$(active_version "$tool")"
    if [[ "$current" == "$version" && -x "$root/$tool/$version/bin/$binary" ]]; then
        write_manifest
        log "$tool $version is already current"
        exit 0
    fi

    if [[ ! -x "$root/$tool/$version/bin/$binary" ]]; then
        local tmp
        tmp="$root/$tool/.tmp-$version-$$"
        rm -rf "$tmp"
        mkdir -p "$tmp"
        log "installing $package@$version into $root/$tool/$version"
        if ! npm install -g --prefix "$tmp" "$package@$version" >"$tmp.log" 2>&1; then
            log "npm install failed; $(tail -n 5 "$tmp.log" | tr '\n' ' ')"
            rm -rf "$tmp" "$tmp.log"
            die "$tool stays on ${current:-the image copy}"
        fi
        if ! smoke_check "$tool" "$version" "$tmp"; then
            rm -rf "$tmp" "$tmp.log"
            die "$tool stays on ${current:-the image copy}"
        fi
        rm -f "$tmp.log"
        rm -rf "${root:?}/${tool:?}/${version:?}"
        mv -T "$tmp" "$root/$tool/$version"
    else
        log "$tool $version is already installed; switching to it"
    fi

    flip_current "$tool" "$version"
    prune "$tool"
    write_manifest
    log "$tool $version is current (image carries ${baked:-nothing})"
}

main "$@"
