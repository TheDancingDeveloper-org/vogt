#!/usr/bin/env bash
# vogt-policy-hook: a PreToolUse hook that asks the engine whether the shell
# command an agent is about to run breaks the project's policy (WI-1243).
#
#   vogt-policy-hook                 # the hook: read the payload on stdin
#   vogt-policy-hook codex           # the same check for a Codex managed hook:
#                                    # translate Codex's tool_input, then ask
#   vogt-policy-hook install-policy [settings.json]
#                                    # add the hook to Claude Code's user
#                                    # settings, once (the entrypoint runs it)
#
# The hook only ever looks at commands that contain a `git` or `gh` word. A
# command without one is allowed without a request, so a policy check can
# never slow an ordinary tool call. With no broker credential to ask with it
# also allows, because there is no session policy to enforce.
#
# A denial prints the engine's reason on stderr and exits 2, which Claude Code
# treats as "do not run the tool". The engine being
# unreachable, answering anything but 2xx, or answering a body whose decision
# is not exactly "allow" fails closed for those git and gh commands: a check
# that cannot be read is a refusal, not a pass.
#
# Credential: the session's own broker token (`VOGT_ENGINE_BROKER_TOKEN`),
# which names the session by itself; a pod session, which has none, presents
# its stream credential from `VOGT_SESSION_TOKEN_FILE`. It goes in a header read from a file
# descriptor, never in argv, where every process in the pod could read it.
set -u

event="${1:-check}"

# Add the hook to Claude Code's user settings, keeping everything else in the
# file and adding nothing twice. A settings file that is not a JSON object is
# left exactly as it was.
if [[ "$event" == "install-policy" ]]; then
    settings="${2:-${CLAUDE_CONFIG_DIR:-$HOME/.claude}/settings.json}"
    self="$(readlink -f "$0")"
    exec python3 - "$settings" "$self" <<'PY'
import json, os, sys, tempfile

settings_path, hook = sys.argv[1:]

try:
    with open(settings_path, encoding="utf-8") as fh:
        settings = json.load(fh)
except FileNotFoundError:
    settings = {}
except ValueError:
    settings = None
if isinstance(settings, dict) and isinstance(settings.get("hooks", {}), dict):
    hooks = settings.setdefault("hooks", {})
    groups = hooks.get("PreToolUse")
    if not isinstance(groups, list):
        groups = []
    present = any(
        isinstance(h, dict) and str(h.get("command", "")).split(" ")[0] == hook
        for g in groups if isinstance(g, dict)
        for h in (g.get("hooks") if isinstance(g.get("hooks"), list) else [])
    )
    if not present:
        groups.append({
            "matcher": "Bash",
            "hooks": [{"type": "command", "command": hook}],
        })
        hooks["PreToolUse"] = groups
        directory = os.path.dirname(os.path.abspath(settings_path))
        os.makedirs(directory, exist_ok=True)
        fd, tmp = tempfile.mkstemp(dir=directory, prefix=".settings.json.")
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(settings, fh, indent=2)
            fh.write("\n")
        if os.path.exists(settings_path):
            os.chmod(tmp, os.stat(settings_path).st_mode & 0o777)
        os.replace(tmp, settings_path)
PY
fi

limit=65536
[[ "$event" == "codex" ]] && limit=1048576
input="$(head -c "$limit")"
raw="$input"

# Codex's managed hook sends Codex's tool_input shape, which is not the
# engine's `{"tool_name": "Bash", "tool_input": {"command": "..."}}`. Translate
# it here so both agents reach the same policy check. A payload this cannot
# read is a refusal, not a pass: a check that cannot be asked must fail closed,
# and it must fail closed before the git/gh shortcut can skip the translation.
if [[ "$event" == "codex" ]]; then
    input="$(printf '%s' "$input" | python3 -c 'import json, os, sys

def die():
    sys.exit(2)

try:
    payload = json.load(sys.stdin)
except Exception:
    die()
if not isinstance(payload, dict):
    die()
tool = payload.get("tool_name") or ""
if not isinstance(tool, str):
    die()
if tool not in ("Bash", "shell", "exec_command", "write_stdin"):
    json.dump({"tool_name": tool}, sys.stdout)
    sys.exit(0)
ti = payload.get("tool_input")
if ti is None:
    ti = {}
if not isinstance(ti, dict):
    die()
cmd = ti.get("command")
if cmd is None:
    cmd = ti.get("cmd")
if cmd is None:
    cmd = ti.get("input", "")
if isinstance(cmd, list):
    cmd = "\n".join(str(part) for part in cmd)
if not isinstance(cmd, str):
    die()
cwd = ti.get("workdir") or payload.get("cwd") or ""
if not isinstance(cwd, str):
    cwd = ""
base = payload.get("cwd") or ""
if cwd and not os.path.isabs(cwd) and isinstance(base, str) and os.path.isabs(base):
    cwd = os.path.join(base, cwd)
json.dump({"tool_name": "Bash", "tool_input": {"command": cmd}, "cwd": cwd}, sys.stdout)')" || {
        echo "vogt policy check unavailable; refusing git/gh write" >&2
        exit 2
    }
fi

# No git and no gh means nothing the policy can refuse, so do not ask. This
# reads the raw payload, not the translation: a Codex command may be a JSON
# array, and the translated string escapes the newlines that separate words.
# `rm` is asked too: a plain rm deletes the files a discard guard protects
# (WI-1326), and the engine reads it as `git rm` for that rule.
printf '%s' "$raw" | grep -Eq '(^|[^[:alnum:]_])(git|gh|rm)([^[:alnum:]_]|$)' || exit 0

url="${VOGT_ENGINE_BROKER_URL:-}"
token="${VOGT_ENGINE_BROKER_TOKEN:-}"
# A pod session has no broker token. Its stream credential, a 0600 file the
# entrypoint wrote, names the session to the same route (WI-3243).
if [[ -z "$token" && -z "$url" && -r "${VOGT_SESSION_TOKEN_FILE:-}" ]]; then
    token="$(head -c 4096 "$VOGT_SESSION_TOKEN_FILE" 2>/dev/null | tr -d '[:space:]')"
    url="${VOGT_ENGINE_URL:-}"
fi
if [[ -z "$url" || -z "$token" ]]; then
    exit 0
fi

body="$(mktemp)"
code="$(curl -sS --max-time 5 -o "$body" -w '%{http_code}' -X POST \
    -H @<(printf 'Authorization: Bearer %s\n' "$token") \
    -H 'Content-Type: application/json' --data-binary @- \
    "${url%/}/api/agent-auth/policy-check" <<<"$input" 2>/dev/null)" || code="000"
if [[ "$code" != 2* ]]; then
    rm -f "$body"
    echo "vogt policy check unavailable; refusing git/gh write" >&2
    exit 2
fi

# Exit 0 only on a decision that is exactly "allow". Anything else — a deny, a
# body that is not JSON, a decision of some other word, an empty body, or no
# python3 to read it with — refuses. A 2xx alone is not a pass.
decision="$(python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("decision", ""))
except Exception:
    print("")' <"$body" 2>/dev/null || true)"
if [[ "$decision" == "allow" ]]; then
    rm -f "$body"
    exit 0
fi
if [[ "$decision" == "deny" ]]; then
    reason="$(python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("reason", "") or "refused by project policy")
except Exception:
    print("refused by project policy")' <"$body" 2>/dev/null || echo "refused by project policy")"
else
    reason="vogt policy check unreadable; refusing git/gh write"
fi
rm -f "$body"
printf '%s\n' "$reason" >&2
exit 2
