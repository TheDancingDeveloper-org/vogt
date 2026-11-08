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
graft=""; [ -z "${GRAFT_TOKEN:-}" ] || graft=$(printf ',"source_token":"%s"' "$GRAFT_TOKEN")
[ -z "${MCP_CONFIG:-}" ] || graft="$graft,\"mcp_config\":$MCP_CONFIG"
[ -z "${CODEX_CONFIG:-}" ] || graft="$graft,\"codex_config\":$(printf %s "$CODEX_CONFIG" | jq -Rs .)"
[ -z "${OPENCODE_CONFIG:-}" ] || graft="$graft,\"opencode_config\":$OPENCODE_CONFIG"
printf '{"session_token":"%s"%s,"source":%s,"bundle":{"sub/.credentials.json":"%s"},"env":{"VOGT_SESSION_ID":"s1","VOGT_BOOTSTRAP_TOKEN":"%s","EXTRA":"yes"},"brief":"hello brief"}' \
  "$(cat "$CASE_DIR/session")" "$graft" "$SOURCE_JSON" "$(printf bundle | base64)" "$(cat "$CASE_DIR/boot")"
SH
  cat >"$dir/bin/graft" <<'SH'
#!/bin/sh
[ "${CLONE_FAIL:-0}" = 0 ] || exit 9
case " $* " in *" clone "*)
  printf '%s\n' "$*" >"$CASE_DIR/graft.args"
  if [ "$1" = --token-file ]; then cp "$2" "$CASE_DIR/graft.token"; fi ;;
esac
dest="$CASE_DIR/work"
after_clone=0
while [ $# -gt 0 ]; do
  if [ "$after_clone" -gt 0 ]; then
    after_clone=$((after_clone + 1))
    if [ "$after_clone" = 3 ]; then dest=$1; break; fi
  fi
  case "$1" in
    clone) after_clone=1 ;;
    log) printf '%s\n' '{"changesets":[{"id":"tip1","subject":"tip change (WI-1)"}],"next":"par1"}'; exit 0 ;;
    diff)
      if [ "${DIFF_IGNORED:-0}" = 1 ]; then
        printf 'put file tracked.txt\n--- a/tracked.txt\n+++ b/tracked.txt\n@@ -1 +1 @@\n-old-ignored\n+new-ignored\n'
      else
        printf 'put file a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n'
      fi
      exit 0 ;;
  esac
  shift
done
mkdir -p "$dest"
printf 'new\n' >"$dest/a.txt"
printf 'tracked.txt\n' >"$dest/.gitignore"
printf 'new-ignored\n' >"$dest/tracked.txt"
mkdir -p "$dest/.graft" "$dest/.vogt"
printf 'graft state\n' >"$dest/.graft/state"
printf 'vogt state\n' >"$dest/.vogt/state"
SH
  cat >"$dir/bin/vogt-session-agent" <<'SH'
#!/bin/sh
env >"$CASE_DIR/agent.env"
printf 'agent:%s:%s\n' "$VOGT_SESSION_CWD" "$(cat "$VOGT_SESSION_BRIEF_FILE")"
SH
  # The two hook scripts the image installs: record how the entrypoint ran them.
  for hook in vogt-claude-session-hook vogt-policy-hook; do
    # shellcheck disable=SC2016 # the stub's own $-expressions stay literal
    printf '#!/bin/sh\nprintf "%%s\\n" "$*" >>"$CASE_DIR/%s.calls"\nprintf "%%s\\n" "$0" >>"$CLAUDE_CONFIG_DIR/settings.json"\n' "$hook" >"$dir/bin/$hook"
  done
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
    SOURCE_JSON='{"location":{"kind":"graft","endpoint":"http://graft","repo":"vogt"},"ref":"rust-2","landing":"line"}' \
    VOGT_SOURCE_JSON='{"location":{"kind":"graft","endpoint":"http://graft","repo":"vogt"},"ref":"rust-2","landing":"line"}' VOGT_CLI=claude \
    VOGT_WORK_DIR="$dir/work" VOGT_SESSION_TOKEN_FILE="$dir/run/session-token" \
    CLAUDE_CONFIG_DIR="$dir/claude" "$@" "$entry"
}

# shellcheck disable=SC2016 # ${VOGT_HTTP_TOKEN} is Claude Code's to expand
MCP='{"mcpServers":{"vogt":{"type":"http","url":"https://e/mcp","headers":{"Authorization":"Bearer ${VOGT_HTTP_TOKEN}"}}}}'
out=$(run_case "$tmp/happy" VOGT_SESSION_COMMAND='["sleep","1"]' GRAFT_TOKEN=graft-secret-value MCP_CONFIG="$MCP" VOGT_EXTRA_CA_FILE=/etc/ca.pem VOGT_CLAUDE_SETTINGS_FILE=off 2>&1) || fail "happy path: $out"
grep -q "agent:$tmp/happy/work:hello brief" <<<"$out" || fail "agent did not receive bootstrap state"
jq -e '.session_id=="s1" and .source.location.kind=="graft" and .source.location.repo=="vogt" and .source.ref=="rust-2" and .cli=="claude"' \
  "$tmp/happy/request.json" >/dev/null || fail "bootstrap request body"
