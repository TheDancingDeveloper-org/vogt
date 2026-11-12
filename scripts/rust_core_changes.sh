#!/usr/bin/env bash
# Decide whether a pull request may skip rust-core.yml's required jobs
# (WI-4158). Reads the changed paths on stdin and prints `rust=true` or
# `rust=false`. Each line is either a bare path or a `git diff
# --name-status` line (`STATUS<TAB>path[<TAB>path]`); every path on a
# line is classified, so a rename or copy is judged on both its old and
# its new path. The workflow also diffs with --no-renames, which turns a
# rename into a delete plus an add.
#
# Fail closed: the jobs are skipped only when EVERY path matches the
# allowlist below. Anything else runs them, and so does an empty list or
# a list of RUST_CORE_MAX_PATHS (default 3000, the GitHub pull-files cap)
# or more paths, which may be truncated.
#
# A skipped required job counts as passing and rust-core has no merge
# queue, so a path belongs here only if no build, test, script or
# rust-core step reads it. scripts/test_rust_core_changes.sh scans the
# repository for references to each entry and fails if it finds one.
#
#   tests/acceptance/**, tests/test_acceptance_suite.py
#       the live acceptance harness and its pytest wrapper. Only
#       acceptance.yml runs them; nothing under engine/, src/, scripts/ or
#       rust-core.yml imports or opens them.
#   .github/ISSUE_TEMPLATE/**, .github/PULL_REQUEST_TEMPLATE.md
#       forge UI text; nothing reads it.
#
# Docs are NOT skippable, not even prose: cargo and pytest read docs
# (include_str!("../../docs/ENGINE.md"), docs/PYTHON_TEST_DISPOSITION.md,
# REPO_ROOT / "docs" / ..., parity renders docs/CONFIG.md) and some paths
# are built at run time, so no grep can prove a doc unread. Top-level
# *.md are not skippable either: engine/Dockerfile copies README.md, and
# tests read SECURITY.md and CLAUDE.md.
set -euo pipefail

max=${RUST_CORE_MAX_PATHS:-3000}

skippable() {
  case "$1" in
    tests/acceptance/*|tests/test_acceptance_suite.py) return 0 ;;
    .github/ISSUE_TEMPLATE/*|.github/PULL_REQUEST_TEMPLATE.md) return 0 ;;
  esac
  return 1
}

relevant() {
  echo "rust-core relevant: $1" >&2
  echo "rust=true"; exit 0
}

seen=0
while IFS= read -r line || [ -n "$line" ]; do
  [ -z "$line" ] && continue
  case "$line" in
    *$'\t'*)
      # name-status: drop the status field, classify every path after it.
      IFS=$'\t' read -r -a fields <<<"$line"
      paths=("${fields[@]:1}")
      [ "${#paths[@]}" -gt 0 ] || relevant "$line"
      ;;
    *) paths=("$line") ;;
  esac
  for f in "${paths[@]}"; do
    seen=$((seen + 1))
    skippable "$f" || relevant "$f"
  done
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
