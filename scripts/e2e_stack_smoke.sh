#!/usr/bin/env bash
# End-to-end stack smoke (#295): walk the stranger's path against a running
# two-container stack, one HTTP step at a time, each step naming what its
# failure means.
#
#     scripts/e2e_stack_smoke.sh https://vogt.example.com [front-door-token]
#     VOGT_SMOKE_BASE=... VOGT_SMOKE_TOKEN=... scripts/e2e_stack_smoke.sh
#
# This is the mechanical form of the human-on-a-phone smoke #273 asked for. It
# is deliberately a *client*: it holds no core token and never reaches inside a
# container, so what it exercises is exactly what a browser and an agent reach
# — the front door the engine publishes (NFR-D11).
#
# Two halves, split by one credential:
#
#   * The credential-free half always runs: `/health/ready`, the PWA at `/`,
#     issuing (or accepting) the first front-door token, creating a native work
#     item, starting a session, and running a synthetic agent task through the
#     `fake-agent` preset (#296).
#   * The forge half needs a GitHub PAT for the fixture repo and is gated on
#     `VOGT_FIXTURE_PAT`. Unset, every forge step prints `SKIP (no
#     VOGT_FIXTURE_PAT)` and the smoke still passes on the half it could run.
#
# Timing is reported per step and as a total, so a regression in how long the
# stack takes to answer is visible rather than felt.
#
# The token, when passed, is a *front-door* token — the one a browser holds.
# Prefer `VOGT_SMOKE_TOKEN` over argv, which is visible in `ps`.
set -euo pipefail

# ── inputs ───────────────────────────────────────────────────────────────
BASE="${1:-${VOGT_SMOKE_BASE:-}}"
TOKEN="${2:-${VOGT_SMOKE_TOKEN:-}}"

if [[ -z "$BASE" ]]; then
    echo "usage: $0 <base-url> [front-door-token]" >&2
    echo "   or: VOGT_SMOKE_BASE=... VOGT_SMOKE_TOKEN=... $0" >&2
    exit 64 # EX_USAGE
fi
BASE="${BASE%/}"

unset CDPATH
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

# The fixture PAT gates the forge half. `fixture_reset.py` reads it under its
# own name (`VOGT_FIXTURE_TOKEN`); the front-door core reads it as a file token
# from the container, not from here — this smoke never sends it to the stack.
FIXTURE_PAT="${VOGT_FIXTURE_PAT:-}"
FIXTURE_REPO="${VOGT_FIXTURE_REPO:-TheDancingDeveloper-org/vogt-fixture}"

# The fake-agent, as the engine sees it. The e2e overlay mounts `scripts/`
# there and registers it as the `Fake Agent (test)` preset; override only if a
# deployment mounts it elsewhere.
FAKE_AGENT="${E2E_FAKE_AGENT:-/opt/vogt-e2e/scripts/fake-agent}"

# A run marker, so anything this smoke creates upstream is identifiable and its
# repeated runs do not collide.
RUN_MARKER="e2e-smoke-$(date -u +%Y%m%dT%H%M%SZ)-$$"

# ── reporting helpers (the smoke_merged_stack.sh vocabulary) ─────────────
failures=0

fail() {
    printf '  FAIL  %s\n' "$1" >&2
    failures=$((failures + 1))
}

pass() {
    printf '  ok    %s\n' "$1"
}

skip() {
    printf '  skip  %s\n' "$1"
}

# curl with `--fail-with-body`, so a 4xx/5xx still prints what the server said:
# this stack's refusals are deliberately explanatory, and discarding the body
# turns a named reason into a bare status code.
get() {
    curl -sS --max-time 20 --fail-with-body "$@" 2>&1
}

# Authenticated calls to the front door. `api_*` speak Vogt's `/api/vogt/*`
# (the engine injects the core token); `engine_*` speak the engine's own `/api`.
api_get() { get -H "Authorization: Bearer ${TOKEN}" "${BASE}/api/vogt/$1"; }
api_post() {
    get -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data "$2" "${BASE}/api/vogt/$1"
}
engine_post() {
    get -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data "$2" "${BASE}/api/$1"
}
engine_get() { get -H "Authorization: Bearer ${TOKEN}" "${BASE}/api/$1"; }

