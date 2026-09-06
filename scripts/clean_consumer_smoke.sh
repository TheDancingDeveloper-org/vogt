#!/usr/bin/env bash
# Clean-consumer smoke (#613): prove a stranger can run the published all-in-one
# product from anonymous images alone — no checkout build, no private
# credential, no knowledge of the maintainer estate.
#
#     scripts/clean_consumer_smoke.sh
#     VOGT_STACK_IMAGE=…/vogt-stack:0.5.4 VOGT_VOICE_IMAGE=…/vogt-voice:0.5.4 \
#         scripts/clean_consumer_smoke.sh
#
# It differs from `e2e_stack_smoke.sh` (which walks a *running* stack somebody
# else stood up) by owning the whole consumer path itself:
#
#   1. Resolve both images' digests over an *anonymous* registry token — the
#      pull a stranger gets, proving the packages are public (#266) — and pull
#      them by that digest. A private package fails here, before anything runs.
#   2. Boot `deploy/stack.compose.yml` from `deploy/stack.env.example` in an
#      isolated working directory, exactly as GETTING_STARTED tells a new user
#      to, with the images pinned to the digests resolved above.
#   3. Walk the front door: readiness, the PWA, the first token, one core
#      read/write, and — with voice on — a full speech round trip through the
#      `voice` sidecar (synthesise a phrase, feed the audio back, read the
#      transcript). This is the "deterministic STT/TTS routing" #613 asks for:
#      the audio provably reached the sidecar and came back.
#   4. Repeat with voice disabled (`COMPOSE_PROFILES` cleared) and verify the
#      rest of the product is still usable while speech reports unavailable
#      (the engine's 404 fallback, FR-T6) rather than erroring.
#   5. Record each image's digest and provenance (labels + any SLSA
#      attestation) to a receipt file, so a run is auditable after the fact.
#
# The stack is a *development pod* (sudo, sshd, agent CLIs); this smoke runs it
# on loopback in a throwaway project and tears it down. Do not point it at a
# shared host.
set -euo pipefail

unset CDPATH
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
COMPOSE_FILE="${REPO_ROOT}/deploy/stack.compose.yml"
ENV_EXAMPLE="${REPO_ROOT}/deploy/stack.env.example"

# ── inputs ───────────────────────────────────────────────────────────────────
# Default the images to this checkout's product version, so CI tests the pair a
# release of *this* commit would publish; override either to test another pair.
VERSION="$(sed -n 's/^__version__ = "\(.*\)"/\1/p' "${REPO_ROOT}/src/vogt/__init__.py" 2>/dev/null || true)"
VERSION="${VERSION:-latest}"
STACK_IMAGE="${VOGT_STACK_IMAGE:-ghcr.io/thedancingdeveloper-org/vogt-stack:${VERSION}}"
VOICE_IMAGE="${VOGT_VOICE_IMAGE:-ghcr.io/thedancingdeveloper-org/vogt-voice:${VERSION}}"
REGISTRY="${VOGT_SMOKE_REGISTRY:-ghcr.io}"
RECEIPT="${VOGT_SMOKE_RECEIPT:-${REPO_ROOT}/clean-consumer-receipt.json}"
KEEP="${VOGT_SMOKE_KEEP:-}"           # non-empty: leave the last stack running
RUN_MARKER="clean-consumer-$(date -u +%Y%m%dT%H%M%SZ)-$$"

# ── reporting helpers (the e2e_stack_smoke.sh vocabulary) ─────────────────────
failures=0
fail() { printf '  FAIL  %s\n' "$1" >&2; failures=$((failures + 1)); }
pass() { printf '  ok    %s\n' "$1"; }
skip() { printf '  skip  %s\n' "$1"; }
info() { printf '    %s\n' "$1" >&2; }

get() { curl -sS --max-time 30 --fail-with-body "$@" 2>&1; }

# Read one value out of a JSON document on stdin — Python against `d`, so the
# smoke carries no `jq` dependency the runner may lack.
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

need() { command -v "$1" >/dev/null 2>&1 || { echo "clean-consumer smoke needs '$1' on PATH" >&2; exit 69; }; }

# ── anonymous registry resolution (the stranger's pull) ───────────────────────
# Split `ghcr.io/owner/name:tag` or `…@sha256:…` into repo path and reference.
image_repo() { local i="${1#*/}"; echo "${i%%:*}" | sed 's/@sha256.*//'; }
image_ref() {
    case "$1" in
        *@*) echo "${1##*@}" ;;
        *:*) echo "${1##*:}" ;;
        *) echo "latest" ;;
    esac
}

