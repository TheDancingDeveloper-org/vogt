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
need VOGT_SOURCE_JSON; need VOGT_CLI
case "$VOGT_CLI" in claude|codex|opencode) ;; *) fail 'VOGT_CLI must be claude, codex or opencode' ;; esac
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
jq -n --arg session_id "$VOGT_SESSION_ID" --arg cli "$VOGT_CLI" --argjson source "$VOGT_SOURCE_JSON" --arg extra_ca_file "${VOGT_EXTRA_CA_FILE:-}" \
  '{session_id: $session_id, source: $source, cli: $cli}
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
step=claude-config; say claude-config
# A Claude session gets what a host-launched one does (WI-3243): the engine's
# MCP endpoints as a --mcp-config file, and the conversation and policy hooks
# in the account's settings. The file names the bearer by reference to
# VOGT_HTTP_TOKEN, so it holds no credential. The session agent adds the
# --settings and --mcp-config flags when it starts claude.
if [ "$VOGT_CLI" = claude ]; then
  mcp_file="$(dirname "$token_file")/mcp.json"
  if jq -e '.mcp_config | type == "object"' "$response" >/dev/null 2>&1; then
    jq '.mcp_config' "$response" >"$mcp_file"; chmod 600 "$mcp_file"; export VOGT_CLAUDE_MCP_CONFIG="$mcp_file"
  else
    printf 'session-entrypoint: the bootstrap response carries no mcp_config\n' >&2
  fi
  # Claude Code trusts its own bundle, not the system store; the engine may
  # sit behind a local CA.
  if [ -n "${VOGT_EXTRA_CA_FILE:-}" ] && [ -z "${NODE_EXTRA_CA_CERTS:-}" ]; then
    NODE_EXTRA_CA_CERTS=$VOGT_EXTRA_CA_FILE; export NODE_EXTRA_CA_CERTS
  fi
  # Best effort, as in the merged image's entrypoint: a hook that cannot be
  # installed must not stop the session from starting.
  if command -v vogt-claude-session-hook >/dev/null 2>&1; then
    vogt-claude-session-hook install || printf 'session-entrypoint: could not add the conversation hook; continuing\n' >&2
  fi
  if command -v vogt-policy-hook >/dev/null 2>&1; then
    vogt-policy-hook install-policy || printf 'session-entrypoint: could not add the policy hook; continuing\n' >&2
  fi
  # What of the posture is actually in place, named so the pod smoke can
  # assert it from the pod's log: the policy file the session agent will pass
  # to --settings, the MCP file, and each hook in the account's settings.
  posture=""
  settings_file=${VOGT_CLAUDE_SETTINGS_FILE:-/usr/local/share/vogt/driven-session-settings.json}
  if [ "$settings_file" != off ] && [ -r "$settings_file" ]; then posture="$posture settings"
  elif [ "$settings_file" != off ]; then printf 'session-entrypoint: policy file %s is not readable\n' "$settings_file" >&2; fi
  [ -n "${VOGT_CLAUDE_MCP_CONFIG:-}" ] && jq -e '.mcpServers | type == "object"' "$VOGT_CLAUDE_MCP_CONFIG" >/dev/null 2>&1 && posture="$posture mcp-config"
  hooked() { hook_path=$(command -v "$1" 2>/dev/null) && grep -qF "$hook_path" "$CLAUDE_CONFIG_DIR/settings.json" 2>/dev/null; }
  hooked vogt-policy-hook && posture="$posture policy-hook"
  hooked vogt-claude-session-hook && posture="$posture session-hook"
  printf 'session-entrypoint: claude posture:%s\n' "$posture" >&2