[[ $(stat -c %a "$tmp/happy/run/session-token") == 600 ]] || fail "token mode"
[[ $(<"$tmp/happy/run/session-token") == "$SESSION" ]] || fail "token content"
[[ $(stat -c %a "$tmp/happy/claude/sub/.credentials.json") == 600 ]] || fail "bundle mode"
grep -q "clone vogt $tmp/happy/work --line rust-2" "$tmp/happy/graft.args" || fail "clone args"
# WI-3356: /work is a git repository whose last commit is the line's tip
# changeset, with no remote, so git log -1 -p, git diff and git status work.
w="$tmp/happy/work"
[[ $(git -C "$w" log -1 --format=%s) == "tip change (WI-1)" ]] || fail "git view: tip subject"
git -C "$w" log -1 -p | grep -q '^+new$' || fail "git view: tip patch"
git -C "$w" log -1 -p | grep -q '^-old$' || fail "git view: tip patch removal"
[[ $(git -C "$w" rev-list --count HEAD) == 2 ]] || fail "git view: expected baseline plus tip"
[[ -z $(git -C "$w" status --porcelain) ]] || fail "git view: tree not clean"
[[ -z $(git -C "$w" ls-files .graft .vogt) ]] || fail "git view: runtime state tracked"
[[ -z $(git -C "$w" remote) ]] || fail "git view: a remote would allow a push"
[[ $(<"$w/a.txt") == new ]] || fail "git view: files changed"
grep -q '^session-entrypoint: step git-view$' <<<"$out" || fail "git-view step marker"
! grep -q "$BOOT" "$tmp/happy/agent.env" || fail "bootstrap token reached the agent"
! grep -q "VOGT_BOOTSTRAP_TOKEN" "$tmp/happy/agent.env" || fail "bootstrap name re-exported"
grep -q '^EXTRA=yes$' "$tmp/happy/agent.env" || fail "brokered env not exported"
! grep -Eq "$BOOT|$SESSION|graft-secret-value" <<<"$out" || fail "secret in output"
for step in bootstrap session-token account-bundle brokered-env claude-config source-token clone brief handoff; do
  grep -q "^session-entrypoint: step $step$" <<<"$out" || fail "step marker $step"
done
grep -Eq '^vogt_session_boot_seconds [0-9]+$' <<<"$out" || fail "boot seconds metric"
# A graft token in the bootstrap response is kept in its own 0600 file beside
# the session token and is what graft clones with.
[[ $(<"$tmp/happy/run/graft-token") == graft-secret-value ]] || fail "graft token content"
[[ $(stat -c %a "$tmp/happy/run/graft-token") == 600 ]] || fail "graft token mode"
[[ $(<"$tmp/happy/graft.token") == graft-secret-value ]] || fail "clone did not use the graft token"
grep -q 'clone vogt '"$tmp/happy/work"' --line rust-2' "$tmp/happy/graft.args" || fail "default project clone scope"
grep -q "^VOGT_GRAFT_TOKEN_FILE=$tmp/happy/run/graft-token$" "$tmp/happy/agent.env" || fail "graft token file not exported"
! grep -v '^GRAFT_TOKEN=' "$tmp/happy/agent.env" | grep -q 'graft-secret-value' || fail "graft token value reached the agent env"
# A Claude session's MCP config lands in its own 0600 file beside the session
# token and is named to the session agent; both hooks are installed once.
[[ $(stat -c %a "$tmp/happy/run/mcp.json") == 600 ]] || fail "mcp config mode"
jq -e '.mcpServers.vogt.headers.Authorization=="Bearer ${VOGT_HTTP_TOKEN}"' "$tmp/happy/run/mcp.json" >/dev/null || fail "mcp config content"
grep -q "^VOGT_CLAUDE_MCP_CONFIG=$tmp/happy/run/mcp.json$" "$tmp/happy/agent.env" || fail "mcp config path not exported"
grep -q '^NODE_EXTRA_CA_CERTS=/etc/ca.pem$' "$tmp/happy/agent.env" || fail "extra CA not given to Claude Code"
[[ $(<"$tmp/happy/vogt-claude-session-hook.calls") == install ]] || fail "conversation hook not installed"
[[ $(<"$tmp/happy/vogt-policy-hook.calls") == install-policy ]] || fail "policy hook not installed"
# The boot log says which parts of the posture are in place; the pod smoke
# asserts on this line. "settings" is the policy file the agent will pass to
# --settings, so it is there only when that file can be read.
grep -q '^session-entrypoint: claude posture: mcp-config policy-hook session-hook$' <<<"$out" || fail "posture line: $(grep 'claude posture' <<<"$out")"
# The requested argv passes through to the session agent untouched.
grep -q '^VOGT_SESSION_COMMAND=\["sleep","1"\]$' "$tmp/happy/agent.env" || fail "VOGT_SESSION_COMMAND not passed through"

