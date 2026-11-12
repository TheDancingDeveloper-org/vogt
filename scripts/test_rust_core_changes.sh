#!/usr/bin/env bash
# Self-test for scripts/rust_core_changes.sh (WI-4158). Each case is a
# set of changed-path lines and the expected decision. The last part
# scans the repository for references to the allowlist and to docs.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
classify="$here/rust_core_changes.sh"
fail=0
check() {
  local want=$1; shift
  local got
  got=$(printf '%s\n' "$@" | bash "$classify" 2>/dev/null)
  if [ "$got" != "rust=$want" ]; then
    echo "FAIL: [$*] want rust=$want, got $got"; fail=1
  fi
}
T=$'\t'

# Skippable: allowlisted paths only.
check false tests/acceptance/harness.py
check false tests/acceptance/checks/x.py tests/test_acceptance_suite.py
check false .github/ISSUE_TEMPLATE/bug_report.yml .github/PULL_REQUEST_TEMPLATE.md
check false "M${T}tests/acceptance/harness.py" "A${T}tests/acceptance/new.py"
check false "R100${T}tests/acceptance/a.py${T}tests/acceptance/b.py"

# Docs that cargo or pytest read (review B1), and docs in general: all run.
check true docs/ENGINE.md
check true docs/PYTHON_TEST_DISPOSITION.md
check true docs/CONFIG.md
check true docs/ACCEPTANCE.md
check true docs/design/context-management.md
check true docs/engine-openapi.yaml
check true README.md
check true SECURITY.md
check true CHANGELOG.md
check true engine/README.md
check true .github/PULL_REQUEST_TEMPLATE/other.md

# Renames and copies (review B2): the old path counts as well as the new.
check true "R100${T}engine/core/src/lib.rs${T}docs/lib.md"
check true "R087${T}engine/foo.rs${T}tests/acceptance/foo.py"
check true "R100${T}docs/foo.md${T}engine/foo.rs"
check true "R100${T}tests/acceptance/x.py${T}engine/core/src/x.rs"
check true "C100${T}engine/core/src/lib.rs${T}tests/acceptance/lib.py"
check true "R100"
# --no-renames output of the same rename: a delete plus an add.
check true "D${T}engine/core/src/lib.rs" "A${T}tests/acceptance/lib.md"
# Deleting a Rust file runs.
check true "D${T}engine/core/src/lib.rs"
check true engine/core/src/lib.rs

# Paths an earlier review named: all run.
check true engine/session-agent/src/main.rs
check true engine/contract/src/lib.rs
check true engine/.cargo/audit.toml
check true scripts/parity.py
check true scripts/select_parity_golden.sh
check true pyproject.toml
check true uv.lock
check true engine/server/tests/rust_core.rs
check true engine/Dockerfile
check true engine/deploy/agent-auth.sh
check true deny.toml
check true Cargo.lock
check true tests/parity/golden/x.json
check true src/vogt/cli.py
check true .github/workflows/rust-core.yml
check true .github/workflows/acceptance.yml
check true scripts/rust_core_changes.sh
check true scripts/test_rust_core_changes.sh
check true web/src/app.ts
check true tests/test_sessions.py
check true tests/acceptance_other.py

# Mixed sets: one relevant path runs everything.
check true tests/acceptance/harness.py src/vogt/cli.py
check true tests/acceptance/harness.py docs/ENGINE.md
check true .github/PULL_REQUEST_TEMPLATE.md README.md

# Empty diff runs.
check true ""

# A list at the pull-files cap may be truncated: run. One under it skips.
many() { local i; for ((i = 0; i < $1; i++)); do echo "tests/acceptance/f$i.py"; done; }
got=$(many 3000 | bash "$classify" 2>/dev/null)
[ "$got" = "rust=true" ] || { echo "FAIL: 3000 paths gave $got"; fail=1; }
got=$(many 2999 | bash "$classify" 2>/dev/null)
[ "$got" = "rust=false" ] || { echo "FAIL: 2999 paths gave $got"; fail=1; }
got=$(many 5 | RUST_CORE_MAX_PATHS=5 bash "$classify" 2>/dev/null)
[ "$got" = "rust=true" ] || { echo "FAIL: 5 paths at a limit of 5 gave $got"; fail=1; }

# Repository scan. Code that a rust-core job builds, runs or reads:
# engine/, src/, scripts/ (minus this classifier), web/ (minus deps),
# tests/ outside the acceptance allowlist, and rust-core.yml.
scan_roots=()
for d in engine src scripts web tests .github/workflows/rust-core.yml; do
  [ -e "$root/$d" ] && scan_roots+=("$root/$d")
done
scan() {
  grep -rlE "$1" "${scan_roots[@]}" \
    --exclude-dir=node_modules --exclude-dir=target --exclude-dir=acceptance \
    --exclude=rust_core_changes.sh --exclude=test_rust_core_changes.sh \
    --exclude=test_acceptance_suite.py 2>/dev/null || true
}

# 1. No scanned file names an allowlisted path (it would be read by a
#    job that the allowlist lets skip).
for pat in 'tests[/.]acceptance' 'test_acceptance_suite' \
           'ISSUE_TEMPLATE' 'PULL_REQUEST_TEMPLATE'; do
  hits=$(scan "$pat")
  if [ -n "$hits" ]; then
    echo "FAIL: allowlisted '$pat' is referenced by:"; echo "$hits"; fail=1
  fi
done

# 2. Every doc a scanned file names (docs/X, "docs" / "X", ../docs/X) is
#    classified as relevant.
docs=$(grep -rhoE '(docs/[A-Za-z0-9_./-]+|"docs"[[:space:]]*/[[:space:]]*"[A-Za-z0-9_./-]+")' \
  "${scan_roots[@]}" --exclude-dir=node_modules --exclude-dir=target 2>/dev/null |
  sed -E 's#"docs"[[:space:]]*/[[:space:]]*"#docs/#; s#"$##; s#[.]$##' | sort -u)
ndocs=0
while IFS= read -r d; do
  [ -z "$d" ] && continue
  ndocs=$((ndocs + 1))
  got=$(printf '%s\n' "$d" | bash "$classify" 2>/dev/null)
  [ "$got" = "rust=true" ] || { echo "FAIL: referenced doc $d is skippable"; fail=1; }
done <<<"$docs"
for must in docs/ENGINE.md docs/PYTHON_TEST_DISPOSITION.md; do
  grep -qxF "$must" <<<"$docs" || { echo "FAIL: scan did not find $must"; fail=1; }
done
[ "$ndocs" -gt 0 ] || { echo "FAIL: doc scan found nothing"; fail=1; }

[ "$fail" -eq 0 ] && echo "rust_core_changes: all cases pass ($ndocs referenced docs checked)"
exit "$fail"
