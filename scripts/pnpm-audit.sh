#!/usr/bin/env bash
# Resilient `pnpm audit` for CI (#571).
#
# The audit is a fatal gate on dependency advisories (OSR-01): a real finding
# must fail the build and is never retried. But the advisories endpoint on
# registry.npmjs.org times out from the self-hosted runners often enough that a
# *network* failure kept failing the compile+test job it shares (`error (23)` /
# "operation was aborted due to timeout"), costing a full re-run each time —
# four times on 2026-09-04 alone. So a registry/network failure is retried with
# a bounded, backing-off budget and, if the endpoint never answers, reported
# distinctly as a reachability problem, not as a dependency finding.
#
# The Rust audit already sidesteps this by auditing a local advisory-db clone
# (`cargo audit --no-fetch`); pnpm has no offline equivalent, so this bounds the
# network instead.
#
# Run from the package directory (web/ or mobile/).
set -uo pipefail

attempts="${AUDIT_ATTEMPTS:-4}"
delay="${AUDIT_INITIAL_DELAY:-15}"

# Fail each attempt fast on a network problem so this wrapper's backoff — not
# pnpm's own minutes-long internal retry — governs the total budget.
export npm_config_fetch_retries="${npm_config_fetch_retries:-1}"
export npm_config_fetch_timeout="${npm_config_fetch_timeout:-60000}"

# Signatures of "could not reach the registry", never of an advisory answer.
network_re='ETIMEDOUT|ECONNRESET|ECONNREFUSED|ENOTFOUND|EAI_AGAIN|error \(23\)|operation was aborted due to timeout|socket hang up|TimeoutError|FetchError|Will retry in|request to .* failed'

for attempt in $(seq 1 "$attempts"); do
  output="$(pnpm audit "$@" 2>&1)"
  rc=$?
  printf '%s\n' "$output"

  if [ "$rc" -eq 0 ]; then
    exit 0
  fi

  if printf '%s\n' "$output" | grep -qiE "$network_re"; then
    if [ "$attempt" -lt "$attempts" ]; then
      echo "::warning::pnpm audit could not reach the advisories endpoint (attempt ${attempt}/${attempts}); retrying in ${delay}s. Registry/network flake (#571), not a dependency finding."
      sleep "$delay"
      delay=$((delay * 2))
      continue
    fi
    echo "::error::pnpm audit could not reach registry.npmjs.org after ${attempts} attempts — a registry/network failure (#571), not a dependency advisory. Re-run once the runner's path to the registry recovers."
    exit 1
  fi

  # pnpm answered with advisories (or another non-network error): fatal, never
  # retried — the audit is a gate on vulnerabilities, not on reachability.
  echo "::error::pnpm audit reported dependency advisories — fix or triage them (OSR-01). This is a real finding, not a network flake."
  exit "$rc"
done
