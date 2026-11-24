#!/usr/bin/env bash
# vogt-claude-session-hook: Claude Code's SessionStart / SessionEnd hook, which
# tells the engine which conversation runs in the session it is in (WI-962).
#
#   vogt-claude-session-hook start   # SessionStart: link the conversation
#   vogt-claude-session-hook end     # SessionEnd: unlink it again
#   vogt-claude-session-hook install [settings.json]
#                                    # add both hooks to Claude Code's user
#                                    # settings, once (the entrypoint runs it)
#
# Claude Code passes the hook a JSON object on stdin whose `session_id` is the
# conversation id. The engine pins the id itself for a `claude` it launches,
# but not for one a person types into a plain shell. Before this hook, such a
# session was a shell to the engine: nothing could hibernate or resume it, a
# restart dropped its record, and History showed only `bash`. The hook covers
# that case, plus `/clear` (a new id) and `--resume` (the id resumed).
#
# The entrypoint installs this hook into the pod user's Claude Code settings,
# so it runs for every `claude` in the pod. It does nothing outside an engine
# session, for a `claude` whose stdin is not a terminal (a `claude -p` an
# agent runs from a tool call, which is not the session's own conversation),
# and when there is no credential to report with. It prints nothing: a
# SessionStart hook's output becomes context for the model. It never fails
# the CLI.
#
# Credential: the session's own broker token (`VOGT_ENGINE_BROKER_TOKEN`),
# which names the session by itself, when the deployment brokers secrets;
# otherwise `VOGT_HTTP_TOKEN` against the engine's session route.
set -u

event="${1:-start}"

# Add the two hooks to Claude Code's user settings, keeping everything else in
# the file and adding nothing twice. A file that is not a JSON object is left
# exactly as it was.
if [[ "$event" == "install" ]]; then
    settings="${2:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json}"
    self="$(readlink -f "$0")"
    exec python3 - "$settings" "$self" <<'PY'
import json, os, sys, tempfile
path, hook = sys.argv[1], sys.argv[2]
try:
    with open(path, encoding="utf-8") as fh:
        settings = json.load(fh)
except FileNotFoundError:
    settings = {}
except ValueError:
    sys.exit(0)
if not isinstance(settings, dict) or not isinstance(settings.get("hooks", {}), dict):
    sys.exit(0)
hooks = settings.setdefault("hooks", {})
changed = False
for event, arg in (("SessionStart", "start"), ("SessionEnd", "end")):
    groups = hooks.get(event)
    if not isinstance(groups, list):
        groups = []
    present = any(
        isinstance(h, dict) and str(h.get("command", "")).split(" ")[0] == hook
        for g in groups if isinstance(g, dict)
        for h in (g.get("hooks") if isinstance(g.get("hooks"), list) else [])
    )
    if not present:
        groups.append({"hooks": [{"type": "command", "command": f"{hook} {arg}"}]})
        hooks[event] = groups
        changed = True
if not changed:
    sys.exit(0)
directory = os.path.dirname(os.path.abspath(path))
os.makedirs(directory, exist_ok=True)
fd, tmp = tempfile.mkstemp(dir=directory, prefix=".settings.json.")
with os.fdopen(fd, "w", encoding="utf-8") as fh:
    json.dump(settings, fh, indent=2)
    fh.write("\n")
if os.path.exists(path):
    os.chmod(tmp, os.stat(path).st_mode & 0o777)
os.replace(tmp, path)
PY
fi

[[ -n "${VOGT_ENGINE_SESSION_ID:-}" ]] || exit 0
command -v curl >/dev/null 2>&1 || exit 0

input="$(head -c 65536)"
id="$(printf '%s' "$input" \
    | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([A-Za-z0-9._-]*\)".*/\1/p' \
    | head -n 1)"
[[ -n "$id" ]] || exit 0

# The nearest `claude` above this hook, and whether a person is at it: its
# stdin is a terminal. No `claude` found (a renamed binary, a wrapper) counts
# as interactive; the engine still validates what is reported.
interactive_cli() {
    local pid="$PPID" depth=0 comm stat
    while (( depth < 6 )) && [[ "$pid" =~ ^[0-9]+$ ]] && (( pid > 1 )); do
        comm="$(cat "/proc/$pid/comm" 2>/dev/null)" || return 0
        if [[ "$comm" == claude* ]]; then
            [[ "$(readlink "/proc/$pid/fd/0" 2>/dev/null)" == /dev/pts/* ]]
            return
        fi
        stat="$(cat "/proc/$pid/stat" 2>/dev/null)" || return 0
        # Field 4 (ppid), after the parenthesised command name, which may
        # itself hold spaces or parentheses.
        pid="$(printf '%s' "${stat##*) }" | cut -d ' ' -f 2)"
        depth=$((depth + 1))
    done
    return 0
}
interactive_cli || exit 0

ended=false
[[ "$event" == "end" ]] && ended=true
body="$(printf '{"agent":"claude","id":"%s","ended":%s}' "$id" "$ended")"

if [[ -n "${VOGT_ENGINE_BROKER_TOKEN:-}" && -n "${VOGT_ENGINE_BROKER_URL:-}" ]]; then
    token="$VOGT_ENGINE_BROKER_TOKEN"
    url="${VOGT_ENGINE_BROKER_URL%/}/api/agent-auth/conversation"
elif [[ -n "${VOGT_HTTP_TOKEN:-}" && -n "${VOGT_ENGINE_URL:-}" ]]; then
    token="$VOGT_HTTP_TOKEN"
    url="${VOGT_ENGINE_URL%/}/api/sessions/${VOGT_ENGINE_SESSION_ID}/conversation"
else
    exit 0
fi

# The token goes in a header read from a file descriptor, never in argv,
# where every process in the pod could read it.
curl -sS --max-time 3 -o /dev/null -X POST \
    -H @<(printf 'Authorization: Bearer %s\n' "$token") \
    -H 'Content-Type: application/json' --data-binary @- \
    "$url" <<<"$body" >/dev/null 2>&1 || true
exit 0
