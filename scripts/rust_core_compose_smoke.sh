#!/usr/bin/env bash
# Compose smoke for the Rust core in the contributor stack's core slot.
#
# Builds deploy/rust-core.Dockerfile, brings up
# deploy/vogt.compose.yml + deploy/engine.overlay.yml + deploy/rust-core.overlay.yml
# with a throwaway project name, and checks the three things the WI-1076
# done-when names that a unit test cannot:
#
#   * the core container becomes healthy (init adopted the stack secret)
#   * the engine's event follower reads that core with the stack secret
#     (an event written after boot appears on the engine's event stream)
#   * a session.* call on the Rust core reaches the real engine
#     (session.list returns the engine's own session list, not a stand-in)
#
# Usage: scripts/rust_core_compose_smoke.sh
# Needs docker. Tears the project down on exit, success or failure.
set -euo pipefail

unset CDPATH
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

PROJECT="vogt-rust-core-smoke-$$"
COMPOSE=(docker compose -p "$PROJECT"
  -f deploy/vogt.compose.yml
  -f deploy/engine.overlay.yml
  -f deploy/rust-core.overlay.yml)
SECRET="$(mktemp)"
cleanup() {
  "${COMPOSE[@]}" down -v --remove-orphans >/dev/null 2>&1 || true
  rm -f "$SECRET"
}
trap cleanup EXIT

openssl rand -hex 32 > "$SECRET"
chmod 644 "$SECRET"
# The overlay's secret sources are relative paths beside the compose files.
cp "$SECRET" deploy/vogt-core-token
printf '%s' "$ENGINE_TOKEN" > deploy/engine-token
chmod 644 deploy/engine-token
trap 'cleanup; rm -f deploy/vogt-core-token deploy/engine-token' EXIT

export VOGT_IMAGE="vogt-core:rust"
export VOGT_PUBLIC_URL="http://127.0.0.1:18080"
export ENGINE_PUBLIC_URL="http://127.0.0.1:18910"
export ENGINE_TOKEN="engine-token-$(openssl rand -hex 16)"
export ENGINE_BIND="127.0.0.1"
export ENGINE_PORT="18910"
export VOGT_BIND_IP="127.0.0.1"
export VOGT_PORT="18080"

echo "building the Rust core image"
docker build -t vogt-core:rust -f deploy/rust-core.Dockerfile .

echo "building the engine image with the Rust core in its core slot"
docker build -t vogt-engine:local -f engine/Dockerfile --build-arg CORE_IMAGE=vogt-core:rust .

echo "starting the stack"
"${COMPOSE[@]}" up -d

echo "waiting for the core to be healthy"
ok=""
for _ in $(seq 1 60); do
  state="$("${COMPOSE[@]}" ps --format '{{.Service}} {{.Health}}' | awk '$1=="vogt"{print $2}')"
  if [ "$state" = "healthy" ]; then ok=yes; break; fi
  sleep 5
done
if [ -z "$ok" ]; then
  echo "vogt (Rust core) did not become healthy" >&2
  "${COMPOSE[@]}" ps || true
  "${COMPOSE[@]}" logs --tail=120 vogt || true
  exit 1
fi
echo "core is healthy"

echo "waiting for the engine"
ok=""
for _ in $(seq 1 60); do
  if "${COMPOSE[@]}" exec -T engine curl -fsS http://127.0.0.1:8910/readyz >/dev/null 2>&1; then
    ok=yes; break
  fi
  sleep 5
done
if [ -z "$ok" ]; then
  echo "engine never became ready" >&2
  "${COMPOSE[@]}" logs --tail=120 engine || true
  exit 1
fi

# The event follower polls every 5s, starting at the head. Write an event
# after it has started, then look for it on the engine's stream.
echo "writing an event through the core and reading it from the engine"
"${COMPOSE[@]}" exec -T -e VOGT_CORE_TOKEN_FILE=/run/secrets/vogt_core_token engine \
  sh -c 'tok=$(cat "$VOGT_CORE_TOKEN_FILE"); curl -fsS -H "Authorization: Bearer $tok" -H "content-type: application/json" \
    -d "{\"kind\":\"chore\",\"title\":\"rust-core smoke event\",\"reason\":\"compose smoke\",\"project\":\"\"}" \
    http://vogt:8000/api/work' >/dev/null

found=""
for _ in $(seq 1 12); do
  # The stream never closes, so curl exits non-zero on --max-time and a
  # pipefail pipeline would fail the smoke on the first event it saw. Bound
  # the read by line count instead, and match the event kind exactly.
  if "${COMPOSE[@]}" exec -T engine sh -c \
      "curl -sS -N --max-time 8 -H \"Authorization: Bearer $ENGINE_TOKEN\" http://127.0.0.1:8910/api/events | head -n 40" \
      | grep -q '"kind":"work.created"'; then
    found=yes; break
  fi
  sleep 2
done
if [ -z "$found" ]; then
  echo "the engine event stream never carried the core event (follower did not read the Rust core)" >&2
  exit 1
fi
echo "event follower saw the Rust core"

# session.* on the Rust core, against the real engine. Start one through the
# core so the row is the engine's own session, then read the engine's list
# and require the core's answer to name that same id. Matching the word
# "sessions" would pass against the empty unconfigured list.
echo "session.start on the Rust core"
started="$("${COMPOSE[@]}" exec -T -e VOGT_CORE_TOKEN_FILE=/run/secrets/vogt_core_token engine \
  sh -c 'tok=$(cat "$VOGT_CORE_TOKEN_FILE"); curl -fsS -H "Authorization: Bearer $tok" -H "content-type: application/json" \
    -d "{\"name\":\"rust-core-smoke\",\"reason\":\"compose smoke\"}" \
    http://vogt:8000/api/sessions')"
engine_id="$(printf '%s' "$started" | sed -n 's/.*"engine_session_id":"\([^"]*\)".*/\1/p')"
if [ -z "$engine_id" ]; then
  echo "session.start did not come back with an engine session id: $started" >&2
  exit 1
fi
echo "session.list on the Rust core against the real engine"
body="$("${COMPOSE[@]}" exec -T -e VOGT_CORE_TOKEN_FILE=/run/secrets/vogt_core_token engine \
  sh -c 'tok=$(cat "$VOGT_CORE_TOKEN_FILE"); curl -fsS -H "Authorization: Bearer $tok" http://vogt:8000/api/sessions')"
# An unconfigured core returns {"sessions": []} with an engine note. A core
# that reached the real engine returns a row naming the engine's own id.
printf '%s\n' "$body" | grep -q "\"$engine_id\"" || {
  echo "session.list did not name the engine session $engine_id: $body" >&2
  exit 1
}
echo "session.list reached the engine"
echo "rust-core compose smoke passed"