# The fixture forge, read/written directly to prove "appears upstream" without
# trusting the surface under test to report on itself.
gh_api() {
    curl -sS --max-time 20 --fail-with-body \
        -H "Authorization: Bearer ${FIXTURE_PAT}" \
        -H "Accept: application/vnd.github+json" \
        -H "X-GitHub-Api-Version: 2022-11-28" "$@" 2>&1
}

# Read one value out of a JSON document on stdin. A test script, so the
# expression is Python evaluated against `d`; keeps the smoke free of a `jq`
# dependency the runner may not carry.
jget() {
    python3 -c '
import sys, json
d = json.load(sys.stdin)
try:
    v = eval(sys.argv[1])
except Exception:
    sys.exit(3)
print("" if v is None else v)
' "$1"
}

# ── step framework with per-step timing ──────────────────────────────────
STEP_NAMES=()
STEP_MS=()

run_step() {
    local label="$1" fn="$2" t0 t1
    printf '\n== %s ==\n' "$label"
    t0=$(date +%s%3N)
    "$fn"
    t1=$(date +%s%3N)
    STEP_NAMES+=("$label")
    STEP_MS+=("$((t1 - t0))")
    printf '  … %d ms\n' "$((t1 - t0))"
}

# ── steps: the credential-free half ──────────────────────────────────────

step_health() {
    # The front door proxies the core's readiness probe (FR-A7). A stranger's
    # very first request; nothing below means anything if this does not answer.
    if get "${BASE}/health/ready" >/dev/null; then
        pass "the stack answers /health/ready"
    else
        fail "GET /health/ready did not succeed — the stack is not up, and no"
        fail "  step below can be trusted"
    fi
}

step_pwa() {
    # The PWA is served at `/`. A stranger opening the app sees HTML, not a
    # JSON 404 or an index that never shipped the bundle.
    local body
    if body="$(get "${BASE}/")" && printf '%s' "$body" | grep -qi '<!doctype html'; then
        pass "the PWA is served at / (HTML document returned)"
    else
        fail "GET / did not return an HTML document — the engine is up but is"
        fail "  not serving the embedded PWA (web/dist missing, or a routing"
        fail "  regression that answers / with something other than index.html)"
    fi
}

step_token() {
    # Either a token was handed to us, or the instance is still in first-run
    # install mode and we mint the first one (#292). A stranger who cannot get
    # a token cannot do anything past the two probes above.
    if [[ -n "$TOKEN" ]]; then
        if api_get "status" >/dev/null; then
            pass "the provided front-door token reaches the core"
        else
            fail "the provided token did not reach the core via /api/vogt/status"
            fail "  — a 503 naming vogt_core_token means it has no paired core"
            fail "  token (FR-S9); a 401 means the engine does not know it"
        fi
        return
    fi

    local status install_mode boot secret
    if ! status="$(get "${BASE}/api/install/status")"; then
        fail "no token given and GET /api/install/status failed — cannot tell"
        fail "  whether the first-run wizard is open (#292)"
        return
    fi
    install_mode="$(printf '%s' "$status" | jget 'd["install_mode"]' || true)"
    if [[ "$install_mode" != "True" ]]; then
        fail "no token given and install mode is closed: the first token was"
        fail "  already issued, so pass one as argv[2] or VOGT_SMOKE_TOKEN"
        return
    fi
    if ! boot="$(api_bootstrap)"; then
        fail "POST /api/install/bootstrap failed while install mode was open"
        return
    fi
    secret="$(printf '%s' "$boot" | jget 'd["secret"]' || true)"
    if [[ -z "$secret" ]]; then
        fail "the bootstrap answered without a secret — no token to continue with"
        return
    fi
    TOKEN="$secret"
    pass "issued the first front-door token via the install bootstrap"
}

api_bootstrap() {
    get -H "Content-Type: application/json" -X POST \
        --data "{\"display_name\":\"E2E Smoke\",\"token_name\":\"${RUN_MARKER}\"}" \
        "${BASE}/api/install/bootstrap"
}

