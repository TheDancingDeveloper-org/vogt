#!/usr/bin/env bash
# Register or remove a stdio MCP server in Klaudia's user config (WI-974).
#
# Klaudia reads MCP servers from <config dir>/.mcp.json — ~/.klaudia, or
# KLAUDIA_CONFIG_DIR — in Claude Code's `mcpServers` shape, and unlike the
# other agent CLIs it has no `mcp add`/`mcp remove` to write it. This is that
# command, so `vogt-mcp-bootstrap` (and a derivative image's own bootstrap)
# can reconcile Klaudia the way it does Claude Code, Codex and opencode:
#
#   vogt-klaudia-mcp set [-e KEY=VALUE]... NAME COMMAND [ARG...]
#                                     upsert NAME to run COMMAND ARG...
#   vogt-klaudia-mcp remove NAME COMMAND
#                                     remove NAME only if it runs COMMAND
#
# `set` writes only when the entry differs, and keeps every other server and
# key in the file. `remove` touches only an entry this kind of caller wrote
# (one running COMMAND), so a server an operator registered by hand under the
# same name survives. Klaudia starts a stdio server with its own environment,
# which is where the session's tokens are, so `-e` is for fixed, non-secret
# settings (an endpoint, an allowlist) and never for a credential. Nothing
# goes to stdout — the bootstrap that calls this runs ahead of a stdio MCP
# server, whose stdout is the protocol.
set -euo pipefail

config="${KLAUDIA_CONFIG_DIR:-$HOME/.klaudia}/.mcp.json"

usage() {
    printf 'usage: vogt-klaudia-mcp set [-e KEY=VALUE]... NAME COMMAND [ARG...] | remove NAME COMMAND\n' >&2
    exit 2
}

op="${1:-}"
[[ $# -gt 0 ]] && shift
env_pairs=()
if [[ "$op" == set ]]; then
    while [[ "${1:-}" == -e ]]; do
        [[ "${2:-}" == ?*=* ]] || usage
        env_pairs+=("$2")
        shift 2
    done
    [[ $# -ge 2 ]] || usage
elif [[ "$op" == remove ]]; then
    [[ $# -eq 2 ]] || usage
else
    usage
fi

# The env pairs travel as one NUL-free, newline-separated argument ahead of
# the rest; a KEY=VALUE holding a newline is refused rather than split.
for pair in "${env_pairs[@]}"; do
    [[ "$pair" != *$'\n'* ]] || usage
done
env_arg="$(printf '%s\n' "${env_pairs[@]}")"
[[ ${#env_pairs[@]} -gt 0 ]] || env_arg=""

exec python3 - "$config" "$op" "$env_arg" "$@" <<'PY'
import json, os, sys, tempfile

path, op, env_arg, name, command, *args = sys.argv[1:]
env = dict(line.split("=", 1) for line in env_arg.splitlines() if line)
try:
    with open(path, encoding="utf-8") as fh:
        data = json.load(fh)
except FileNotFoundError:
    data = {}
except (OSError, ValueError) as exc:
    # A file Klaudia itself could not read either; never overwrite it blind.
    print(f"vogt-klaudia-mcp: {path} is not readable JSON ({exc}); left alone", file=sys.stderr)
    sys.exit(1)
if not isinstance(data, dict):
    print(f"vogt-klaudia-mcp: {path} is not a JSON object; left alone", file=sys.stderr)
    sys.exit(1)
servers = data.get("mcpServers")
if not isinstance(servers, dict):
    servers = {}
current = servers.get(name)

if op == "set":
    wanted = {"command": command}
    if args:
        wanted["args"] = args
    if env:
        wanted["env"] = env
    if current == wanted:
        sys.exit(0)
    servers[name] = wanted
else:
    if not (isinstance(current, dict) and current.get("command") == command):
        sys.exit(0)
    del servers[name]

data["mcpServers"] = servers
directory = os.path.dirname(path)
os.makedirs(directory, mode=0o700, exist_ok=True)
fd, tmp = tempfile.mkstemp(dir=directory, prefix=".mcp.json.")
with os.fdopen(fd, "w", encoding="utf-8") as fh:
    json.dump(data, fh, indent=2)
    fh.write("\n")
os.replace(tmp, path)
PY
