#!/bin/sh
# Exchange the single-use pod bootstrap credential, then start the CLI agent.
set -eu
step=validate
started_at=$(date +%s)
tmp_dir="${TMPDIR:-/tmp}/vogt-session-entrypoint.$$"
mkdir -m 700 "$tmp_dir"
cleanup() { find "$tmp_dir" -type f ! -name session-token -delete 2>/dev/null || true; rmdir "$tmp_dir" 2>/dev/null || true; }
trap cleanup EXIT HUP INT TERM
fail() { printf 'session-entrypoint: %s failed: %s\n' "$step" "$1" >&2; exit 1; }
say() { printf 'session-entrypoint: step %s\n' "$1" >&2; }
need() { eval "value=\${$1:-}"; [ -n "$value" ] || fail "required environment variable $1 is not set"; }
need VOGT_ENGINE_URL; need VOGT_SESSION_ID; need VOGT_BOOTSTRAP_TOKEN_FILE
need VOGT_GRAFT_REPO; need VOGT_GRAFT_LINE; need VOGT_CLI
case "$VOGT_CLI" in claude|codex) ;; *) fail 'VOGT_CLI must be claude or codex' ;; esac
# /cache is a per-pod emptyDir (WI-3242), so the directories the image creates
# there are shadowed by it. Recreate the tool caches best-effort; each tool
# would create its own anyway.
for dir in "${XDG_CACHE_HOME:-}" "${NPM_CONFIG_CACHE:-}" "${PNPM_HOME:-}" "${PIP_CACHE_DIR:-}" "${UV_CACHE_DIR:-}" "${CARGO_HOME:-}"; do
  case "$dir" in /cache/*) mkdir -p "$dir" 2>/dev/null || true ;; esac
done
step=bootstrap; say bootstrap
auth="$tmp_dir/auth"; response="$tmp_dir/response"; request="$tmp_dir/request.json"
umask 077
# The credential is a file the engine mounts from a per-session Secret, never
# an environment value; the path is not a secret.
[ -r "$VOGT_BOOTSTRAP_TOKEN_FILE" ] || fail "bootstrap token file $VOGT_BOOTSTRAP_TOKEN_FILE is not readable"
bootstrap_token=$(cat "$VOGT_BOOTSTRAP_TOKEN_FILE")
[ -n "$bootstrap_token" ] || fail "bootstrap token file $VOGT_BOOTSTRAP_TOKEN_FILE is empty"
printf 'header = "Authorization: Bearer %s"\n' "$bootstrap_token" >"$auth"
# shellcheck disable=SC2153 # VOGT_SESSION_ID is validated by need() above
jq -n --arg session_id "$VOGT_SESSION_ID" --arg graft_repo "$VOGT_GRAFT_REPO" \
  --arg graft_line "$VOGT_GRAFT_LINE" --arg cli "$VOGT_CLI" --arg extra_ca_file "${VOGT_EXTRA_CA_FILE:-}" \
  '{session_id: $session_id, graft_repo: $graft_repo, graft_line: $graft_line, cli: $cli}
   + (if $extra_ca_file == "" then {} else {extra_ca_file: $extra_ca_file} end)' >"$request"
unset bootstrap_token VOGT_BOOTSTRAP_TOKEN_FILE
url="${VOGT_ENGINE_URL%/}/api/agent-auth/bootstrap"
# A new pod's IP reaches the engine's allow-set 1-3 s after start; until then
# the connection is refused (curl exit 7) or times out (28). Retry those with
# backoff (250 ms doubling, 2 s cap, 15 s total); an HTTP error (4xx) is final.
backoff_ms=250; waited_ms=0; max_wait_ms=${VOGT_BOOTSTRAP_RETRY_MS:-15000}
while :; do
  rc=0
  curl --silent --show-error --fail-with-body --connect-timeout 5 --max-time 20 --config "$auth" -X POST -H "Content-Type: application/json" --data-binary "@$request" "$url" >"$response" 2>"$tmp_dir/curl.err" || rc=$?
  [ "$rc" -eq 0 ] && break
  case "$rc" in 6|7|28|35|52|55|56) [ "$waited_ms" -lt "$max_wait_ms" ] || break ;; *) break ;; esac
  sleep "$(awk -v ms="$backoff_ms" 'BEGIN { printf "%.3f", ms / 1000 }')"
  waited_ms=$((waited_ms + backoff_ms)); backoff_ms=$((backoff_ms * 2)); [ "$backoff_ms" -le 2000 ] || backoff_ms=2000
done
if [ "$rc" -ne 0 ]; then
  # curl's error names the URL and the failure, never the bearer (that is in
  # the config file); the body is the engine's one-line reason.
  fail "bootstrap request was refused or unavailable (curl exit $rc, waited ${waited_ms} ms): $(tail -n1 "$tmp_dir/curl.err" | cut -c1-200) $(head -c 200 "$response" | tr '\n' ' ')"
fi
jq -e '(.session_token|type)=="string" and (.session_token|length)>0' "$response" >/dev/null 2>&1 || fail 'bootstrap response is invalid'
step=session-token; say session-token
token_file="${VOGT_SESSION_TOKEN_FILE:-/run/vogt/session-token}"
mkdir -p "$(dirname "$token_file")"
jq -r '.session_token' "$response" >"$token_file"; chmod 600 "$token_file"; export VOGT_SESSION_TOKEN_FILE="$token_file"
step=account-bundle; say account-bundle
CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR:-$HOME/.claude-account}; export CLAUDE_CONFIG_DIR
mkdir -p "$CLAUDE_CONFIG_DIR"; chmod 700 "$CLAUDE_CONFIG_DIR"
jq -r '.bundle // {} | to_entries[] | [.key, .value] | @tsv' "$response" >"$tmp_dir/bundle.tsv"
while IFS="$(printf '\t')" read -r path encoded; do
  case "$path" in ''|/*|../*|*/../*|*/..) fail 'bootstrap returned an unsafe bundle path' ;; esac
  dest="$CLAUDE_CONFIG_DIR/$path"; mkdir -p "$(dirname "$dest")"; chmod 700 "$(dirname "$dest")"
  printf '%s' "$encoded" | base64 -d >"$dest" || fail 'bootstrap returned invalid bundle data'; chmod 600 "$dest"
