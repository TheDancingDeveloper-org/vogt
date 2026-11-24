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
# The hook only ever asks about commands that contain a word the policy can
# act on (`git`, `gh`, `rm` and the other deleting programs, the shells,
# `eval`, `xargs`), the environment programs the environment guard reads
# (`env`, `printenv`, `set`, `export`, `readonly`, `declare`, `typeset`,
# `local`, `ps`), `environ`, any `/proc` path, `vars(os)`, or a
# credential-named expansion, read from the decoded command. A command
# without one is allowed without a request, so a policy check rarely slows an
# ordinary tool call. With no broker credential to ask with it also allows,
# because there is no session policy to enforce.
#
# A denial prints the engine's reason on stderr and exits 2, which Claude Code
# treats as "do not run the tool". The engine being
# unreachable, answering anything but 2xx, or answering a body whose decision
# is not exactly "allow" fails closed: a check that cannot be read is a
# refusal, not a pass. The one exception is a command forwarded only for an
# ambiguous environment word whose every use an offline allowlist recognises
# as harmless (`set -euo pipefail`, `export FOO=bar`, `env FOO=bar cmd`); a
# real dump, a `/proc` read, a credential expansion, and any
# git/gh/destructive command stay refused.
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
        echo "vogt policy check unavailable; refusing a command the policy may cover" >&2
        exit 2
    }
fi

# A command that names nothing the policy can refuse is allowed without
# asking. The test reads the *decoded* command, never the raw JSON: JSON
# escapes a newline as `\n`, so a raw-text match would miss the `git` in
# `ls\ngit checkout -- f` (WI-1326 review). Quotes and backslashes are
# dropped and case folded first, so `g""it` and `\git` still read as git.
# Line continuations (backslash-newline) are removed first, and the command
# is also read with its backslash escapes decoded, so `r\<newline>m` and
# `$'\x72m'` read as rm. The words are the ones the engine acts on: git and
# gh, `rm` and `unlink` (the discard guard judges a plain rm by its target),
# git's discarding verbs (for `$GIT checkout …`), and the shells, `eval`,
# `xargs`, `source` and `.` that can run any of them out of sight. The engine
# does not read `mv`, `shred` or `truncate`; they are asked about only
# because a variable or opaque text holding them is a possible discard. A
# program that is an expansion (`$R a.txt`, `${R}m`) is always asked: what it
# runs is not on the line. A payload that cannot be decoded is asked.
#
# The environment guard (WI-4046) adds the programs that print the
# environment (`env`, `printenv`, `set`, `export`, `readonly`, `declare`,
# `typeset`, `local`, `ps`), `environ` for a read of /proc/<pid>/environ, and
# — matched separately below — an expansion of a credential-named variable
# (`$VOGT_HTTP_TOKEN`), an indirect expansion (`${!name}`), and a one-liner
# naming `os.environ`, `process.env` or `%ENV`.
words='git|gh|rm|unlink|mv|shred|truncate|checkout|restore|reset|clean|stash|switch|bash|sh|zsh|dash|ksh|eval|xargs|source|env|printenv|set|export|readonly|declare|typeset|local|ps|environ'
# The words that are an environment concern only. An outage may allow a
# harmless use of one of these; a destructive word keeps the refusal.
env_words='env|printenv|set|export|readonly|declare|typeset|local|ps|environ'
destructive_words='git|gh|rm|unlink|mv|shred|truncate|checkout|restore|reset|clean|stash|switch|bash|sh|zsh|dash|ksh|eval|xargs|source'
envish='\$\{?!|\$\{?[a-z0-9_]*(token|secret|key|passw|credential|private)|os\.environ|process\.env|%env'
decoded="$(printf '%s' "$input" | python3 -c 'import json, re, sys
try:
    payload = json.load(sys.stdin)
    cmd = payload.get("tool_input", {}).get("command", "")
except Exception:
    sys.exit(3)
if isinstance(cmd, list):
    cmd = "\n".join(str(part) for part in cmd)
if not isinstance(cmd, str):
    sys.exit(3)
cmd = cmd.replace("\\\n", "")
# A program that is an expansion or a command substitution (`$R`, `$(…)`,
# backticks), or a sourced script: always asked.
start = r"(^|[;&|(){}`\n!]|\b(then|do|else|eval|exec|command|builtin|sudo|env|time|nohup|xargs))\s*(\w+=\S*\s+)*"
# A wrapper with options or a duration before its program (`timeout 5 $R`,
# `nice -n 5 $R`, `stdbuf -oL $R`): a word that is an expansion anywhere
# after it in the same simple command is asked, since it may be the program.
wrapped = r"(^|[^\w-])(timeout|nice|stdbuf|setsid|ionice|doas|chronic|sudo|env|exec|command|builtin|nohup|time|xargs|watch|flock|su|script|busybox)\s+([^;&|\n]*?\s)?[\"\x27]*[$`]"
# A brace expansion as the program (`{rm,a.txt}`, `{r,}m a.txt`).
braced = start + r"[\"\x27]*\S*\{[^}\s]*,"
# A command attached to its option (`env -S"rm a.txt"`, `su -c"…"`,
# `script -qc"…"`): the word holding it starts with `-`, and the word test
# below would read `-srm` as one word (WI-4001 round 4, blocker 3).
attached = r"(^|[^\w-])(env|su|runuser|script|sudo|doas|busybox)\s+([^;&|\n]*?\s)?-[A-Za-z]*[Sc][^\s=]"
if (
    re.search(start + r"[\"\x27]*[$`]", cmd)
    or re.search(start + r"\.\s", cmd)
    or re.search(wrapped, cmd)
    or re.search(braced, cmd)
    or re.search(attached, cmd)
):
    sys.stdout.write("git")
    sys.exit(0)