fi
# Codex and opencode read their MCP servers from their own config (WI-3161).
# The bootstrap response carries the file's content; the bearer is named by
# environment variable, so no credential is written. An existing config keeps
# its own settings: the engine's servers replace only the tables it owns.
if [ "$VOGT_CLI" = codex ] && [ -n "$(jq -r '.codex_config // empty' "$response")" ]; then
  codex_home=${CODEX_HOME:-$HOME/.codex}; mkdir -p "$codex_home"; chmod 700 "$codex_home"
  codex_cfg="$codex_home/config.toml"
  # The LLM providers the engine registered for this launch (WI-3491) replace
  # any table of the same name: the merged file must not declare one twice.
  codex_providers=$(jq -r '.codex_config' "$response" | sed -n 's/^\[model_providers\.\([a-z0-9_-]*\)\]$/\1/p' | paste -sd'|' -)
  if [ -f "$codex_cfg" ]; then
    # Drop earlier vogt/cadastre MCP server definitions, keep the rest. A table
    # header is compared with its spaces and quotes removed, so [mcp_servers."vogt"],
    # [ mcp_servers . 'vogt' ] and [mcp_servers.vogt.env] all match, as do
    # single-line dotted keys (mcp_servers.vogt.url = ... at the top level, vogt.url
    # = ... under [mcp_servers]). A multi-line value on such a dotted key is not split.
    awk -v providers="$codex_providers" '
      /^[ \t]*\[\[?[^][]*\]\]?[ \t]*(#.*)?$/ {
        h = $0; sub(/[ \t]*#.*$/, "", h); gsub(/[ \t"\047]/, "", h)
        skip = (h ~ /^\[\[?mcp_servers\.(vogt|cadastre)(\.|\])/)
        if (providers != "" && h ~ ("^\\[model_providers\\.(" providers ")(\\.|\\])")) skip = 1
        root = (h == "[mcp_servers]") ? 2 : 0
        if (skip) next
        print; next
      }
      BEGIN { root = 1 }
      skip { next }
      root && /=/ {
        k = $0; sub(/[ \t]*=.*$/, "", k); gsub(/[ \t"\047]/, "", k)
        if (root == 1 && k ~ /^mcp_servers\.(vogt|cadastre)(\.|$)/) next
        if (root == 2 && k ~ /^(vogt|cadastre)(\.|$)/) next
      }
      { print }
    ' "$codex_cfg" >"$tmp_dir/codex.toml"
  else : >"$tmp_dir/codex.toml"; fi
  jq -r '.codex_config' "$response" >>"$tmp_dir/codex.toml"
  cat "$tmp_dir/codex.toml" >"$codex_cfg"; chmod 600 "$codex_cfg"
  printf 'session-entrypoint: codex posture: mcp-config\n' >&2
fi
if [ "$VOGT_CLI" = opencode ] && jq -e '.opencode_config | type == "object"' "$response" >/dev/null 2>&1; then
  oc_dir=${XDG_CONFIG_HOME:-$HOME/.config}/opencode; mkdir -p "$oc_dir"; chmod 700 "$oc_dir"
  jq '.opencode_config' "$response" >"$oc_dir/opencode.json"; chmod 600 "$oc_dir/opencode.json"
  printf 'session-entrypoint: opencode posture: mcp-config\n' >&2
fi
source_kind=$(jq -r '.source.location.kind' "$response")
source_repo=$(jq -r '.source.location.repo' "$response")
source_ref=$(jq -r '.source.ref' "$response")
VOGT_GRAFT_LINE=$source_ref
step=source-token; say source-token
# The source credential (scoped to this repo and ref) arrives as .source_token
# (.graft_token from an engine one release older) and is kept in a 0600 file;
# without one the session token is tried.
source_token_file=$token_file
if [ -n "$(jq -r '.source_token // .graft_token // empty' "$response")" ]; then
  source_token_file="${VOGT_GRAFT_TOKEN_FILE:-$(dirname "$token_file")/graft-token}"
  jq -j '.source_token // .graft_token' "$response" >"$source_token_file"; chmod 600 "$source_token_file"
  export VOGT_GRAFT_TOKEN_FILE="$source_token_file"
fi
step=clone; say clone
work_dir=${VOGT_WORK_DIR:-/work}
# A bare mkdir under `set -e` would end the script with no reason in the log;
# name the directory so a failed clone step says why (WI-3762).
mkdir -p "$work_dir" || fail "cannot create $work_dir"
if [ "$source_kind" = git ]; then
  if ! git clone --branch "$source_ref" --single-branch "$source_repo" "$work_dir" >/dev/null 2>"$tmp_dir/git.err"; then
    fail "git clone $source_repo into $work_dir failed: $(tail -n3 "$tmp_dir/git.err" | tr '\n' ' ' | cut -c1-400)"
  fi
elif [ "$source_kind" = graft ]; then
if ! graft --token-file "$source_token_file" clone "$source_repo" "$work_dir" --line "$source_ref" >/dev/null 2>"$tmp_dir/graft.err"; then
  # graft's stderr names the endpoint and the refusal, never the token file's content.
  fail "g clone $source_repo --line $source_ref into $work_dir failed (GRAFT_ENDPOINT=${GRAFT_ENDPOINT:-unset}): $(tail -n3 "$tmp_dir/graft.err" | tr '\n' ' ' | cut -c1-400)"
fi
else
  fail "unsupported source kind: $source_kind"
fi
# WI-3356: give /work a read-only git view so `git log -1 -p`, `git diff` and
# `git status` work in the pod (reviewers and most briefs assume git). graft
# has no git export, so the view is built from the clone: a baseline commit of
# the line's tree before its tip changeset, and the tip as the next commit, so
# `git log -1 -p` shows the last landed change. No remote is configured, so
# nothing can be pushed; commits and lands still go through `g`. Best effort: a
# failure leaves a single commit of the cloned tree, or no git view, and never
# stops the boot.
git_view() {
  command -v git >/dev/null 2>&1 || return 0
  [ -e "$work_dir/.git" ] && return 0
  gv() { git -C "$work_dir" -c user.name=graft -c user.email=graft@localhost -c core.hooksPath=/dev/null "$@"; }
  gg() { graft --token-file "$source_token_file" -C "$work_dir" "$@"; }
  gv init -q 2>/dev/null || return 0
  printf '.graft/\n.vogt/\n' >"$work_dir/.git/info/exclude"
  gv add -A -f -- . ':(exclude).graft' ':(exclude).vogt' >/dev/null 2>&1 || return 0
  tree=$(gv write-tree 2>/dev/null) || return 0
  tip_json=$(gg --json log --limit 1 2>/dev/null) || tip_json=
  tip=$(printf '%s' "$tip_json" | jq -r '.changesets[0].id // empty' 2>/dev/null) || tip=
  parent=$(printf '%s' "$tip_json" | jq -r '.next // empty' 2>/dev/null) || parent=
  subject=$(printf '%s' "$tip_json" | jq -r '.changesets[0].subject // empty' 2>/dev/null) || subject=
  head=
  if [ -n "$tip" ] && [ -n "$parent" ] && command -v patch >/dev/null 2>&1 \
     && gg diff "$parent" "$tip" >"$tmp_dir/tip.patch" 2>/dev/null && [ -s "$tmp_dir/tip.patch" ]; then
    # Undo the tip's patch on the files to get the parent's tree, commit it,
    # then commit the real tree on top.
    if (cd "$work_dir" && patch -R -E -p1 -s -f --no-backup-if-mismatch <"$tmp_dir/tip.patch" >/dev/null 2>&1) \
       && gv add -A -f -- . ':(exclude).graft' ':(exclude).vogt' >/dev/null 2>&1 && ptree=$(gv write-tree 2>/dev/null) \
       && base=$(printf 'graft %s: before %s\n' "$VOGT_GRAFT_LINE" "$tip" | gv commit-tree "$ptree" 2>/dev/null) \
       && head=$(printf '%s\n\ngraft changeset %s\n' "$subject" "$tip" | gv commit-tree "$tree" -p "$base" 2>/dev/null); then
      :
    else
      head=
    fi
  fi
  if [ -z "$head" ]; then
    head=$(printf 'graft %s at %s\n' "$VOGT_GRAFT_LINE" "${tip:-tip}" | gv commit-tree "$tree" 2>/dev/null) || return 0
  fi
  # Restore the files to the real tree whichever way the commits were built.
  gv update-ref HEAD "$head" && gv reset -q --hard >/dev/null 2>&1 && gv clean -fdq >/dev/null 2>&1 || true
  printf 'session-entrypoint: git view of %s ready (read-only; land with g)\n' "$work_dir" >&2
}
step=git-view; say git-view
git_view || printf 'session-entrypoint: could not build a git view of %s; continuing\n' "$work_dir" >&2
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
