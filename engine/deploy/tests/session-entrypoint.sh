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
if [ -n "${REFUSE_FIRST:-}" ]; then
  n=$(cat "$CASE_DIR/attempts" 2>/dev/null || echo 0); n=$((n + 1)); echo "$n" >"$CASE_DIR/attempts"
  [ "$n" -gt "$REFUSE_FIRST" ] || { echo "connection refused" >&2; exit 7; }
fi
[ "${BOOT_MODE:-ok}" != 401 ] || { echo unauthorized >&2; exit 22; }
graft=""; [ -z "${GRAFT_TOKEN:-}" ] || graft=$(printf ',"graft_token":"%s"' "$GRAFT_TOKEN")
printf '{"session_token":"%s"%s,"bundle":{"sub/.credentials.json":"%s"},"env":{"VOGT_SESSION_ID":"s1","VOGT_BOOTSTRAP_TOKEN":"%s","EXTRA":"yes"},"brief":"hello brief"}' \
  "$(cat "$CASE_DIR/session")" "$graft" "$(printf bundle | base64)" "$(cat "$CASE_DIR/boot")"
SH
  cat >"$dir/bin/graft" <<'SH'
#!/bin/sh
[ "${CLONE_FAIL:-0}" = 0 ] || exit 9
printf '%s\n' "$*" >"$CASE_DIR/graft.args"
if [ "$1" = --token-file ]; then cp "$2" "$CASE_DIR/graft.token"; fi
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
  # The engine mounts the credential from a Secret; the pod env has the path.
  [ -e "$dir/bootstrap-token" ] || printf %s "$BOOT" >"$dir/bootstrap-token"
  env -i PATH="$dir/bin:/usr/bin:/bin" HOME="$dir/home" TMPDIR="$dir/tmp" CASE_DIR="$dir" \
    VOGT_ENGINE_URL=http://engine VOGT_SESSION_ID=s1 VOGT_BOOTSTRAP_TOKEN_FILE="$dir/bootstrap-token" \
    VOGT_GRAFT_REPO=vogt VOGT_GRAFT_LINE=rust-2 VOGT_CLI=claude \
    VOGT_WORK_DIR="$dir/work" VOGT_SESSION_TOKEN_FILE="$dir/run/session-token" \
    CLAUDE_CONFIG_DIR="$dir/claude" "$@" "$entry"
}

out=$(run_case "$tmp/happy" VOGT_SESSION_COMMAND='["sleep","1"]' GRAFT_TOKEN=graft-secret-value 2>&1) || fail "happy path: $out"
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
! grep -Eq "$BOOT|$SESSION|graft-secret-value" <<<"$out" || fail "secret in output"
for step in bootstrap session-token account-bundle brokered-env graft-token clone brief handoff; do
  grep -q "^session-entrypoint: step $step$" <<<"$out" || fail "step marker $step"
done
grep -Eq '^vogt_session_boot_seconds [0-9]+$' <<<"$out" || fail "boot seconds metric"
# A graft token in the bootstrap response is kept in its own 0600 file beside
# the session token and is what graft clones with.
[[ $(<"$tmp/happy/run/graft-token") == graft-secret-value ]] || fail "graft token content"
[[ $(stat -c %a "$tmp/happy/run/graft-token") == 600 ]] || fail "graft token mode"
[[ $(<"$tmp/happy/graft.token") == graft-secret-value ]] || fail "clone did not use the graft token"
grep -q "^VOGT_GRAFT_TOKEN_FILE=$tmp/happy/run/graft-token$" "$tmp/happy/agent.env" || fail "graft token file not exported"
! grep -v '^GRAFT_TOKEN=' "$tmp/happy/agent.env" | grep -q 'graft-secret-value' || fail "graft token value reached the agent env"
# The requested argv passes through to the session agent untouched.
grep -q '^VOGT_SESSION_COMMAND=\["sleep","1"\]$' "$tmp/happy/agent.env" || fail "VOGT_SESSION_COMMAND not passed through"

# Without a graft token the session token is what graft clones with.
run_case "$tmp/no-graft-token" >/dev/null 2>&1 || fail "no-graft-token path"
[[ $(<"$tmp/no-graft-token/graft.token") == "$SESSION" ]] || fail "clone should fall back to the session token"

# A pod's IP reaches the engine's allow-set a moment after start: refused
# connections (curl exit 7) are retried, a 4xx is final.
run_case "$tmp/retry" REFUSE_FIRST=2 VOGT_BOOTSTRAP_RETRY_MS=5000 >/dev/null 2>&1 || fail "bootstrap retry"
[[ $(<"$tmp/retry/attempts") == 3 ]] || fail "expected 3 bootstrap attempts"
if run_case "$tmp/retry-exhausted" REFUSE_FIRST=99 VOGT_BOOTSTRAP_RETRY_MS=0 >"$tmp/retry-exhausted.out" 2>&1; then fail "exhausted retry succeeded"; fi
grep -q 'bootstrap failed.*curl exit 7' "$tmp/retry-exhausted.out" || fail "exhausted retry error"
[[ $(<"$tmp/retry-exhausted/attempts") == 1 ]] || fail "retry budget 0 should try once"

if run_case "$tmp/missing" VOGT_BOOTSTRAP_TOKEN_FILE= >"$tmp/missing.out" 2>&1; then fail "missing token succeeded"; fi
grep -q 'validate failed.*VOGT_BOOTSTRAP_TOKEN_FILE' "$tmp/missing.out" || fail "missing token error"
# A token file that is absent or empty is refused before any request is made.
if run_case "$tmp/nofile" VOGT_BOOTSTRAP_TOKEN_FILE="$tmp/nofile/none" >"$tmp/nofile.out" 2>&1; then fail "absent token file succeeded"; fi
grep -q 'bootstrap failed.*not readable' "$tmp/nofile.out" || fail "absent token file error"
[ ! -e "$tmp/nofile/auth.cfg" ] || fail "request made without a token"
mkdir -p "$tmp/emptyfile"; : >"$tmp/emptyfile/bootstrap-token"
if run_case "$tmp/emptyfile" >"$tmp/emptyfile.out" 2>&1; then fail "empty token file succeeded"; fi
grep -q 'bootstrap failed.*is empty' "$tmp/emptyfile.out" || fail "empty token file error"
if run_case "$tmp/unauthorized" BOOT_MODE=401 >"$tmp/401.out" 2>&1; then fail "401 succeeded"; fi
grep -q 'bootstrap failed.*refused' "$tmp/401.out" || fail "401 error"
if run_case "$tmp/clone-failure" CLONE_FAIL=1 >"$tmp/clone.out" 2>&1; then fail "clone failure succeeded"; fi
grep -q 'clone failed' "$tmp/clone.out" || fail "clone error"
for output in "$tmp/missing.out" "$tmp/401.out" "$tmp/clone.out"; do
  ! grep -Eq "$BOOT|$SESSION" "$output" || fail "secret appeared in output"
done
echo 'session-entrypoint: ok'