step_work_create() {
    # A native work item — no project, no forge, no credential (FR-W1). Proves
    # the whole write path (auth → scope → declared store → audit) end to end.
    local body ref
    if ! body="$(api_post "work" \
        "{\"kind\":\"chore\",\"title\":\"${RUN_MARKER} native work\",\"priority\":\"p2\",\"reason\":\"e2e stack smoke\"}")"; then
        fail "POST /api/vogt/work failed — the credential-free write path is"
        fail "  broken (auth, scope, or the declared store): ${body}"
        return
    fi
    ref="$(printf '%s' "$body" | jget 'd["item"]["ref"]' || true)"
    if [[ -n "$ref" ]]; then
        pass "created a native work item (${ref})"
    else
        fail "POST /api/vogt/work returned no item ref: ${body}"
    fi
}

step_session() {
    # The engine owns the terminals a work item's session runs in. A trivially
    # short command proves creation without leaving a process behind.
    local body id
    if ! body="$(engine_post "sessions" \
        "{\"name\":\"${RUN_MARKER}\",\"command\":[\"/bin/true\"]}")"; then
        fail "POST /api/sessions failed — the engine will not open a terminal:"
        fail "  ${body}"
        return
    fi
    id="$(printf '%s' "$body" | jget 'd["id"]' || true)"
    if [[ -n "$id" ]]; then
        pass "started a session (${id})"
    else
        fail "POST /api/sessions returned no session id: ${body}"
    fi
}

step_agent_task() {
    # A synthetic agent task through the `fake-agent` preset (#296): the run
    # prints a VOGT_NOTIFY line, which the engine records as a durable finding
    # (FR-E7). This is the run-orchestration path a real agent CLI drives, with
    # a deterministic stand-in so it runs in CI.
    local created task_id notify_text deadline detail findings
    notify_text="${RUN_MARKER} synthetic finding"
    if ! created="$(engine_post "agent-tasks" \
        "{\"name\":\"${RUN_MARKER}\",\"prompt\":\"Report a finding.\",\"schedule\":{\"kind\":\"manual\"},\"command\":[\"${FAKE_AGENT}\",\"findings\"],\"env\":[[\"FAKE_AGENT_NOTIFY_TEXT\",\"${notify_text}\"]]}")"; then
        fail "POST /api/agent-tasks failed — cannot create the synthetic task:"
        fail "  ${created}"
        fail "  (is the Fake Agent preset registered and ${FAKE_AGENT} mounted?)"
        return
    fi
    task_id="$(printf '%s' "$created" | jget 'd["id"]' || true)"
    if [[ -z "$task_id" ]]; then
        fail "POST /api/agent-tasks returned no task id: ${created}"
        return
    fi
    if ! engine_post "agent-tasks/${task_id}/run" '' >/dev/null; then
        fail "POST /api/agent-tasks/${task_id}/run failed — the run did not start"
        return
    fi

    # Poll until the run records a finding, or time out.
    deadline=$(($(date +%s) + 40))
    while true; do
        detail="$(engine_get "agent-tasks/${task_id}" || true)"
        findings="$(printf '%s' "$detail" | jget 'len(d["runs"][0]["findings"])' 2>/dev/null || echo 0)"
        if [[ "$findings" =~ ^[0-9]+$ ]] && ((findings > 0)); then
            pass "the fake-agent run recorded a finding (FR-E7)"
            return
        fi
        if (($(date +%s) >= deadline)); then
            fail "the fake-agent run recorded no finding within 40s — the run"
            fail "  did not reach the VOGT_NOTIFY watcher (preset command wrong,"
            fail "  fake-agent not executable, or python3 absent in the engine)"
            return
        fi
        sleep 1
    done
}

# ── steps: the forge half (gated on VOGT_FIXTURE_PAT) ─────────────────────

FORGE_PROJECT=""

step_forge_reset() {
    # Rebuild the fixture repo to its known state (#294). Idempotent and
    # additive; never deletes history.
    if VOGT_FIXTURE_TOKEN="$FIXTURE_PAT" python3 "${REPO_ROOT}/scripts/fixture_reset.py" \
        --repo "$FIXTURE_REPO" >/dev/null; then
        pass "fixture repo reset to its known state (${FIXTURE_REPO})"
    else
        fail "fixture_reset.py failed — the fixture is not in a known state, so"
        fail "  the assertions below have no baseline to compare against"
    fi
}

