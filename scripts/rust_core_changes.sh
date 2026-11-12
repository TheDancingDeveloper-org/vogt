#!/usr/bin/env bash
# Decide whether a pull request may skip rust-core.yml's required jobs
# (WI-4158). Reads `git diff --raw --no-renames` lines on stdin and
# prints `rust=true` or `rust=false`.
#
# Fail closed. The jobs are skipped only when EVERY line is
#
#   :<old mode> <new mode> <sha> <sha> A|M<TAB><path>
#
# where the status is exactly A (add) or M (modify), the file is a
# regular file (mode 100644 or 100755) whose mode does not change, and
# the path is on the allowlist below. Everything else runs the jobs,
# whatever the path: D (delete), R (rename), C (copy), T (type change),
# U, X, a mode change, a symlink or submodule, a line that is not --raw
# output, an empty list, and a list of RUST_CORE_MAX_PATHS (default
# 3000) or more lines, which may be truncated.
#
# Why only add and modify: a delete or rename changes which files exist,
# and Rust code enumerates files (disposition.rs lists tests/test_*.py by
# name), so whether a removal is safe depends on readers an allowlist
# can't see. Adding or editing a file is safe only when no job opens
# that file or lists its directory.
#
# A skipped required job counts as passing and rust-core has no merge
# queue, so a path belongs here only if no build, test, script or
# rust-core step opens or enumerates it. scripts/test_rust_core_changes.sh
# scans the repository for references to each entry and fails if it
# finds one.
#
#   tests/acceptance/**
#       the live acceptance harness. Only acceptance.yml runs it; nothing
#       under engine/, src/, scripts/ or rust-core.yml imports, opens or
#       lists it (disposition.rs lists tests/ itself, not its
#       subdirectories, and keeps only names shaped test_*.py).
#   .github/ISSUE_TEMPLATE/**, .github/PULL_REQUEST_TEMPLATE.md
#       forge UI text; nothing reads it.
#
# Not skippable, and why:
#   tests/test_acceptance_suite.py, and every tests/test_*.py:
#       disposition.rs (cargo test -p vogt-core) enumerates them against
#       docs/PYTHON_TEST_DISPOSITION.md.
#   docs/**, top-level *.md: cargo and pytest read docs, some by paths
#       built at run time; engine/Dockerfile copies README.md.
set -euo pipefail

max=${RUST_CORE_MAX_PATHS:-3000}

skippable_path() {
  case "$1" in
    tests/acceptance/*) return 0 ;;
    .github/ISSUE_TEMPLATE/*|.github/PULL_REQUEST_TEMPLATE.md) return 0 ;;
  esac
  return 1
}

regular() { [ "$1" = 100644 ] || [ "$1" = 100755 ]; }

relevant() {
  echo "rust-core relevant: $1" >&2
  echo "rust=true"; exit 0
}

seen=0
while IFS= read -r line || [ -n "$line" ]; do
  [ -z "$line" ] && continue
  seen=$((seen + 1))
  # --raw: ":old new oldsha newsha STATUS<TAB>path[<TAB>path]".
  case "$line" in
    :*$'\t'*) ;;
    *) relevant "$line" ;;
  esac
  meta=${line%%$'\t'*}
  path=${line#*$'\t'}
  case "$path" in *$'\t'*|'') relevant "$line" ;; esac
  read -r old new _ _ status extra <<<"${meta#:}"
  [ -z "${extra:-}" ] || relevant "$line"
  case "$status" in
    A) { [ "$old" = 000000 ] && regular "$new"; } || relevant "$line" ;;
    M) { [ "$old" = "$new" ] && regular "$new"; } || relevant "$line" ;;
    *) relevant "$line" ;;
  esac
  skippable_path "$path" || relevant "$line"
done
if [ "$seen" -eq 0 ]; then
  echo "no changed paths; running everything" >&2
  echo "rust=true"; exit 0
fi
if [ "$seen" -ge "$max" ]; then
  echo "$seen paths, at or over the $max limit; the list may be truncated" >&2
  echo "rust=true"; exit 0
fi
echo "rust=false"
