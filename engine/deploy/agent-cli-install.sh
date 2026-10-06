#!/usr/bin/env bash
# Runtime-pinned agent CLIs.
#
# The engine image bakes the agent CLIs (Claude Code, Codex, ...) at the versions in
# `engine/agent-versions.env`, and every update path inside the running pod is
# deliberately closed (`DISABLE_UPDATES=1`). That made a new CLI version
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
# Which tools exist, where each comes from and what its binary is called
# comes from a table the image writes beside its resolved versions
# (`agent-clis.tools`, tab-separated: tool, source, binary, env var, kind) — so
# this script names no tool and a deployment that bakes a different set needs
# no change here. The kind says how a version is fetched; a row without one is
# `npm`, the shape every row had before kinds existed:
#
#   npm       source is an npm package; `npm install -g --prefix` per version.
#   go-dist   source is a Go release mirror (https://go.dev/dl); the version's
#             linux tarball, checked against the sha256 the mirror's own
#             release index gives for it (WI-951). The prefix is a GOROOT.
#   go-src    source is a git repository of a Go program whose main package
#             is `./cmd/<binary>`; the version is a full commit id, fetched
#             alone and built with the `go` on PATH, CGO off (WI-950). The
#             prefix records `source` (`<repository>@<commit>`). Codex is the one exception in *shape*: it gets no `bin/`
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
  tool     a tool named in the image's agent CLI table (claude-code, codex,
           go, ...)
  version  an exact version (2.1.261, or a Go release like 1.27.1), `image`
           for the baked copy, or — only with VOGT_AGENT_CLI_ALLOW_DIST_TAGS=1
           — `latest` or `stable` (for Go, either means the newest stable)
USAGE
    exit 64
}

# One column of the tools table for a tool: 2 = source, 3 = binary, 5 = kind.
table_field() {
    local tool="$1" column="$2"
    [[ -r "$tools_table" ]] || return 1
    awk -F'\t' -v tool="$tool" -v column="$column" \
        '$1 == tool { print $column; found = 1; exit } END { exit !found }' "$tools_table"
}
package_for() { table_field "$1" 2; }
binary_for() { table_field "$1" 3; }
kind_for() {
    local kind
    kind="$(table_field "$1" 5 2>/dev/null || true)"
    printf '%s' "${kind:-npm}"
}

# Binaries a tool puts on PATH beyond its own: a Go toolchain is `go` and
# `gofmt`, and a `gofmt` left on the image copy beside a pinned `go` would be
# a second, older toolchain answering to the same name.
extra_binaries() {
    case "$(kind_for "$1")" in
        go-dist) echo gofmt ;;
    esac
}
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
    local tool="$1" version="$2" binary name
    binary="$(binary_for "$tool")"
    ln -sfn "$version" "$root/$tool/current.tmp.$$"
    mv -fT "$root/$tool/current.tmp.$$" "$root/$tool/current"
    if [[ "$binary" != "codex" ]]; then
        mkdir -p "$root/bin"
        for name in "$binary" $(extra_binaries "$tool"); do
            ln -sfn "../$tool/current/bin/$name" "$root/bin/$name.tmp.$$"
            mv -fT "$root/bin/$name.tmp.$$" "$root/bin/$name"
        done
    fi
}