done <"$tmp_dir/bundle.tsv"
step=brokered-env; say brokered-env
jq -r '.env // {} | to_entries[] | [.key, (.value|tostring)] | @tsv' "$response" >"$tmp_dir/env.tsv"
while IFS="$(printf '\t')" read -r name value; do
  case "$name" in ''|[!A-Za-z_]*|*[!A-Za-z0-9_]*) fail 'bootstrap returned an invalid environment name' ;; esac
  case "$name" in VOGT_BOOTSTRAP_TOKEN|VOGT_BOOTSTRAP_TOKEN_FILE|VOGT_AGENT_BOOTSTRAP_TOKEN) continue ;; esac
  export "$name=$value"
done <"$tmp_dir/env.tsv"
step=graft-token; say graft-token
# The graft credential (scoped to this repo and line) arrives as .graft_token
# and is kept in a 0600 file; without one the session token is tried.
graft_token_file=$token_file
if [ -n "$(jq -r '.graft_token // empty' "$response")" ]; then
  graft_token_file="${VOGT_GRAFT_TOKEN_FILE:-$(dirname "$token_file")/graft-token}"
  jq -j '.graft_token' "$response" >"$graft_token_file"; chmod 600 "$graft_token_file"
  export VOGT_GRAFT_TOKEN_FILE="$graft_token_file"
fi
step=clone; say clone
work_dir=${VOGT_WORK_DIR:-/work}
mkdir -p "$work_dir"
if ! graft --token-file "$graft_token_file" clone "$VOGT_GRAFT_REPO" "$work_dir" --line "$VOGT_GRAFT_LINE" >/dev/null 2>"$tmp_dir/graft.err"; then
  # graft's stderr names the endpoint and the refusal, never the token file's content.
  fail "g clone $VOGT_GRAFT_REPO --line $VOGT_GRAFT_LINE into $work_dir failed (GRAFT_ENDPOINT=${GRAFT_ENDPOINT:-unset}): $(tail -n3 "$tmp_dir/graft.err" | tr '\n' ' ' | cut -c1-400)"
fi
step=brief; say brief
add_dir=${VOGT_ADD_DIR:-$work_dir/.vogt}; mkdir -p "$add_dir"; chmod 700 "$add_dir"
jq -r '.brief // ""' "$response" >"$add_dir/brief.md"; chmod 600 "$add_dir/brief.md"
export VOGT_ADD_DIR="$add_dir"
export VOGT_SESSION_CWD="$work_dir"; export VOGT_SESSION_BRIEF_FILE="$add_dir/brief.md"
step=handoff; say handoff
elapsed=$(( $(date +%s) - started_at )); printf 'session-entrypoint: boot completed in %ss\n' "$elapsed" >&2; printf 'vogt_session_boot_seconds %s\n' "$elapsed" >&2
trap - EXIT HUP INT TERM; cleanup
command -v vogt-session-agent >/dev/null 2>&1 || { printf 'session-entrypoint: handoff failed: vogt-session-agent not found\n' >&2; exit 1; }
exec vogt-session-agent
