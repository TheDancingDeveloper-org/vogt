#!/usr/bin/env bash
# Copy agent context — Claude Code and Codex transcripts, per-project memory,
# prompt history, and the top-level notes in ~/Working — from one home
# directory to another. The companion of `vogt clone` (DEPLOYMENT.md §5,
# "Cloning prod into dev"): the clone moves the Vogt database, this moves
# what the agents remember, so a session on the copy can resume a
# conversation that started on the source.
#
#     scripts/clone-agent-context.sh SRC_HOME DST_HOME            # dry run
#     scripts/clone-agent-context.sh SRC_HOME DST_HOME --apply    # copy
#
# SRC_HOME and DST_HOME are home directories, local or rsync-remote
# (`user@host:/home/name`); at most one may be remote, as rsync requires.
#
# What is copied is an ALLOWLIST, so anything not named below stays behind:
#
#   .claude/projects/        transcripts and per-project memory (MEMORY.md …)
#   .claude/history.jsonl    prompt history
#   .claude/file-history/    edit checkpoints a resumed session can rewind to
#   .claude/todos/ .claude/plans/   when present
#   .codex/sessions/  .codex/history.jsonl  .codex/session_index.jsonl
#   .codex/memories_1.sqlite (+ -wal/-shm)   Codex memory
#   Working/*.md             top-level notes and session snapshots, except
#                            AGENTS.md and CLAUDE.md (the workspace bootstrap
#                            writes those on each home itself)
#
# Never copied, by construction (not in the allowlist) and again by an
# explicit exclude inside the allowlisted trees, per the workspace safety
# rules: credentials and tokens (.credentials.json, auth.json, *.pem, *.key,
# .env*), MCP configuration and auth (~/.claude.json, .codex/config.toml),
# settings and local permissions (settings.json, settings.local.json), logs,
# MCP logs and caches. Transcripts can still *quote* a secret a session once
# printed; that is a property of the transcripts, and the reason this copy is
# between two homes of the same operator only.
#
# Paths must line up. Claude Code keys .claude/projects/ by the absolute path
# of each project (`/home/u/Working/x` → `-home-u-Working-x`), so a transcript
# resumes only where the same path exists. The script checks that every
# project key in the source starts with the encoding of --home-path (default:
# the source home's path) and warns about any that do not.
#
# Additive: no --delete. A file that exists only on the destination is kept,
# and a file on both is overwritten when the source copy is newer (rsync
# --update). Run it while no agent session is writing on either side; a
# SQLite file copied mid-write is torn. The source stays usable afterwards —
# which copy is *live* is the operator's decision (DEPLOYMENT.md §5), not
# something this copy can know.
set -euo pipefail

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed -e '$d' -e 's/^# \{0,1\}//'
    exit "${1:-0}"
}

apply=0
home_path=""
positional=()
while [ $# -gt 0 ]; do
    case "$1" in
        --apply) apply=1 ;;
        --home-path) home_path=${2:?--home-path needs a value}; shift ;;
        --home-path=*) home_path=${1#--home-path=} ;;
        -h|--help) usage 0 ;;
        -*) echo "unknown option: $1" >&2; usage 64 ;;
        *) positional+=("$1") ;;
    esac
    shift
done
[ ${#positional[@]} -eq 2 ] || usage 64
src=${positional[0]%/}
dst=${positional[1]%/}

is_remote() { case "$1" in *:*) return 0 ;; *) return 1 ;; esac; }
if is_remote "$src" && is_remote "$dst"; then
    echo "only one of SRC_HOME and DST_HOME may be remote" >&2
    exit 64
fi
if ! is_remote "$src" && [ ! -d "$src" ]; then
    echo "source home does not exist: $src" >&2
    exit 66
fi
command -v rsync >/dev/null || { echo "rsync is required" >&2; exit 69; }

# --- path compatibility -----------------------------------------------------
[ -n "$home_path" ] || home_path=${src#*:}
encoded=$(printf '%s' "$home_path" | tr '/.' '--')
list_projects() {
    if is_remote "$src"; then
        ssh "${src%%:*}" ls -1 "${src#*:}/.claude/projects" 2>/dev/null || true
    else
        ls -1 "$src/.claude/projects" 2>/dev/null || true
    fi
}
mismatched=0
while IFS= read -r key; do
    [ -n "$key" ] || continue
    case "$key" in
        "$encoded"|"$encoded"-*) ;;
        *) echo "warning: project key $key is not under $home_path;" \
                "its transcripts will not resume at that path" >&2
           mismatched=$((mismatched + 1)) ;;
    esac
done < <(list_projects)
echo "path check: project keys expected under $home_path ($encoded); $mismatched outside it"

# --- the copy ---------------------------------------------------------------
excludes=(
    --exclude=.credentials.json --exclude=auth.json
    --exclude='.env' --exclude='.env.*' --exclude='*.pem' --exclude='*.key'
    --exclude=settings.json --exclude=settings.local.json
    --exclude=config.toml --exclude='mcp-logs*/' --exclude='*.log'
    --exclude=cache/ --exclude=.cache/ --exclude=shell-snapshots/
)
rsync_opts=(-a --update --relative --human-readable --itemize-changes "${excludes[@]}")
[ "$apply" -eq 1 ] || rsync_opts+=(--dry-run)

# `--relative` with a `/./` marker keeps each path relative to the home, so
# one rsync per tree lands it at the same place under DST_HOME.
trees=(
    .claude/projects/
    .claude/history.jsonl
    .claude/file-history/
    .claude/todos/
    .claude/plans/
    .codex/sessions/
    .codex/history.jsonl
    .codex/session_index.jsonl
    .codex/memories_1.sqlite
    .codex/memories_1.sqlite-wal
    .codex/memories_1.sqlite-shm
)
exists() {
    if is_remote "$src"; then
        ssh "${src%%:*}" test -e "${src#*:}/$1"
    else
        test -e "$src/$1"
    fi
}
copied=0
for tree in "${trees[@]}"; do
    if ! exists "$tree"; then
        echo "skip (absent): $tree"
        continue
    fi
    echo "== $tree"
    rsync "${rsync_opts[@]}" "$src/./$tree" "$dst/"
    copied=$((copied + 1))
done

# Working/*.md: the top level only, minus the bootstrap-owned pair.
echo "== Working/*.md"
rsync "${rsync_opts[@]}" --include='/Working/' --exclude='/Working/AGENTS.md' \
    --exclude='/Working/CLAUDE.md' --include='/Working/*.md' --exclude='*' \
    "$src/./" "$dst/"

if [ "$apply" -eq 1 ]; then
    echo "applied: $copied trees plus Working/*.md copied into $dst"
else
    echo "dry run: nothing written. Re-run with --apply to copy."
fi