# Back to the image copy: no `current`, no bin link, nothing on PATH ahead of
# /usr/local/bin. Version directories are kept so a later flip is offline.
reset_to_image() {
    local tool="$1" binary name
    binary="$(binary_for "$tool")"
    rm -f "$root/$tool/current"
    for name in "$binary" $(extra_binaries "$tool"); do
        rm -f "$root/bin/$name"
    done
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

# The install must prove itself the way the Dockerfile's does: `npm
# install -g` exiting 0 is not evidence that the binary runs — 2.1.237 shipped
# `bin/claude.exe` as a shell stub. `--version` has to succeed, and when it
# prints a version at all it has to be the one asked for; a CLI that prints
# nothing version-shaped is held to exit status alone.
smoke_check() {
    local tool="$1" version="$2" prefix="$3" binary output
    binary="$(binary_for "$tool")"
    [[ -x "$prefix/bin/$binary" ]] || { log "$prefix/bin/$binary is missing or not executable"; return 1; }
    if [[ "$(kind_for "$tool")" == "go-src" ]]; then
        # A program built from a commit names its own release, not the
        # commit: a clean `--version` is the check.
        output="$("$prefix/bin/$binary" --version 2>&1)" || { log "$binary --version failed: ${output:-<no output>}"; return 1; }
        return 0
    fi
    if [[ "$(kind_for "$tool")" == "go-dist" ]]; then
        # `go version`, not `--version`, and it names itself `go<version>`.
        output="$("$prefix/bin/$binary" version 2>&1)" || { log "$binary version failed: ${output:-<no output>}"; return 1; }
        [[ "$output" == *"go$version "* ]] || { log "$binary version says '${output}', not go$version"; return 1; }
        return 0
    fi
    if ! output="$("$prefix/bin/$binary" --version 2>&1)"; then
        log "$binary --version failed: ${output:-<no output>}"
        return 1
    fi
    if [[ "$output" =~ [0-9]+\.[0-9]+\.[0-9]+ && "$output" != *"$version"* ]]; then
        log "$binary --version says '${output}', not $version — a stub install"
        return 1
    fi
}

# The GOARCH of this machine, as Go's release tarballs name it.
go_arch() {
    case "$(uname -m)" in
        x86_64|amd64) echo amd64 ;;
        aarch64|arm64) echo arm64 ;;
        *) return 1 ;;
    esac
}

# go.dev's release index (every release, with each file's sha256), from the
# mirror the table names. VOGT_GO_DIST_INDEX points at a local copy instead —
# the tests' offline mirror, or a deployment's own.
go_index() {
    local source="$1"
    if [[ -n "${VOGT_GO_DIST_INDEX:-}" ]]; then
        cat "$VOGT_GO_DIST_INDEX"
    else
        curl -fsSL --max-time 60 "$source/?mode=json&include=all"
    fi
}

# The newest stable Go release the index lists, without its `go` prefix.
go_latest() {
    go_index "$1" | python3 -c '
import json, sys
for release in json.load(sys.stdin):
    if release.get("stable"):
        print(release["version"].removeprefix("go"))
        break
'
}

# Unpack one Go release into `prefix` (a GOROOT: `prefix/bin/go`), checked
# against the sha256 the index records for that exact file.
install_go_dist() {
    local tool="$1" version="$2" prefix="$3" source arch file sum archive
    source="$(package_for "$tool")"
    arch="$(go_arch)" || { log "no Go release for $(uname -m)"; return 1; }
    file="go$version.linux-$arch.tar.gz"
    sum="$(go_index "$source" | python3 -c '
import json, sys
name = sys.argv[1]
for release in json.load(sys.stdin):
    for f in release.get("files", []):
        if f.get("filename") == name and f.get("sha256"):
            print(f["sha256"])
            sys.exit(0)
sys.exit(1)
' "$file")" || { log "the Go release index lists no $file"; return 1; }
    archive="$prefix.tar.gz"
    if ! curl -fsSL --max-time 600 "$source/$file" -o "$archive"; then
        log "could not download $source/$file"
        rm -f "$archive"
        return 1
    fi
    if ! echo "$sum  $archive" | sha256sum -c - >/dev/null 2>&1; then
        log "$file does not match the sha256 the release index gives for it"
        rm -f "$archive"
        return 1
    fi
    tar -xzf "$archive" -C "$prefix" --strip-components=1
    local status=$?
    rm -f "$archive"
    return "$status"
}