step_forge_import() {
    # Clone the fixture into the import root, register it, and read its existing
    # issues/PRs/labels (FR-P6, FR-B3). Uses the core's github.com file token.
    local body slug
    if ! body="$(api_post "projects/import" \
        "{\"repo\":\"${FIXTURE_REPO}\",\"consolidate\":true,\"reason\":\"e2e stack smoke\"}")"; then
        fail "POST /api/vogt/projects/import failed — the core could not clone or"
        fail "  read the fixture (no usable github.com credential in the core, or"
        fail "  the repo is unreachable): ${body}"
        return
    fi
    slug="$(printf '%s' "$body" | jget 'd["project"]["slug"]' || true)"
    if [[ -n "$slug" ]]; then
        FORGE_PROJECT="$slug"
        pass "imported the fixture as project '${slug}'"
    else
        fail "projects/import returned no project slug: ${body}"
    fi
}

step_forge_link() {
    # Make the imported project upstream-truth (#181), so a work item created in
    # it is written through to the forge.
    if [[ -z "$FORGE_PROJECT" ]]; then
        fail "no imported project to link (import step did not succeed)"
        return
    fi
    local body
    if body="$(api_post "forge/link" \
        "{\"project\":\"${FORGE_PROJECT}\",\"reason\":\"e2e stack smoke\"}")"; then
        pass "linked '${FORGE_PROJECT}' as upstream-truth"
    else
        fail "POST /api/vogt/forge/link failed — the project has no repo_url a"
        fail "  provider matches, or no usable credential: ${body}"
    fi
}

step_forge_sweep() {
    # Run the collectors over the linked project, so its issues and PRs land as
    # observations the backlog and the observed graph read from.
    if [[ -z "$FORGE_PROJECT" ]]; then
        fail "no imported project to sweep (import step did not succeed)"
        return
    fi
    local body
    if body="$(api_post "sweep" \
        "{\"project\":\"${FORGE_PROJECT}\",\"reason\":\"e2e stack smoke\"}")"; then
        pass "swept '${FORGE_PROJECT}'"
    else
        fail "POST /api/vogt/sweep failed — the collectors did not run: ${body}"
    fi
}

step_forge_backlog() {
    # The fixture ships open issues and PRs, so its backlog must be non-empty
    # after a sweep. An empty backlog here means the sweep observed nothing.
    if [[ -z "$FORGE_PROJECT" ]]; then
        fail "no imported project to rank (import step did not succeed)"
        return
    fi
    local body count total
    if ! body="$(api_get "backlog?project=${FORGE_PROJECT}")"; then
        fail "GET /api/vogt/backlog failed: ${body}"
        return
    fi
    count="$(printf '%s' "$body" | jget 'len(d["items"])' 2>/dev/null || echo 0)"
    total="$(printf '%s' "$body" | jget 'd["total_considered"]' 2>/dev/null || echo 0)"
    if [[ "$count" =~ ^[0-9]+$ ]] && ((count > 0)); then
        pass "the fixture backlog is non-empty (${count} of ${total} ranked)"
    else
        fail "the fixture backlog is empty after a sweep — the collectors ran"
        fail "  but observed no issues/PRs, so nothing reached the ranked view"
    fi
}

step_forge_observed_pr() {
    # The fixture's pull requests must appear as observed edges (forge.pull_request
    # observations). This is the read that #273's acceptance turns on.
    if [[ -z "$FORGE_PROJECT" ]]; then
        fail "no imported project to read observations from"
        return
    fi
    local body count
    if ! body="$(api_get "observations?project=${FORGE_PROJECT}&kind=forge.pull_request")"; then
        fail "GET /api/vogt/observations failed: ${body}"
        return
    fi
    count="$(printf '%s' "$body" | jget 'len(d["observations"])' 2>/dev/null || echo 0)"
    if [[ "$count" =~ ^[0-9]+$ ]] && ((count > 0)); then
        pass "the fixture pull requests appear as observed edges (${count})"
    else
        fail "no forge.pull_request observations after a sweep — a PR the fixture"
        fail "  ships is not being observed (collector or provider regression)"
    fi
}

