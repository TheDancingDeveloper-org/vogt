#!/usr/bin/env bash
# WI-1331: vogt-git-askpass asks the broker (via a fake vogt-agent-auth on PATH)
# only when no token is in the environment, only at a Password prompt, and
# stays silent (exit 1) on a refusal.
set -euo pipefail

askpass="$(cd "$(dirname "$0")/.." && pwd)/git-askpass.sh"
testdir="$(mktemp -d)"
trap 'rm -rf "$testdir"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

mkdir "$testdir/bin"
# The fake helper records its host in a file (never the token) and answers
# only for github.com.
cat >"$testdir/bin/vogt-agent-auth" <<'FAKE'
#!/bin/sh
[ "$1" = git-credential ] || exit 64
printf '%s\n' "$2" >>"$CALLS"
[ "$2" = github.com ] || exit 1
printf 'x-access-token\nbrokered-value\n'
FAKE
chmod +x "$testdir/bin/vogt-agent-auth"
export CALLS="$testdir/calls"
: >"$CALLS"
export PATH="$testdir/bin:$PATH"
unset GH_TOKEN GIT_AUTH_TOKEN ENGINE_WORKTREE_FETCH_TOKEN VOGT_ENGINE_BROKER_URL

pw="Password for 'https://x-access-token@github.com': "

# 1. GH_TOKEN set: unchanged, the broker is not asked.
export VOGT_ENGINE_BROKER_URL=http://127.0.0.1:1
out="$(GH_TOKEN=env-value "$askpass" "$pw")"
[[ "$out" == env-value ]] || fail "GH_TOKEN case printed '$out'"
[[ ! -s "$CALLS" ]] || fail "broker asked although GH_TOKEN was set"

# 2. no token, broker configured: the broker's token is printed.
out="$("$askpass" "$pw")"
[[ "$out" == brokered-value ]] || fail "broker case printed '$out'"

# 3. a Username prompt never asks the broker (a once grant is spent once).
: >"$CALLS"
out="$("$askpass" "Username for 'https://github.com': ")"
[[ "$out" == x-access-token ]] || fail "username printed '$out'"
[[ ! -s "$CALLS" ]] || fail "broker asked at a Username prompt"

# 4. no grant (helper refuses): silent exit 1.
rc=0
out="$("$askpass" "Password for 'https://forge.example.com': " 2>&1)" || rc=$?
[[ "$rc" == 1 && -z "$out" ]] || fail "refusal gave rc=$rc out='$out'"
grep -qx forge.example.com "$CALLS" || fail "host not passed to the helper"

# 5. no broker URL: today's behaviour, exit 1 without asking.
: >"$CALLS"
rc=0
(unset VOGT_ENGINE_BROKER_URL; "$askpass" "$pw" >/dev/null 2>&1) || rc=$?
[[ "$rc" == 1 ]] || fail "no-broker case rc=$rc"
[[ ! -s "$CALLS" ]] || fail "broker asked with no VOGT_ENGINE_BROKER_URL"

echo "git-askpass: ok"
