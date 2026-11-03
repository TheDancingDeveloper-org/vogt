#!/bin/sh
# Bootstrap a remote session pod. Secrets are read from mounted files or the
# authenticated engine broker; no credential is ever passed as an argument.
set -eu

step=validate
started_at=$(date +%s)
tmp_dir="${TMPDIR:-/tmp}/vogt-session-entrypoint.$$"
mkdir -m 700 "$tmp_dir"
cleanup() { find "$tmp_dir" -type f -delete 2>/dev/null || true; rmdir "$tmp_dir" 2>/dev/null || true; }
trap cleanup EXIT HUP INT TERM

fail() {
  printf 'session-entrypoint: %s failed: %s\n' "$step" "$1" >&2
  exit 1
}
need() {
  eval "value=\${$1:-}"
  [ -n "$value" ] || fail "required environment variable $1 is not set"
}

need VOGT_ENGINE_BROKER_URL
need VOGT_ENGINE_BROKER_TOKEN_FILE
need VOGT_SESSION_REPO
need VOGT_SESSION_LINE
need VOGT_SESSION_WORKDIR
need VOGT_GRAFT_TOKEN_FILE
need VOGT_SESSION_COMMAND
[ -r "$VOGT_ENGINE_BROKER_TOKEN_FILE" ] || fail "broker token file is not readable"
[ -r "$VOGT_GRAFT_TOKEN_FILE" ] || fail "graft token file is not readable"

# curl reads the bearer header from a protected config file. The value is never
# expanded into curl's argv (and curl errors cannot reflect it).
auth_config="$tmp_dir/curl-auth"
umask 077
{
  printf 'header = "Authorization: Bearer '
  tr -d '\r\n' < "$VOGT_ENGINE_BROKER_TOKEN_FILE"
  printf '"\n'
} > "$auth_config"

broker_get() {
  curl --silent --show-error --fail-with-body --config "$auth_config" "$1" 2>"$tmp_dir/curl.err" \
    || { status=$?; printf 'broker request was refused or unavailable (curl %s)' "$status" >"$tmp_dir/failure"; return "$status"; }
}

step=account-bundle
CLAUDE_CONFIG_DIR=${CLAUDE_CONFIG_DIR:-$HOME/.claude-account}
export CLAUDE_CONFIG_DIR
mkdir -p "$CLAUDE_CONFIG_DIR"
chmod 700 "$CLAUDE_CONFIG_DIR"
bundle_url=${VOGT_ACCOUNT_BUNDLE_URL:-${VOGT_ENGINE_BROKER_URL%/}/api/agent-auth/account-bundle}
if ! broker_get "$bundle_url" > "$tmp_dir/account-bundle.tar"; then
  fail "$(cat "$tmp_dir/failure")"
fi
# The endpoint returns an uncompressed tar. Reject links and absolute/parent
# paths before extracting, then make every account file private.
tar -tf "$tmp_dir/account-bundle.tar" > "$tmp_dir/bundle.list" 2>/dev/null \
  || fail "broker returned an invalid account bundle"
while IFS= read -r member; do
  case "$member" in /*|../*|*/../*|*/..) fail "broker returned an unsafe account bundle" ;; esac
done < "$tmp_dir/bundle.list"
if tar -tvf "$tmp_dir/account-bundle.tar" | grep -Eq '^[lh]'; then
  fail "broker returned a linked account bundle"
fi
tar -xf "$tmp_dir/account-bundle.tar" -C "$CLAUDE_CONFIG_DIR"
find "$CLAUDE_CONFIG_DIR" -type d -exec chmod 700 {} +
find "$CLAUDE_CONFIG_DIR" -type f -exec chmod 600 {} +

step=brokered-env
# AGENT_AUTH_GRANTED contains names only. Fetch each value independently so
# the engine applies its manifest and audit policy. Variable names are strictly
# shell identifiers; values are exported without evaluation.
for name in ${AGENT_AUTH_GRANTED:-}; do
  case "$name" in ''|[!A-Za-z_]*|*[!A-Za-z0-9_]*) fail "AGENT_AUTH_GRANTED contains an invalid variable name" ;; esac
  value_file="$tmp_dir/env.$name"
  if ! broker_get "${VOGT_ENGINE_BROKER_URL%/}/api/agent-auth/fetch/$name" > "$value_file"; then
    fail "could not fetch brokered variable $name: $(cat "$tmp_dir/failure")"
  fi
  value=$(cat "$value_file")
  export "$name=$value"
done

step=clone
mkdir -p "$(dirname "$VOGT_SESSION_WORKDIR")"
if [ -e "$VOGT_SESSION_WORKDIR" ]; then
  fail "workspace already exists"
fi
if ! graft --token-file "$VOGT_GRAFT_TOKEN_FILE" clone "$VOGT_SESSION_REPO" "$VOGT_SESSION_WORKDIR" --line "$VOGT_SESSION_LINE"; then
  fail "g clone exited non-zero"
fi

step=brief
brief_dir=${VOGT_SESSION_ADD_DIR:-${VOGT_SESSION_BRIEF_DIR:-$VOGT_SESSION_WORKDIR/.vogt}}
brief_file=${VOGT_SESSION_BRIEF_FILE:-$brief_dir/brief.md}
guide_file=${VOGT_SESSION_GUIDE_FILE:-$brief_dir/brief.guide.md}
mkdir -p "$brief_dir"
chmod 700 "$brief_dir"
if [ -n "${VOGT_SESSION_BRIEF_SOURCE:-}" ]; then
  [ -r "$VOGT_SESSION_BRIEF_SOURCE" ] || fail "brief source is not readable"
  cp "$VOGT_SESSION_BRIEF_SOURCE" "$brief_file"
else
  printf '%s' "${VOGT_SESSION_BRIEF:-}" > "$brief_file"
fi
if [ -n "${VOGT_SESSION_GUIDE_SOURCE:-}" ]; then
  [ -r "$VOGT_SESSION_GUIDE_SOURCE" ] || fail "guide source is not readable"
  cp "$VOGT_SESSION_GUIDE_SOURCE" "$guide_file"
elif [ -n "${VOGT_SESSION_GUIDE:-}" ]; then
  printf '%s' "$VOGT_SESSION_GUIDE" > "$guide_file"
fi
chmod 600 "$brief_file"
[ ! -e "$guide_file" ] || chmod 600 "$guide_file"
export VOGT_SESSION_CWD="$VOGT_SESSION_WORKDIR"
export VOGT_SESSION_BRIEF_FILE="$brief_file"

step=handoff
elapsed=$(( $(date +%s) - started_at ))
printf 'session-entrypoint: boot completed in %ss\n' "$elapsed" >&2
# A collector can scrape this stable metric line from pod logs. Do not make
# telemetry a boot dependency.
printf 'vogt_session_boot_seconds %s\n' "$elapsed" >&2
trap - EXIT HUP INT TERM
cleanup
exec vogt-session-agent
