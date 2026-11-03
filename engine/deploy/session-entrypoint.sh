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
need() { eval "value=\${$1:-}"; [ -n "$value" ] || fail "required environment variable $1 is not set"; }
need VOGT_ENGINE_URL; need VOGT_SESSION_ID; need VOGT_BOOTSTRAP_TOKEN
need VOGT_GRAFT_REPO; need VOGT_GRAFT_LINE; need VOGT_CLI
case "$VOGT_CLI" in claude|codex) ;; *) fail 'VOGT_CLI must be claude or codex' ;; esac
step=bootstrap
auth="$tmp_dir/auth"; response="$tmp_dir/response"; request="$tmp_dir/request.json"
umask 077
printf 'header = "Authorization: Bearer %s"\n' "$VOGT_BOOTSTRAP_TOKEN" >"$auth"
# shellcheck disable=SC2153 # VOGT_SESSION_ID is validated by need() above
jq -n --arg session_id "$VOGT_SESSION_ID" --arg graft_repo "$VOGT_GRAFT_REPO" \
  --arg graft_line "$VOGT_GRAFT_LINE" --arg cli "$VOGT_CLI" --arg extra_ca_file "${VOGT_EXTRA_CA_FILE:-}" \
  '{session_id: $session_id, graft_repo: $graft_repo, graft_line: $graft_line, cli: $cli}
   + (if $extra_ca_file == "" then {} else {extra_ca_file: $extra_ca_file} end)' >"$request"
unset VOGT_BOOTSTRAP_TOKEN
url="${VOGT_ENGINE_URL%/}/api/agent-auth/bootstrap"
if ! curl --silent --show-error --fail-with-body --config "$auth" -X POST -H "Content-Type: application/json" --data-binary "@$request" "$url" >"$response" 2>/dev/null; then fail 'bootstrap request was refused or unavailable'; fi
jq -e '(.session_token|type)=="string" and (.session_token|length)>0' "$response" >/dev/null 2>&1 || fail 'bootstrap response is invalid'
step=session-token
token_file="${VOGT_SESSION_TOKEN_FILE:-/run/vogt/session-token}"
mkdir -p "$(dirname "$token_file")"
jq -r '.session_token' "$response" >"$token_file"; chmod 600 "$token_file"; export VOGT_SESSION_TOKEN_FILE="$token_file"
step=account-bundle
CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR:-$HOME/.claude-account}; export CLAUDE_CONFIG_DIR
mkdir -p "$CLAUDE_CONFIG_DIR"; chmod 700 "$CLAUDE_CONFIG_DIR"
jq -r '.bundle // {} | to_entries[] | [.key, .value] | @tsv' "$response" >"$tmp_dir/bundle.tsv"
while IFS="$(printf '\t')" read -r path encoded; do
  case "$path" in ''|/*|../*|*/../*|*/..) fail 'bootstrap returned an unsafe bundle path' ;; esac
  dest="$CLAUDE_CONFIG_DIR/$path"; mkdir -p "$(dirname "$dest")"; chmod 700 "$(dirname "$dest")"
  printf '%s' "$encoded" | base64 -d >"$dest" || fail 'bootstrap returned invalid bundle data'; chmod 600 "$dest"
done <"$tmp_dir/bundle.tsv"
step=brokered-env
jq -r '.env // {} | to_entries[] | [.key, (.value|tostring)] | @tsv' "$response" >"$tmp_dir/env.tsv"
while IFS="$(printf '\t')" read -r name value; do
  case "$name" in ''|[!A-Za-z_]*|*[!A-Za-z0-9_]*) fail 'bootstrap returned an invalid environment name' ;; esac
  case "$name" in VOGT_BOOTSTRAP_TOKEN|VOGT_AGENT_BOOTSTRAP_TOKEN) continue ;; esac
  export "$name=$value"
done <"$tmp_dir/env.tsv"
step=clone
work_dir=${VOGT_WORK_DIR:-/work}
mkdir -p "$work_dir"
if ! graft --token-file "$token_file" clone "$VOGT_GRAFT_REPO" "$work_dir" --line "$VOGT_GRAFT_LINE" >/dev/null 2>"$tmp_dir/graft.err"; then fail 'g clone failed'; fi
step=brief
add_dir=${VOGT_ADD_DIR:-$work_dir/.vogt}; mkdir -p "$add_dir"; chmod 700 "$add_dir"
jq -r '.brief // ""' "$response" >"$add_dir/brief.md"; chmod 600 "$add_dir/brief.md"
export VOGT_ADD_DIR="$add_dir"
export VOGT_SESSION_CWD="$work_dir"; export VOGT_SESSION_BRIEF_FILE="$add_dir/brief.md"
step=handoff
elapsed=$(( $(date +%s) - started_at )); printf 'session-entrypoint: boot completed in %ss\n' "$elapsed" >&2; printf 'vogt_session_boot_seconds %s\n' "$elapsed" >&2
trap - EXIT HUP INT TERM; cleanup; exec vogt-session-agent
