#!/usr/bin/env bash
# Self-test for scripts/rust_core_changes.sh (WI-4158). Each case is a
# space-separated path set and the expected decision.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
fail=0
check() {
  local want=$1; shift
  local got
  got=$(printf '%s\n' "$@" | bash "$here/rust_core_changes.sh" 2>/dev/null)
  if [ "$got" != "rust=$want" ]; then
    echo "FAIL: [$*] want rust=$want, got $got"; fail=1
  fi
}
# Skippable: allowlisted paths only.
check false tests/acceptance/harness.py
check false tests/acceptance/checks/x.py tests/test_acceptance_suite.py
check false docs/ENGINE.md README.md
check false docs/design/context-management.md CHANGELOG.md
check false .github/ISSUE_TEMPLATE/bug.md .github/PULL_REQUEST_TEMPLATE.md
# Paths the WI-4158 review named: all run.
check true engine/session-agent/src/main.rs
check true engine/contract/src/lib.rs
check true engine/.cargo/audit.toml
check true scripts/parity.py
check true scripts/select_parity_golden.sh
check true pyproject.toml
check true uv.lock
# Previously listed paths still run.
check true engine/core/src/lib.rs
check true engine/server/tests/rust_core.rs
check true engine/Dockerfile
check true engine/deploy/agent-auth.sh
check true deny.toml
check true Cargo.lock
check true tests/parity/golden/x.json
check true src/vogt/cli.py
check true .github/workflows/rust-core.yml
check true scripts/rust_core_changes.sh
check true scripts/test_rust_core_changes.sh
# Not allowlisted, so fail closed.
check true docs/CONFIG.md
check true docs/engine-openapi.yaml
check true engine/README.md
check true src/vogt/README.md
check true .github/workflows/ci.yml
check true web/src/app.ts
check true tests/test_sessions.py
# Mixed sets: one relevant path runs everything.
check true tests/acceptance/harness.py src/vogt/cli.py
check true docs/ENGINE.md engine/core/src/lib.rs
check true README.md .github/workflows/rust-core.yml
# Empty diff runs.
check true ""
[ "$fail" -eq 0 ] && echo "rust_core_changes: all cases pass"
exit "$fail"