# Resolve a digest using an anonymously-obtained pull token. Succeeds only for a
# public package — the point of the check. Prints the sha256 digest on success.
anon_digest() {
    local image="$1" repo ref token digest
    repo="$(image_repo "$image")"
    ref="$(image_ref "$image")"
    token="$(get "https://${REGISTRY}/token?scope=repository:${repo}:pull" | jget 'd["token"]' 2>/dev/null || true)"
    if [[ -z "$token" ]]; then
        return 1
    fi
    digest="$(curl -sS --max-time 30 -o /dev/null -D - \
        -H "Authorization: Bearer ${token}" \
        -H 'Accept: application/vnd.oci.image.index.v1+json' \
        -H 'Accept: application/vnd.docker.distribution.manifest.list.v2+json' \
        -H 'Accept: application/vnd.docker.distribution.manifest.v2+json' \
        "https://${REGISTRY}/v2/${repo}/manifests/${ref}" 2>/dev/null \
        | tr -d '\r' | awk -F': ' 'tolower($1)=="docker-content-digest"{print $2}')"
    [[ -n "$digest" ]] && printf '%s' "$digest"
}

# ── receipt (digests + provenance) ────────────────────────────────────────────
declare -A RESOLVED     # image ref -> repo@digest
record_image() {
    # $1 image ref, $2 friendly name. Resolves anonymously, pulls by digest, and
    # appends a receipt entry (digest, labels, and any SLSA provenance).
    local image="$1" name="$2" repo digest pinned labels prov
    repo="${REGISTRY}/$(image_repo "$image")"
    printf '\n== image: %s (%s) ==\n' "$name" "$image"

    if ! digest="$(anon_digest "$image")"; then
        fail "could not resolve ${image} over an anonymous pull token — the"
        fail "  package is private or unreachable, so a stranger cannot pull it"
        return 1
    fi
    pass "resolved ${name} anonymously (${digest})"
    pinned="${repo}@${digest}"
    RESOLVED["$name"]="$pinned"

    if docker pull --quiet "$pinned" >/dev/null 2>&1; then
        pass "pulled ${name} by digest"
    else
        fail "docker pull ${pinned} failed"
        return 1
    fi

    # Labels and provenance are read from files, not passed as argv: an image's
    # label set (and its SLSA provenance) is large enough to blow past ARG_MAX,
    # which failed the receipt with "Argument list too long" on a real host.
    local labels_file prov_file
    labels_file="$(mktemp)"
    prov_file="$(mktemp)"
    docker inspect --format '{{json .Config.Labels}}' "$pinned" >"$labels_file" 2>/dev/null || echo null >"$labels_file"
    docker buildx imagetools inspect "$pinned" --format '{{json .Provenance}}' >"$prov_file" 2>/dev/null || echo null >"$prov_file"
    RECEIPT_ENTRIES+=("$(python3 -c '
import json, sys
labels = json.load(open(sys.argv[5])) or {}
prov_raw = open(sys.argv[6]).read().strip()
prov = json.loads(prov_raw) if prov_raw and prov_raw != "null" else None
print(json.dumps({
    "name": sys.argv[1], "image": sys.argv[2], "pinned": sys.argv[3],
    "digest": sys.argv[4], "labels": labels, "provenance": prov,
}))
' "$name" "$image" "$pinned" "$digest" "$labels_file" "$prov_file")")
    rm -f "$labels_file" "$prov_file"
}

# ── the consumer path, once, at a given voice posture ─────────────────────────
BASE=""
TOKEN=""

find_free_port() {
    python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()'
}

boot_stack() {
    # $1 = "on" | "off". Generates an isolated deploy dir from the tracked
    # example and brings the stack up pinned to the resolved digests.
    local voice="$1" port workdir env_file profiles
    port="$(find_free_port)"
    workdir="$(mktemp -d)"
    STACK_WORKDIR="$workdir"
    STACK_PROJECT="${RUN_MARKER//[^a-z0-9]/}-${voice}"
    cp "$COMPOSE_FILE" "${workdir}/stack.compose.yml"
    openssl rand -hex 32 >"${workdir}/vogt-core-token"

    profiles="voice"
    [[ "$voice" == "off" ]] && profiles=""

    env_file="${workdir}/.env"
    # Seed from the shipped example, then override only the consumer's choices.
    grep -vE '^(ENGINE_TOKEN|ENGINE_BIND|ENGINE_PORT|COMPOSE_PROFILES|VOGT_STACK_IMAGE|VOGT_VOICE_IMAGE)=' \
        "$ENV_EXAMPLE" >"$env_file"
    {
        echo "ENGINE_TOKEN=$(openssl rand -hex 24)"
        echo "ENGINE_BIND=127.0.0.1"
        echo "ENGINE_PORT=${port}"
        echo "COMPOSE_PROFILES=${profiles}"
        echo "VOGT_STACK_IMAGE=${RESOLVED[stack]}"
        echo "VOGT_VOICE_IMAGE=${RESOLVED[voice]}"
    } >>"$env_file"

    BASE="http://127.0.0.1:${port}"
    TOKEN="$(sed -n 's/^ENGINE_TOKEN=//p' "$env_file")"

    info "booting stack (voice ${voice}) as project ${STACK_PROJECT} on ${BASE}"
    # `--project-directory` fixes where the compose file's relative paths (the
    # `./vogt-core-token` secret) resolve, independent of this script's cwd.
    if ! docker compose -p "$STACK_PROJECT" --project-directory "$workdir" \
        --env-file "$env_file" -f "${workdir}/stack.compose.yml" \
        up -d --wait --wait-timeout 300 >"${workdir}/up.log" 2>&1; then
        fail "the stack did not come up healthy within the timeout (voice ${voice})"
        sed 's/^/      /' "${workdir}/up.log" >&2 || true
        return 1
    fi
    pass "the stack is up and healthy (voice ${voice})"
}

teardown_stack() {
    [[ -n "${STACK_PROJECT:-}" ]] || return 0
    if [[ -n "$KEEP" ]]; then
        skip "leaving ${STACK_PROJECT} running (VOGT_SMOKE_KEEP set) at ${BASE}"
        return 0
    fi
    docker compose -p "$STACK_PROJECT" --project-directory "${STACK_WORKDIR}" \
        --env-file "${STACK_WORKDIR}/.env" -f "${STACK_WORKDIR}/stack.compose.yml" \
        down -v --remove-orphans >/dev/null 2>&1 || true
    rm -rf "${STACK_WORKDIR}" 2>/dev/null || true
    STACK_PROJECT=""
}

api_get() { get -H "Authorization: Bearer ${TOKEN}" "${BASE}/api/vogt/$1"; }
api_post() {
    get -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data "$2" "${BASE}/api/vogt/$1"
}
engine_post_json() {
    get -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data "$2" "${BASE}/api/$1"
}

walk_core() {
    local body ref got
    # Readiness through the front door (the two probes a browser and an agent hit).
    if get "${BASE}/healthz" | grep -q '"ok":true'; then
        pass "the front door is live (/healthz)"
    else
        fail "GET /healthz did not report ok — nothing below can be trusted"
        return
    fi
    if get "${BASE}/readyz" | grep -q '"name":"vogt_core","ok":true'; then
        pass "vogt-core is ready behind the front door (/readyz)"
    else
        fail "vogt-core is not ready in /readyz — a stack can be 'ready' with no"
        fail "  core at all (FR-E9); the AIO must carry its own"
    fi
    # The PWA at /.
    if get "${BASE}/" | grep -qi '<!doctype html'; then
        pass "the PWA is served at / (HTML document)"
    else
        fail "GET / did not return an HTML document — the embedded PWA is missing"
    fi
    # The token reaches the core.
    if api_get "status" >/dev/null; then
        pass "the front-door token reaches the core (/api/vogt/status)"
    else
        fail "the token did not reach the core via /api/vogt/status"
        return
    fi
    # One core write, then read it back.
    if ! body="$(api_post "work" \
        "{\"kind\":\"chore\",\"title\":\"${RUN_MARKER} native work\",\"priority\":\"p2\",\"reason\":\"clean consumer smoke\"}")"; then
        fail "POST /api/vogt/work failed — the credential-free write path is broken: ${body}"
        return
    fi
    ref="$(printf '%s' "$body" | jget 'd["item"]["ref"]' || true)"
    if [[ -z "$ref" ]]; then
        fail "work.create returned no item ref: ${body}"
        return
    fi
    got="$(api_get "work/${ref}" | jget 'd["item"]["ref"]' 2>/dev/null || true)"
    if [[ "$got" == "$ref" ]]; then
        pass "created and read back a native work item (${ref})"
    else
        fail "created ${ref} but could not read it back (got '${got}')"
    fi
}

walk_voice_on() {
    # A full round trip through the sidecar: synthesise, then transcribe the
    # synthesised audio. Success proves the engine routed both halves to the
    # voice service and the baked models answered.
    local wav transcript text phrase="the quick brown fox jumps over the lazy dog"
    wav="$(mktemp --suffix=.wav)"
    if get -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data "{\"text\":\"${phrase}\"}" -o "$wav" "${BASE}/api/assistant/tts" \
        && [[ "$(head -c 4 "$wav" 2>/dev/null)" == "RIFF" ]]; then
        pass "TTS synthesised WAV audio through the voice sidecar ($(wc -c <"$wav") bytes)"
    else
        fail "POST /api/assistant/tts did not return WAV audio — the engine is not"
        fail "  routing synthesis to the voice sidecar (voice on)"
        rm -f "$wav"; return
    fi
    if transcript="$(get -H "Authorization: Bearer ${TOKEN}" \
        -F "file=@${wav};type=audio/wav" "${BASE}/api/assistant/stt")"; then
        text="$(printf '%s' "$transcript" | jget 'd["text"].lower()' 2>/dev/null || true)"
        if printf '%s' "$text" | grep -qE 'quick|brown|fox|lazy|dog'; then
            pass "STT transcribed the synthesised audio back (\"$(echo "$text" | tr -s ' ' | cut -c1-48)…\")"
        else
            fail "STT answered but the transcript matched none of the spoken words: ${transcript}"
        fi
    else
        fail "POST /api/assistant/stt failed on the synthesised audio: ${transcript}"
    fi
    rm -f "$wav"
}

walk_voice_off() {
    # With the profile cleared the sidecar is absent, so the engine finds nothing
    # at the speech URL and answers 404 (FR-T6). That is the honest "unavailable"
    # state — the rest of the product (walked above) must still work.
    local status
    status="$(curl -sS --max-time 30 -o /dev/null -w '%{http_code}' \
        -H "Authorization: Bearer ${TOKEN}" -H "Content-Type: application/json" \
        -X POST --data '{"text":"hello"}' "${BASE}/api/assistant/tts" 2>/dev/null || true)"
    if [[ "$status" == "404" ]]; then
        pass "with voice disabled, speech reports unavailable (404) and the rest works"
    else
        fail "with voice disabled, /api/assistant/tts answered ${status}, not the"
        fail "  expected 404 fallback — voice-off is not the honest-unavailable state"
    fi
}

run_posture() {
    local voice="$1"
    printf '\n======== consumer path: voice %s ========\n' "$voice"
    if ! boot_stack "$voice"; then
        teardown_stack
        return
    fi
    walk_core
    if [[ "$voice" == "on" ]]; then
        walk_voice_on
    else
        walk_voice_off
    fi
    teardown_stack
}

# ── run ────────────────────────────────────────────────────────────────────
need docker
need curl
need python3
need openssl
docker compose version >/dev/null 2>&1 || { echo "clean-consumer smoke needs the docker compose plugin" >&2; exit 69; }

echo "clean-consumer smoke"
echo "  stack image: ${STACK_IMAGE}"
echo "  voice image: ${VOICE_IMAGE}"
echo "  run marker:  ${RUN_MARKER}"

RECEIPT_ENTRIES=()
record_image "$STACK_IMAGE" "stack" || true
record_image "$VOICE_IMAGE" "voice" || true

if ((failures > 0)) || [[ -z "${RESOLVED[stack]:-}" || -z "${RESOLVED[voice]:-}" ]]; then
    echo >&2
    echo "cannot boot: one or both images were not resolvable over an anonymous pull" >&2
else
    trap 'teardown_stack' EXIT
    run_posture on
    run_posture off
    trap - EXIT
fi

# ── receipt ──────────────────────────────────────────────────────────────────
python3 -c '
import json, sys
entries = [json.loads(e) for e in sys.argv[2:] if e]
json.dump({
    "run_marker": sys.argv[1],
    "images": entries,
}, open("'"$RECEIPT"'", "w"), indent=2)
open("'"$RECEIPT"'", "a").write("\n")
' "$RUN_MARKER" "${RECEIPT_ENTRIES[@]:-}" 2>/dev/null || true
echo
echo "receipt written to ${RECEIPT}"

echo
if ((failures > 0)); then
    echo "${failures} check(s) failed" >&2
    exit 1
fi
echo "a stranger can run the published stack, with and without voice"