try:
    unescaped = cmd.encode("latin-1", "backslashreplace").decode("unicode_escape")
except Exception:
    unescaped = "git"
for text in (cmd, unescaped):
    sys.stdout.write(text.replace("\"", "").replace("\x27", "").replace("\\", "").lower() + "\n")' 2>/dev/null)" ||
    decoded="git"
printf '%s' "$decoded" | grep -Eq "(^|[^[:alnum:]_-])($words)([^[:alnum:]_-]|\$)" ||
    printf '%s' "$decoded" | grep -Eq "$envish" ||
    printf '%s' "$decoded" | grep -Fq '/proc' ||
    printf '%s' "$decoded" | grep -Eq 'vars[[:space:]]*\([[:space:]]*os[[:space:]]*\)' ||
    exit 0

# Whether the command was asked about only for an environment word (or an
# environment-looking expansion). A destructive word, or the "git" sentinel
# the decoded filter emits for a hidden program/brace/substitution, clears
# this: the outage allowlist below never covers those.
ask_env=0
if printf '%s' "$decoded" | grep -Eq "(^|[^[:alnum:]_-])($env_words)([^[:alnum:]_-]|\$)" ||
    printf '%s' "$decoded" | grep -Eq "$envish"; then
    ask_env=1
fi
if printf '%s' "$decoded" | grep -Eq "(^|[^[:alnum:]_-])($destructive_words)([^[:alnum:]_-]|\$)"; then
    ask_env=0
fi

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
    # The engine cannot be asked. A command forwarded only because it names
    # one of the ambiguous environment words (`set -euo pipefail`,
    # `export FOO=bar && …`) is still allowed when an offline allowlist
    # recognises every such use as harmless. Everything else keeps the
    # existing fail-closed refusal: a real dump, another language reading the
    # environment, a credential expansion, or any git/gh/destructive call.
    if [[ "$ask_env" == 1 ]] && printf '%s' "$input" | python3 -c 'import json, re, shlex, sys

try:
    payload = json.load(sys.stdin)
    cmd = payload.get("tool_input", {}).get("command", "")
except Exception:
    sys.exit(1)
if isinstance(cmd, list):
    cmd = "\n".join(str(part) for part in cmd)
if not isinstance(cmd, str) or not cmd.strip():
    sys.exit(1)

SENSITIVE = {
    "TOKEN", "TOKENS", "SECRET", "SECRETS", "PASSWORD", "PASSWORDS",
    "PASSWD", "CREDENTIAL", "CREDENTIALS", "PRIVATE",
}

# A program that can read the environment inside its own program, where the
# line itself shows no expansion. An outage cannot ask the engine, so a
# segment that runs one is refused however harmless its arguments look: the
# old fall-through allowed a Ruby one-liner printing ENV and jq -n env
# (WI-4061 review).
INTERPRETERS = {
    "python", "pypy", "node", "nodejs", "perl", "ruby", "irb", "awk",
    "gawk", "mawk", "nawk", "jq", "php", "lua", "luajit", "Rscript",
    "tclsh", "pwsh", "powershell",
}


def interpreter(name):
    return (
        name in INTERPRETERS
        or re.match(r"^(python|pypy|perl|ruby|node|php|lua)[0-9.]+$", name) is not None
    )


def credential(name):
    n = name.upper()
    if n == "KEY" or n in SENSITIVE:
        return True
    if n.endswith("_KEY") or "_KEY_" in n:
        return True
    return any(n.endswith("_" + w) or "_" + w + "_" in n for w in SENSITIVE)


if re.search(
    r"(^|[^A-Za-z0-9_-])(git|gh|rm|unlink|mv|shred|truncate|checkout|restore|reset|clean|stash|switch|bash|sh|zsh|dash|ksh|eval|xargs|source)([^A-Za-z0-9_-]|$)",
    cmd,
):
    sys.exit(1)