step_forge_upstream_work() {
    # Create a work item in the linked project. On a linked project this is a
    # write-through: the core calls provider.create_issue and the item's ref
    # becomes the upstream subject key (#181). Verified against the forge
    # itself, then closed as cleanup so repeated runs do not pile up open
    # issues on the fixture.
    if [[ -z "$FORGE_PROJECT" ]]; then
        fail "no linked project to create an upstream work item in"
        return
    fi
    local title body ref found number
    title="${RUN_MARKER} upstream work"
    if ! body="$(api_post "work" \
        "{\"kind\":\"feature\",\"title\":\"${title}\",\"project\":\"${FORGE_PROJECT}\",\"reason\":\"e2e stack smoke\"}")"; then
        fail "POST /api/vogt/work (with project) failed — the upstream write-through"
        fail "  did not complete: ${body}"
        return
    fi
    ref="$(printf '%s' "$body" | jget 'd["item"]["ref"]' || true)"

    # Confirm upstream, from the forge's own side.
    if ! found="$(gh_api "https://api.github.com/repos/${FIXTURE_REPO}/issues?state=open&per_page=100")"; then
        fail "could not read the fixture issues back from GitHub to confirm the"
        fail "  work item appeared upstream: ${found}"
        return
    fi
    number="$(printf '%s' "$found" |
        python3 -c 'import sys,json;t=sys.argv[1];print(next((str(i["number"]) for i in json.load(sys.stdin) if i.get("title")==t),""))' "$title" 2>/dev/null || true)"
    if [[ -n "$number" ]]; then
        pass "the work item appears upstream as issue #${number} (ref ${ref})"
        # Cleanup: close the issue we created (never delete — history stands).
        if gh_api -X PATCH \
            -H "Content-Type: application/json" \
            --data '{"state":"closed"}' \
            "https://api.github.com/repos/${FIXTURE_REPO}/issues/${number}" >/dev/null; then
            skip "closed the smoke's upstream issue #${number} as cleanup"
        else
            skip "could not close upstream issue #${number} (leaving it open)"
        fi
    else
        fail "created work item ${ref} but no matching issue titled '${title}'"
        fail "  is open upstream — the write-through did not reach the forge"
    fi
}

# ── run ──────────────────────────────────────────────────────────────────
echo "e2e stack smoke against ${BASE}"
echo "run marker: ${RUN_MARKER}"

run_step "health: /health/ready" step_health
run_step "pwa: GET /" step_pwa
run_step "token: first token or provided" step_token
run_step "work-create: native item" step_work_create
run_step "session: start a terminal" step_session
run_step "agent: fake-agent synthetic task" step_agent_task

if [[ -n "$FIXTURE_PAT" ]]; then
    run_step "forge: reset fixture" step_forge_reset
    run_step "forge: import fixture" step_forge_import
    run_step "forge: link upstream-truth" step_forge_link
    run_step "forge: sweep" step_forge_sweep
    run_step "forge: backlog non-empty" step_forge_backlog
    run_step "forge: PR observed edge" step_forge_observed_pr
    run_step "forge: work appears upstream" step_forge_upstream_work
else
    echo
    echo "== forge half =="
    skip "SKIP (no VOGT_FIXTURE_PAT) — reset/import/link/sweep/backlog/observed/upstream"
    skip "  the credential-free half above is the whole run without the PAT"
fi

# ── timing report ─────────────────────────────────────────────────────────
echo
echo "timing:"
total=0
for i in "${!STEP_NAMES[@]}"; do
    printf '  %6d ms  %s\n' "${STEP_MS[$i]}" "${STEP_NAMES[$i]}"
    total=$((total + STEP_MS[i]))
done
printf '  %6d ms  TOTAL\n' "$total"

echo
if ((failures > 0)); then
    echo "${failures} check(s) failed" >&2
    exit 1
fi
echo "the stranger's path holds end to end"
