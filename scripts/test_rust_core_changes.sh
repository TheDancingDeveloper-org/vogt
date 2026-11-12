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
Z=0000000000000000000000000000000000000000
S=1111111111111111111111111111111111111111
# raw STATUS PATH [OLDMODE NEWMODE]: one `git diff --raw` line.
raw() {
  local st=$1 p=$2 om=${3:-} nm=${4:-}
  case "$st" in
    A*) om=${om:-000000}; nm=${nm:-100644} ;;
    D*) om=${om:-100644}; nm=${nm:-000000} ;;
    *) om=${om:-100644}; nm=${nm:-100644} ;;
  esac
  printf ':%s %s %s %s %s\t%s' "$om" "$nm" "$S" "$Z" "$st" "$p"
}

# Skippable: A or M of an allowlisted regular file, mode unchanged.
check false "$(raw M tests/acceptance/harness.py)"
check false "$(raw A tests/acceptance/checks/new.py)" "$(raw M tests/acceptance/matrix.toml)"
check false "$(raw M .github/ISSUE_TEMPLATE/bug_report.yml)" "$(raw M .github/PULL_REQUEST_TEMPLATE.md)"
check false "$(raw M tests/acceptance/run.py 100755 100755)"
check false "$(raw A tests/acceptance/run2.py 000000 100755)"

# Review B3: tests/test_acceptance_suite.py is enumerated by disposition.rs,
# so no change to it skips: modify, delete, rename.
check true "$(raw M tests/test_acceptance_suite.py)"
check true "$(raw D tests/test_acceptance_suite.py)"
check true "$(raw D tests/test_acceptance_suite.py)" "$(raw A tests/acceptance/test_suite.py)"
check true "$(raw R100 tests/test_acceptance_suite.py$'\t'tests/acceptance/suite.py)"
check true "$(raw A tests/test_new_thing.py)"

# Only A and M skip, whatever the path (review B3 principle).
check true "$(raw D tests/acceptance/harness.py)"
check true "$(raw R100 tests/acceptance/a.py$'\t'tests/acceptance/b.py)"
check true "$(raw C100 tests/acceptance/a.py$'\t'tests/acceptance/b.py)"
check true "$(raw T tests/acceptance/a.py 100644 120000)"
check true "$(raw U tests/acceptance/a.py)"
check true "$(raw X tests/acceptance/a.py)"
check true "$(raw M tests/acceptance/run.py 100644 100755)"
check true "$(raw M tests/acceptance/run.py 100755 100644)"
check true "$(raw A tests/acceptance/link 000000 120000)"
check true "$(raw M tests/acceptance/sub 160000 160000)"
check true "$(raw A .github/ISSUE_TEMPLATE/x.yml 100644 100644)"
# Not --raw output: bare paths and name-status lines run.
check true tests/acceptance/harness.py
check true "M${T}tests/acceptance/harness.py"
check true ":100644 100644 $S $Z M"
check true ":100644 100644 $S $Z M extra${T}tests/acceptance/a.py"
check true "$(raw M tests/acceptance/a.py)" "garbage"

# Docs that cargo or pytest read (review B1), and docs in general: all run.
for p in docs/ENGINE.md docs/PYTHON_TEST_DISPOSITION.md docs/CONFIG.md \
         docs/ACCEPTANCE.md docs/design/context-management.md \
         docs/engine-openapi.yaml README.md SECURITY.md CHANGELOG.md \
         engine/README.md .github/PULL_REQUEST_TEMPLATE/other.md; do
  check true "$(raw M "$p")"
done

# Renames and copies (review B2): never skippable, either direction.
check true "$(raw R100 engine/core/src/lib.rs$'\t'docs/lib.md)"
check true "$(raw R087 engine/foo.rs$'\t'tests/acceptance/foo.py)"
check true "$(raw R100 tests/acceptance/x.py$'\t'engine/core/src/x.rs)"
check true "$(raw C100 engine/core/src/lib.rs$'\t'tests/acceptance/lib.py)"
# --no-renames output of the same rename: a delete plus an add.
check true "$(raw D engine/core/src/lib.rs)" "$(raw A tests/acceptance/lib.md)"
check true "$(raw D engine/core/src/lib.rs)"

# Paths an earlier review named: all run.
for p in engine/core/src/lib.rs engine/session-agent/src/main.rs \
         engine/contract/src/lib.rs engine/.cargo/audit.toml scripts/parity.py \
         scripts/select_parity_golden.sh pyproject.toml uv.lock \
         engine/server/tests/rust_core.rs engine/Dockerfile \
         engine/deploy/agent-auth.sh deny.toml Cargo.lock \
         tests/parity/golden/x.json src/vogt/cli.py \
         .github/workflows/rust-core.yml .github/workflows/acceptance.yml \
         scripts/rust_core_changes.sh scripts/test_rust_core_changes.sh \
         web/src/app.ts tests/test_sessions.py tests/acceptance_other.py; do
  check true "$(raw M "$p")"
done

# Mixed sets: one relevant line runs everything.
check true "$(raw M tests/acceptance/harness.py)" "$(raw M src/vogt/cli.py)"
check true "$(raw M tests/acceptance/harness.py)" "$(raw M docs/ENGINE.md)"
check true "$(raw M .github/PULL_REQUEST_TEMPLATE.md)" "$(raw M README.md)"

# Empty diff runs.
check true ""

# A list at the cap may be truncated: run. One under it skips.
many() { local i; for ((i = 0; i < $1; i++)); do raw M "tests/acceptance/f$i.py"; echo; done; }
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

# tests/test_acceptance_suite.py imports tests.acceptance, but only pytest
# (ci.yml, acceptance.yml) runs it; no rust-core job runs pytest, and the
# file itself is never skippable (disposition.rs enumerates it).
if grep -v '^[[:space:]]*#' "$root/.github/workflows/rust-core.yml" | grep -qw pytest; then
  echo "FAIL: rust-core.yml runs pytest; test_acceptance_suite.py's import of tests.acceptance now matters"; fail=1
fi

# 1. No scanned file names an allowlisted path (it would be read by a
#    job that the allowlist lets skip).
for pat in 'tests[/.]acceptance[/.]' 'ISSUE_TEMPLATE' 'PULL_REQUEST_TEMPLATE'; do
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
  got=$(raw M "$d" | bash "$classify" 2>/dev/null)
  [ "$got" = "rust=true" ] || { echo "FAIL: referenced doc $d is skippable"; fail=1; }
done <<<"$docs"
for must in docs/ENGINE.md docs/PYTHON_TEST_DISPOSITION.md; do
  grep -qxF "$must" <<<"$docs" || { echo "FAIL: scan did not find $must"; fail=1; }
done
[ "$ndocs" -gt 0 ] || { echo "FAIL: doc scan found nothing"; fail=1; }

[ "$fail" -eq 0 ] && echo "rust_core_changes: all cases pass ($ndocs referenced docs checked)"
exit "$fail"