def proc_read(text):
    # The raw line hides a read behind quotes (`env""iron`) or a glob
    # (`env*`, `e?viron`, `envir[o]n`), so read it with quotes and
    # backslashes removed. An outage cannot ask the engine whose answer
    # would name the file, so any /proc read fails closed.
    flat = text.replace("\"", "").replace("\x27", "").replace("\\", "")
    if "/proc/" in flat:
        return True
    return re.search(r"/proc/\S*(environ|env[*?\[]|[*?\[])", flat, re.I) is not None


if proc_read(cmd):
    sys.exit(1)
if re.search(r"(?i)(os\.environ|os\.getenv|process\.env|%env|\benviron\b|\$ENV\b|ENVIRON)", cmd):
    sys.exit(1)
if "<<<" in cmd:
    sys.exit(1)
for found in re.finditer(r"\$\{?(!?)([A-Za-z_][A-Za-z0-9_]*)", cmd):
    if credential(found.group(2)):
        sys.exit(1)


def words_of(text):
    try:
        return shlex.split(text, posix=True)
    except ValueError:
        return None


def strip_wrappers(words):
    i = 0
    while i < len(words):
        word = words[i]
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", word):
            i += 1
            continue
        name = word.rsplit("/", 1)[-1]
        if name in ("sudo", "doas", "command", "exec", "nohup", "builtin",
                    "setsid", "chronic", "busybox", "time"):
            i += 1
            continue
        if name in ("nice", "ionice", "stdbuf"):
            i += 1
            while i < len(words) and words[i].startswith("-"):
                takes = words[i] in ("-n", "--adjustment", "-c", "--class",
                                     "-p", "-t", "-i", "-o", "-e")
                i += 1
                if takes and i < len(words) and not words[i].startswith("-"):
                    i += 1
            continue
        if name == "timeout":
            i += 1
            while i < len(words) and words[i].startswith("-"):
                takes = words[i] in ("-s", "--signal", "-k", "--kill-after")
                i += 1
                if takes:
                    i += 1
            if i < len(words) and not words[i].startswith("-"):
                i += 1
            continue
        break
    return words[i:]


def env_runs_a_program(args, depth=0):
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "-" or arg.startswith("--"):
            if arg == "--" or arg.startswith(("--unset=", "--chdir=")):
                i += 1
                continue
            if arg == "--ignore-environment":
                i += 1
                continue
            if arg in ("--unset", "--chdir"):
                i += 2
                continue
            return False
        if arg.startswith("-") and len(arg) > 1:
            flag = arg[1:]
            if "S" in flag:
                return False
            if flag in ("u", "C"):
                i += 2
                continue
            if flag[-1] in ("u", "C"):
                i += 1
                continue
            i += 1
            continue
        if re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", arg):
            i += 1
            continue
        return depth < 8 and harmless(" ".join(args[i:]), depth + 1)
    return False


def harmless(seg, depth=0):
    if proc_read(seg):
        return False
    words = words_of(seg)
    if words is None:
        return False
    words = strip_wrappers(words)
    if not words:
        return True
    name = words[0].rsplit("/", 1)[-1]
    if interpreter(name):
        return False
    # A launcher can run an interpreter as its operand (`find -exec jq -n
    # env`, `watch ruby -e …`), so the program in front is not the whole
    # story.
    if name in ("find", "xargs", "watch", "parallel") and any(
        interpreter(word.rsplit("/", 1)[-1]) for word in words[1:]
    ):
        return False
    args = words[1:]
    flags = [a for a in args if a.startswith("-")]
    operands = [a for a in args if not a.startswith(("-", "+"))]
    if name == "set":
        if not args:
            return False
        previous = ""
        for arg in args:
            if arg == "--":
                return True
            if arg.startswith(("-", "+")):
                previous = arg
                continue
            if "o" in previous and not previous.startswith("+"):
                previous = ""
                continue
            return False
        return True
    if name in ("export", "readonly", "local", "declare", "typeset"):
        if any("p" in flag[1:] for flag in flags):
            return False
        if not operands:
            functions_only = bool(flags) and all(
                flag[1:] and set(flag[1:]) <= {"f", "F"} for flag in flags
            )
            return functions_only
        return all(
            re.match(r"^[A-Za-z_][A-Za-z0-9_]*(=.*)?$", a, re.S) for a in operands
        )
    if name == "env":
        return env_runs_a_program(args, depth)
    if name == "printenv":
        return bool(operands) and all(
            not credential(a) and "$" not in a and "`" not in a for a in operands
        )
    if name == "ps":
        return not any(
            not a.startswith("-") and a.isalpha() and "e" in a for a in args
        )
    return True


for segment in re.split(r"&&|\|\||[;&|\n(){}]", cmd):
    if segment.strip() and not harmless(segment):
        sys.exit(1)
sys.exit(0)
' 2>/dev/null; then
        rm -f "$body"
        exit 0
    fi
    rm -f "$body"
    echo "vogt policy check unavailable; refusing a command the policy may cover" >&2
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
    reason="vogt policy check unreadable; refusing a command the policy may cover"
fi
rm -f "$body"
printf '%s\n' "$reason" >&2
exit 2
