#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
entry="$root/session-entrypoint.sh"
tmp="$(mktemp -d)"
trap 'find "$tmp" -depth -delete 2>/dev/null || true' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

make_bin() {
  local dir=$1
  mkdir -p "$dir/bin"
  cat >"$dir/bin/curl" <<'SH'
#!/bin/sh
url=""
for arg in "$@"; do url="$arg"; done
case "$url" in
  */account-bundle) [ "${BROKER_MODE:-ok}" != 401 ] || { echo unauthorized >&2; exit 22; }; tar -cf - -C "$FIXTURE" . ;;
  */fetch/TEST_SECRET) printf '%s' "$TEST_SECRET_VALUE" ;;
  *) exit 22 ;;
esac
SH
  cat >"$dir/bin/graft" <<'SH'
#!/bin/sh
[ "${CLONE_FAIL:-0}" = 0 ] || exit 9
while [ "$1" != clone ]; do shift; done
shift; repo=$1; work=$2; shift 2
mkdir -p "$work"; printf '%s\n' "$repo $*" > "$work/clone.args"
SH
  cat >"$dir/bin/vogt-session-agent" <<'SH'
#!/bin/sh
printf 'agent:%s:%s:%s\n' "$VOGT_SESSION_CWD" "$(cat "$VOGT_SESSION_BRIEF_FILE")" "$TEST_SECRET"
SH
  chmod +x "$dir/bin/"*
}
run_case() {
  local dir=$1; shift
  mkdir -p "$dir/fixture" "$dir/secrets"
  printf bundle >"$dir/fixture/.credentials.json"
  printf broker-token >"$dir/secrets/broker"
  printf graft-token >"$dir/secrets/graft"
  make_bin "$dir"
  env PATH="$dir/bin:$PATH" HOME="$dir/home" FIXTURE="$dir/fixture" TEST_SECRET_VALUE='do-not-print-me' \
    VOGT_ENGINE_BROKER_URL=http://broker VOGT_ENGINE_BROKER_TOKEN_FILE="$dir/secrets/broker" \
    VOGT_GRAFT_TOKEN_FILE="$dir/secrets/graft" VOGT_SESSION_REPO=vogt VOGT_SESSION_LINE=rust-2 \
    VOGT_SESSION_WORKDIR="$dir/work" VOGT_SESSION_COMMAND=claude VOGT_SESSION_BRIEF='hello brief' \
    AGENT_AUTH_GRANTED=TEST_SECRET "$@" "$entry"
}

out=$(run_case "$tmp/happy" 2>&1) || fail "happy path"
grep -q 'agent:.*/work:hello brief:do-not-print-me' <<<"$out" || fail "agent did not receive bootstrap state"
[[ $(stat -c %a "$tmp/happy/home/.claude-account/.credentials.json") == 600 ]] || fail "bundle mode"
grep -q 'vogt --line rust-2' "$tmp/happy/work/clone.args" || fail "clone line"

if run_case "$tmp/missing" VOGT_ENGINE_BROKER_TOKEN_FILE= >"$tmp/missing.out" 2>&1; then fail "missing token succeeded"; fi
grep -q 'validate failed.*VOGT_ENGINE_BROKER_TOKEN_FILE' "$tmp/missing.out" || fail "missing token error"
if run_case "$tmp/unauthorized" BROKER_MODE=401 >"$tmp/401.out" 2>&1; then fail "401 succeeded"; fi
grep -q 'account-bundle failed.*refused' "$tmp/401.out" || fail "401 error"
if run_case "$tmp/clone-failure" CLONE_FAIL=1 >"$tmp/clone.out" 2>&1; then fail "clone failure succeeded"; fi
grep -q 'clone failed' "$tmp/clone.out" || fail "clone error"
for output in "$tmp/missing.out" "$tmp/401.out" "$tmp/clone.out"; do
  ! grep -Eq 'broker-token|graft-token|do-not-print-me' "$output" || fail "secret appeared in output"
done
echo 'session-entrypoint: ok'