# Build one commit of a Go program from its repository into `prefix/bin`.
# Only that commit is fetched; modules land in a cache on the volume, so the
# next build of a nearby commit is mostly offline.
install_go_src() {
    local tool="$1" version="$2" prefix="$3" source binary src build_log
    source="$(package_for "$tool")"
    binary="$(binary_for "$tool")"
    command -v go >/dev/null 2>&1 || { log "no go on PATH to build $tool"; return 1; }
    src="$prefix.src"
    build_log="$prefix.log"
    rm -rf "$src"
    if ! { git init -q "$src" \
            && git -C "$src" fetch -q --depth 1 "$source" "$version" \
            && git -C "$src" checkout -q --detach FETCH_HEAD; } >"$build_log" 2>&1; then
        log "could not fetch $source at $version; $(tail -n 3 "$build_log" | tr '\n' ' ')"
        rm -rf "$src" "$build_log"
        return 1
    fi
    if [[ "$(git -C "$src" rev-parse HEAD)" != "$version" ]]; then
        log "$source gave $(git -C "$src" rev-parse HEAD) for $version"
        rm -rf "$src" "$build_log"
        return 1
    fi
    mkdir -p "$prefix/bin" "$root/.gomodcache"
    log "building $tool $version from $source with $(go version | cut -d' ' -f3)"
    if ! (cd "$src" && CGO_ENABLED=0 GOFLAGS=-mod=readonly \
            GOMODCACHE="$root/.gomodcache" GOCACHE="$src.gocache" \
            go build -trimpath -o "$prefix/bin/$binary" "./cmd/$binary") >"$build_log" 2>&1; then
        log "go build failed; $(tail -n 5 "$build_log" | tr '\n' ' ')"
        rm -rf "$src" "$src.gocache" "$build_log"
        return 1
    fi
    printf '%s@%s\n' "$source" "$version" > "$prefix/source"
    rm -rf "$src" "$src.gocache" "$build_log"
}

# Fetch `version` of `tool` into the fresh directory `prefix`, by its kind.
fetch_into() {
    local tool="$1" version="$2" prefix="$3" package log_file
    case "$(kind_for "$tool")" in
        npm)
            package="$(package_for "$tool")"
            log_file="$prefix.log"
            log "installing $package@$version into $root/$tool/$version"
            if ! npm install -g --prefix "$prefix" "$package@$version" >"$log_file" 2>&1; then
                log "npm install failed; $(tail -n 5 "$log_file" | tr '\n' ' ')"
                rm -f "$log_file"
                return 1
            fi
            rm -f "$log_file"
            ;;
        go-dist)
            log "installing Go $version into $root/$tool/$version"
            install_go_dist "$tool" "$version" "$prefix"
            ;;
        go-src)
            install_go_src "$tool" "$version" "$prefix"
            ;;
        *)
            log "'$(kind_for "$tool")' is not a kind of tool this installer knows"
            return 1
            ;;
    esac
}

main() {
    (( $# == 2 )) || usage
    local tool="$1" requested="$2" package binary version baked kind
    package="$(package_for "$tool")" || die "'$tool' is not a tool this image knows (see $tools_table)" 64
    binary="$(binary_for "$tool")"
    baked="$(baked_version "$tool")"
    kind="$(kind_for "$tool")"

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
            if [[ "$kind" == "go-dist" ]]; then
                version="$(go_latest "$package" 2>/dev/null | tr -d '[:space:]')"
            elif [[ "$kind" == "go-src" ]]; then
                # The repository's default branch, as it is now.
                version="$(git ls-remote "$package" HEAD 2>/dev/null | cut -f1 | tr -d '[:space:]')"
            else
                version="$(npm view "$package@$requested" version 2>/dev/null | tail -n 1 | tr -d '[:space:]')"
            fi
            [[ -n "$version" ]] || die "could not resolve $tool $requested against $package"
            log "$tool $requested resolves to $version"
            ;;
        *)
            if [[ "$kind" == "go-src" ]]; then
                # A commit, in full: a short id is a prefix a later commit can
                # share, and a branch or tag can move under the pin.
                if [[ ! "$requested" =~ ^[0-9a-f]{40}$ ]]; then
                    die "'$requested' is not a full commit id (40 hex characters) or 'image'; $tool is built from a commit" 64
                fi
                version="$requested"
            elif [[ ! "$requested" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$ ]]; then
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
        if ! fetch_into "$tool" "$version" "$tmp"; then
            rm -rf "$tmp"
            die "$tool stays on ${current:-the image copy}"
        fi
        if ! smoke_check "$tool" "$version" "$tmp"; then
            rm -rf "$tmp"
            die "$tool stays on ${current:-the image copy}"
        fi
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
