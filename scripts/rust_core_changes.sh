#!/usr/bin/env bash
# Decide whether a pull request may skip rust-core.yml's required jobs
# (WI-4158). Reads changed paths, one per line, on stdin and prints
# `rust=true` or `rust=false`.
#
# Fail closed: the jobs are skipped only when EVERY path matches the
# allowlist below. Anything else, including an empty list, runs them.
# A skipped required job counts as passing and rust-core has no merge
# queue, so a path belongs here only if no required job can read it:
#
#   docs/**/*.md       prose. docs/CONFIG.md is excluded: parity's
#                      contract check renders it from the config model.
#   /*.md              top-level prose (README, CHANGELOG, ...).
#   tests/acceptance/**, tests/test_acceptance_suite.py
#                      the live acceptance harness; it runs against a
#                      deployed stack and no rust-core job imports it.
#   .github/ISSUE_TEMPLATE/**, .github/PULL_REQUEST_TEMPLATE*
#                      forge UI text.
#
# Everything else runs: engine/**, Cargo files, src/vogt/**, scripts/**,
# tests/parity/**, pyproject.toml, uv.lock, other workflows, this script.
set -euo pipefail

skippable() {
  case "$1" in
    docs/CONFIG.md) return 1 ;;
    docs/*.md) return 0 ;;
    */*) ;;
    *.md) return 0 ;;
  esac
  case "$1" in
    tests/acceptance/*|tests/test_acceptance_suite.py) return 0 ;;
    .github/ISSUE_TEMPLATE/*|.github/PULL_REQUEST_TEMPLATE*) return 0 ;;
  esac
  return 1
}

seen=0
while IFS= read -r f || [ -n "$f" ]; do
  [ -z "$f" ] && continue
  seen=$((seen + 1))
  if ! skippable "$f"; then
    echo "rust-core relevant: $f" >&2
    echo "rust=true"; exit 0
  fi
done
if [ "$seen" -eq 0 ]; then
  echo "no changed paths; running everything" >&2
  echo "rust=true"; exit 0
fi
echo "rust=false"
