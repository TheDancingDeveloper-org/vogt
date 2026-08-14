#!/usr/bin/env bash
# Check that a merged stack is actually merged (DEPLOYMENT §9.2).
#
# The failure this exists for is not a crash. It is a front door that comes up,
# passes its healthcheck and serves no Vogt — which happens because the core's
# readiness probe is deliberately non-fatal (FR-E9), so `/readyz`'s top-level
# `ok` can be true while the half of the product somebody just deployed is
# absent. Six checks, each naming what its failure means.
#
#     scripts/smoke_merged_stack.sh https://vogt.sprooty.com "$FRONT_DOOR_TOKEN"
#
# The token is a *front-door* token — the one a browser holds. It is read from
# argv only for a one-off check; prefer `VOGT_SMOKE_TOKEN` in the environment,
# since argv is visible in `ps`.
set -euo pipefail

BASE="${1:-${VOGT_SMOKE_BASE:-}}"
TOKEN="${2:-${VOGT_SMOKE_TOKEN:-}}"

if [[ -z "$BASE" ]]; then
    echo "usage: $0 <base-url> [front-door-token]" >&2
    echo "   or: VOGT_SMOKE_BASE=... VOGT_SMOKE_TOKEN=... $0" >&2
    exit 64  # EX_USAGE
fi
BASE="${BASE%/}"

failures=0

fail() {
    printf '  FAIL  %s\n' "$1" >&2
    failures=$((failures + 1))
}

pass() {
    printf '  ok    %s\n' "$1"
}

get() {
    # --fail-with-body so a 4xx/5xx still prints what the server said: the
    # refusals this stack produces are deliberately explanatory, and throwing
    # the body away turns a named reason into a status code.
    curl -sS --max-time 10 --fail-with-body "$@" 2>&1
}

echo "checking ${BASE}"

# 1. The engine is up at all.
if health="$(get "${BASE}/healthz")" && [[ "$health" == *'"ok":true'* ]]; then
    pass "the engine answers"
else
    fail "the engine did not answer /healthz — nothing below will mean anything"
    exit 1
fi

# 2. The core is behind it. This is the check the top-level `ok` cannot make.
ready="$(get "${BASE}/readyz" || true)"
core_detail="$(printf '%s' "$ready" | tr ',' '\n' | grep -A2 'vogt_core' | tr '\n' ' ' || true)"
if printf '%s' "$ready" | grep -q '"name":"vogt_core","ok":true'; then
    pass "vogt-core is ready — ${core_detail}"
else
    fail "vogt-core is not ready: ${core_detail:-no vogt_core check in /readyz}"
    fail "  (a stack can be 'ready' with no core at all — that is FR-E9, and it"
    fail "   is why this script exists rather than a healthcheck)"
fi

# 3. The two halves agree about where the estate is (FR-E3).
if printf '%s' "$ready" | grep -q '"name":"workspace_agreement","ok":true'; then
    pass "the import root and the workspace root agree"
else
    fail "workspace_agreement is false: imported projects will be invisible to"
    fail "  sessions and to the collectors running here (FR-E3, §6.3)"
fi

# 3a. A backup taken here would cover the engine as well as the core (NFR-I6).
#     The failure is invisible until a restore: `vogt backup` treats absent
#     engine state as non-fatal, so a misconfigured pair produces an archive
#     that looks complete and has no session history in it.
if printf '%s' "$ready" | grep -q '"name":"backup_agreement","ok":true'; then
    pass "a backup here would cover both halves"
else
    fail "backup_agreement is false: \`vogt backup\` would succeed and contain"
    fail "  no session history, push subscriptions or VAPID keypair (NFR-I6)."
    fail "  VOGT_ENGINE_STATE_DIR and the engine's state_dir must be one path"
fi

# 4. A client can discover Vogt without provoking a 503.
if get "${BASE}/api/config" | grep -q '"configured":true'; then
    pass "/api/config advertises Vogt, so the GUI will offer its tabs"
else
    fail "/api/config says no core is configured — the Vogt tabs will not appear"
fi

# 5. The proxy actually reaches the core, as a caller.
if [[ -n "$TOKEN" ]]; then
    if status="$(get -H "Authorization: Bearer ${TOKEN}" "${BASE}/api/vogt/status")"; then
        principal="$(printf '%s' "$status" | tr ',' '\n' | grep principal || true)"
        pass "the front door reaches the core as ${principal:-an actor it did not name}"
    else
        fail "GET /api/vogt/status: ${status}"
        fail "  a 503 naming vogt_core_token means this token has no paired core"
        fail "  token and no fallback is configured (FR-S9)"
    fi
else
    printf '  skip  /api/vogt (no token given; pass one to check the proxy)\n'
fi

echo
if (( failures > 0 )); then
    echo "${failures} check(s) failed — see DEPLOYMENT.md §9 before deploying further" >&2
    exit 1
fi
echo "the stack is merged and both halves answer"