# A second project descriptor must reach the clone unchanged; the deployment
# default above must not leak into a project-owned source.
run_case "$tmp/other-project" \
  SOURCE_JSON='{"location":{"kind":"graft","endpoint":"http://graft","repo":"farmeggs"},"ref":"backlog-1","landing":"line"}' \
  VOGT_SOURCE_JSON='{"location":{"kind":"graft","endpoint":"http://graft","repo":"farmeggs"},"ref":"backlog-1","landing":"line"}' \
  GRAFT_TOKEN=other-graft-token >/dev/null 2>&1 || fail "other-project path"
grep -q 'clone farmeggs '"$tmp/other-project/work"' --line backlog-1' "$tmp/other-project/graft.args" || fail "project source clone scope"
! grep -q 'clone vogt '"$tmp/other-project/work" "$tmp/other-project/graft.args" || fail "deployment repo leaked into project clone"

# Without a graft token the session token is what graft clones with. An
# engine that sends no MCP config leaves the session agent nothing to pass.
run_case "$tmp/no-graft-token" >/dev/null 2>&1 || fail "no-graft-token path"
[ ! -e "$tmp/no-graft-token/run/mcp.json" ] || fail "mcp config written without one in the response"
! grep -q '^VOGT_CLAUDE_MCP_CONFIG=' "$tmp/no-graft-token/agent.env" || fail "mcp config exported without one"
posture=$(run_case "$tmp/posture" VOGT_CLAUDE_SETTINGS_FILE="$tmp/policy.json" MCP_CONFIG="$MCP" 2>&1 | grep 'claude posture') || true
[[ $posture == 'session-entrypoint: claude posture: mcp-config policy-hook session-hook' ]] || fail "unreadable policy counted: $posture"
echo '{}' >"$tmp/policy.json"
posture=$(run_case "$tmp/posture2" VOGT_CLAUDE_SETTINGS_FILE="$tmp/policy.json" MCP_CONFIG="$MCP" 2>&1 | grep 'claude posture') || true
[[ $posture == 'session-entrypoint: claude posture: settings mcp-config policy-hook session-hook' ]] || fail "readable policy not counted: $posture"
# Codex gets neither Claude's hooks nor its MCP file.
run_case "$tmp/codex" VOGT_CLI=codex MCP_CONFIG="$MCP" >/dev/null 2>&1 || fail "codex path"
[ ! -e "$tmp/codex/vogt-policy-hook.calls" ] || fail "claude hooks installed for codex"
[ ! -e "$tmp/codex/run/mcp.json" ] || fail "mcp config written for codex"
# WI-3161: Codex gets the engine's MCP servers in $CODEX_HOME/config.toml,
# replacing only the tables the engine owns; opencode gets opencode.json.
# shellcheck disable=SC2016
CODEX_TOML=$'[mcp_servers.vogt]\nurl = "https://e/mcp"\nbearer_token_env_var = "VOGT_HTTP_TOKEN"\n\n'
mkdir -p "$tmp/codexcfg/home/.codex"
printf '[model]\nname = "x"\n\n[mcp_servers.vogt]\nurl = "https://stale/mcp"\n\n[mcp_servers.other]\nurl = "https://o"\n' >"$tmp/codexcfg/home/.codex/config.toml"
out=$(run_case "$tmp/codexcfg" VOGT_CLI=codex CODEX_CONFIG="$CODEX_TOML" 2>&1) || fail "codex config path: $out"
cfg="$tmp/codexcfg/home/.codex/config.toml"
[[ $(stat -c %a "$cfg") == 600 ]] || fail "codex config mode"
grep -q 'url = "https://e/mcp"' "$cfg" || fail "codex vogt table missing"
! grep -q stale "$cfg" || fail "stale vogt table kept"
{ grep -q '^\[model\]' "$cfg" && grep -q 'mcp_servers.other' "$cfg"; } || fail "codex config's own tables lost"
[ "$(grep -c 'mcp_servers.vogt' "$cfg")" = 1 ] || fail "duplicate vogt table"
grep -q '^session-entrypoint: codex posture: mcp-config$' <<<"$out" || fail "codex posture line"
! grep -q 'minted' "$cfg" || fail "credential in codex config"
# WI-3288: quoted, spaced and dotted-key forms of the owned tables are replaced
# too, so Codex never sees a duplicate table.
mkdir -p "$tmp/codexq/home/.codex"
cat >"$tmp/codexq/home/.codex/config.toml" <<'TOML'
mcp_servers.cadastre.url = "https://stale-dotted/cadastre"
model = "x"
[mcp_servers."vogt"]
url = "https://stale-quoted/mcp"
[ mcp_servers . 'cadastre' . env ]
A = "stale-spaced"
[mcp_servers]
vogt.url = "https://stale-under-parent/mcp"
keep.url = "https://keep"
[[mcp_servers.vogt.extra]]
n = "stale-array-table"
[mcp_servers."other.one"]
args = [
  "a",
]
TOML
out=$(run_case "$tmp/codexq" VOGT_CLI=codex CODEX_CONFIG="$CODEX_TOML" 2>&1) || fail "codex quoted config path: $out"
cfg="$tmp/codexq/home/.codex/config.toml"
! grep -q stale "$cfg" || fail "stale quoted/dotted vogt/cadastre definition kept: $(grep stale "$cfg")"
grep -q 'url = "https://e/mcp"' "$cfg" || fail "codex vogt table missing (quoted case)"
{ grep -q '^model = "x"' "$cfg" && grep -q 'keep.url' "$cfg" && grep -q 'other.one' "$cfg" && grep -q '^  "a",' "$cfg"; } || fail "codex config's own entries lost (quoted case)"
[ "$(grep -c '^\[mcp_servers.vogt\]' "$cfg")" = 1 ] || fail "duplicate vogt table (quoted case)"
# WI-3491: a registered LLM provider replaces a table of the same name; the
# config's other providers stay, and no table is declared twice.
mkdir -p "$tmp/codexprov/home/.codex"
printf '[model_providers.deepseek]\nbase_url = "https://stale"\n\n[model_providers.mine]\nbase_url = "https://m"\n' >"$tmp/codexprov/home/.codex/config.toml"
PROV_TOML="$CODEX_TOML"$'\n[model_providers.deepseek]\nname = "deepseek"\nbase_url = "https://new/v1"\nwire_api = "chat"\nenv_key = "DEEPSEEK_KEY"\n'
out=$(run_case "$tmp/codexprov" VOGT_CLI=codex CODEX_CONFIG="$PROV_TOML" 2>&1) || fail "codex provider path: $out"
cfg="$tmp/codexprov/home/.codex/config.toml"
[ "$(grep -c '^\[model_providers.deepseek\]' "$cfg")" = 1 ] || fail "duplicate provider table"
grep -q 'base_url = "https://new/v1"' "$cfg" || fail "provider table not replaced"
! grep -q 'https://stale' "$cfg" || fail "stale provider table kept"
grep -q '^\[model_providers.mine\]' "$cfg" || fail "config's own provider lost"
OC='{"mcp":{"vogt":{"type":"remote","url":"https://e/mcp"}}}'
out=$(run_case "$tmp/ocfg" VOGT_CLI=opencode OPENCODE_CONFIG="$OC" XDG_CONFIG_HOME="$tmp/ocfg/xdg" 2>&1) || fail "opencode path: $out"
jq -e '.mcp.vogt.url=="https://e/mcp"' "$tmp/ocfg/xdg/opencode/opencode.json" >/dev/null || fail "opencode config content"
[[ $(stat -c %a "$tmp/ocfg/xdg/opencode/opencode.json") == 600 ]] || fail "opencode config mode"
[[ $(<"$tmp/no-graft-token/graft.token") == "$SESSION" ]] || fail "clone should fall back to the session token"

# Force-tracked files can be ignored by the checkout's .gitignore. The git
# view must still include them in both the parent and tip commits.
run_case "$tmp/ignored" DIFF_IGNORED=1 >/dev/null 2>&1 || fail "ignored-file path"
iw="$tmp/ignored/work"
[[ $(git -C "$iw" log -1 --format=%s) == "tip change (WI-1)" ]] || fail "ignored-file git view: tip subject"
[[ $(git -C "$iw" show HEAD^:tracked.txt) == old-ignored ]] || fail "ignored-file git view: parent file missing"
[[ $(git -C "$iw" show HEAD:tracked.txt) == new-ignored ]] || fail "ignored-file git view: tip file missing"
[[ $(<"$iw/tracked.txt") == new-ignored ]] || fail "ignored-file git view: working tree changed"
[[ -z $(git -C "$iw" ls-files .graft .vogt) ]] || fail "ignored-file git view: runtime state tracked"
[[ -z $(git -C "$iw" status --porcelain) ]] || fail "ignored-file git view: tree not clean"

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
