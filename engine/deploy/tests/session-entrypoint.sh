#!/usr/bin/env bash
# Entrypoint contract test: fake curl (bootstrap JSON), fake graft, fake agent.
# The real-engine counterpart is pod_stream::real_entrypoint_* in
# server/tests/integration.rs.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
entry="$root/session-entrypoint.sh"
tmp="$(mktemp -d)"
trap 'find "$tmp" -depth -delete 2>/dev/null || true' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
BOOT=bootstrap-secret-value
SESSION=session-secret-value

make_bin() {
  local dir=$1
  mkdir -p "$dir/bin"
  cat >"$dir/bin/curl" <<'SH'
#!/bin/sh
# Records the request body and bearer config; answers like the engine would.
body=""; cfg=""
while [ $# -gt 0 ]; do
  case "$1" in
    --data-binary) body=${2#@}; shift ;;
    --config) cfg=$2; shift ;;
  esac
  shift
done
[ -n "$body" ] && cp "$body" "$CASE_DIR/request.json"
[ -n "$cfg" ] && cp "$cfg" "$CASE_DIR/auth.cfg"
[ "${BOOT_MODE:-ok}" != 401 ] || { echo unauthorized >&2; exit 22; }
printf '{"session_token":"%s","bundle":{"sub/.credentials.json":"%s"},"env":{"VOGT_SESSION_ID":"s1","VOGT_BOOTSTRAP_TOKEN":"%s","EXTRA":"yes"},"brief":"hello brief"}' \
  "$(cat "$CASE_DIR/session")" "$(printf bundle | base64)" "$(cat "$CASE_DIR/boot")"
SH
  cat >"$dir/bin/graft" <<'SH'
#!/bin/sh
[ "${CLONE_FAIL:-0}" = 0 ] || exit 9
printf '%s\n' "$*" >"$CASE_DIR/graft.args"
while [ "$1" != clone ]; do shift; done
mkdir -p "$3"
SH
  cat >"$dir/bin/vogt-session-agent" <<'SH'
#!/bin/sh
env >"$CASE_DIR/agent.env"
printf 'agent:%s:%s\n' "$VOGT_SESSION_CWD" "$(cat "$VOGT_SESSION_BRIEF_FILE")"
SH
  chmod +x "$dir/bin/"*
}
run_case() {
  local dir=$1; shift
  mkdir -p "$dir/tmp"
  make_bin "$dir"
  printf %s "$BOOT" >"$dir/boot"; printf %s "$SESSION" >"$dir/session"
  env -i PATH="$dir/bin:/usr/bin:/bin" HOME="$dir/home" TMPDIR="$dir/tmp" CASE_DIR="$dir" \
    VOGT_ENGINE_URL=http://engine VOGT_SESSION_ID=s1 VOGT_BOOTSTRAP_TOKEN="$BOOT" \
    VOGT_GRAFT_REPO=vogt VOGT_GRAFT_LINE=rust-2 VOGT_CLI=claude \
    VOGT_WORK_DIR="$dir/work" VOGT_SESSION_TOKEN_FILE="$dir/run/session-token" \
    CLAUDE_CONFIG_DIR="$dir/claude" "$@" "$entry"
}

out=$(run_case "$tmp/happy" 2>&1) || fail "happy path: $out"
grep -q "agent:$tmp/happy/work:hello brief" <<<"$out" || fail "agent did not receive bootstrap state"
jq -e '.session_id=="s1" and .graft_repo=="vogt" and .graft_line=="rust-2" and .cli=="claude"' \
  "$tmp/happy/request.json" >/dev/null || fail "bootstrap request body"
[[ $(stat -c %a "$tmp/happy/run/session-token") == 600 ]] || fail "token mode"
[[ $(<"$tmp/happy/run/session-token") == "$SESSION" ]] || fail "token content"
[[ $(stat -c %a "$tmp/happy/claude/sub/.credentials.json") == 600 ]] || fail "bundle mode"
grep -q "clone vogt $tmp/happy/work --line rust-2" "$tmp/happy/graft.args" || fail "clone args"
! grep -q "$BOOT" "$tmp/happy/agent.env" || fail "bootstrap token reached the agent"
! grep -q "VOGT_BOOTSTRAP_TOKEN" "$tmp/happy/agent.env" || fail "bootstrap name re-exported"
grep -q '^EXTRA=yes$' "$tmp/happy/agent.env" || fail "brokered env not exported"
! grep -Eq "$BOOT|$SESSION" <<<"$out" || fail "secret in output"

if run_case "$tmp/missing" VOGT_BOOTSTRAP_TOKEN= >"$tmp/missing.out" 2>&1; then fail "missing token succeeded"; fi
grep -q 'validate failed.*VOGT_BOOTSTRAP_TOKEN' "$tmp/missing.out" || fail "missing token error"
if run_case "$tmp/unauthorized" BOOT_MODE=401 >"$tmp/401.out" 2>&1; then fail "401 succeeded"; fi
grep -q 'bootstrap failed.*refused' "$tmp/401.out" || fail "401 error"
if run_case "$tmp/clone-failure" CLONE_FAIL=1 >"$tmp/clone.out" 2>&1; then fail "clone failure succeeded"; fi
grep -q 'clone failed' "$tmp/clone.out" || fail "clone error"
for output in "$tmp/missing.out" "$tmp/401.out" "$tmp/clone.out"; do
  ! grep -Eq "$BOOT|$SESSION" "$output" || fail "secret appeared in output"
done
echo 'session-entrypoint: ok'
