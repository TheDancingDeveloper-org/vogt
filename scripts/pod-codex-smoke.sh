#!/usr/bin/env bash
# Pod Codex posture smoke: the launch line a pod really runs, in the pod image.
#
# A pod cannot create the user namespace Codex's default `workspace-write`
# sandbox needs, so a launch that omits the host's posture flags fails its
# first tool call with `bwrap: No permissions to create a new namespace` and
# asks for approval (WI-3864). Unit tests pin the flags; this runs them.
#
#     POD_IMAGE=…/vogt-dev-pod@sha256:… \
#     POD_CODEX_MODEL=gpt-5.4-mini POD_CODEX_BASE_URL=http://100.92.54.45:4000/v1 \
#     POD_CODEX_ENV_FILE=./litellm.env \
#         scripts/pod-codex-smoke.sh
#
# It generates the line with the session agent's own `command_line` (through
# `engine/session-agent/examples/print_command_line.rs`) and then drives that
# line under a PTY in the image with the pod's security context: uid 1000,
# `--cap-drop ALL`, `no-new-privileges`, a git checkout at `/work`, and a
# `~/.codex/config.toml` carrying a provider as the bootstrap supplies one.
# The model is asked to run `echo PONG`; the screen must show the tool result,
# no `bwrap` error and no approval menu.
#
# Environment:
#   POD_IMAGE                  pod image ref to run (required)
#   POD_CODEX_MODEL            model the provider serves (required)
#   POD_CODEX_BASE_URL         provider base URL (required)
#   POD_CODEX_ENV_FILE         file of KEY=VALUE lines holding the provider key
#                              (required unless POD_CODEX_API_KEY is exported)
#   POD_CODEX_ENV_KEY          the provider's `env_key` (default
#                              POD_CODEX_API_KEY); the key must be under this
#                              name, in the env file or the environment
#   POD_CODEX_API_KEY          the key itself, if not using ENV_FILE
#   POD_CODEX_PROVIDER         `model_providers` key (default litellm)
#   POD_CODEX_WIRE_API         chat or responses (default chat)
#   POD_CODEX_PERMISSION_MODE  posture to launch with; unset is the engine's
#                              default posture (`default`)
#   POD_CODEX_APPROVAL_POLICY  Codex approval policy (default `never`)
#   POD_CODEX_TASK             prompt typed into the session
#   POD_CODEX_TIMEOUT          seconds to wait for the tool result (default 180)
#
# Needs docker, cargo and uv (the screen is parsed with pyte). This is a
# developer/acceptance harness, not a CI job: it calls a real model.
set -euo pipefail
unset CDPATH VOGT_ENGINE_AGENT_TASK_PROMPT_FILE
SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

die() { echo "pod-codex-smoke: $*" >&2; exit 1; }

for tool in docker cargo uv; do
  command -v "$tool" >/dev/null || die "$tool is required"
done
[ -n "${POD_IMAGE:-}" ] || die "set POD_IMAGE to the pinned session pod image"
[ -n "${POD_CODEX_MODEL:-}" ] || die "set POD_CODEX_MODEL"
[ -n "${POD_CODEX_BASE_URL:-}" ] || die "set POD_CODEX_BASE_URL"

POD_CODEX_PROVIDER="${POD_CODEX_PROVIDER:-litellm}"
POD_CODEX_WIRE_API="${POD_CODEX_WIRE_API:-chat}"
POD_CODEX_ENV_KEY="${POD_CODEX_ENV_KEY:-POD_CODEX_API_KEY}"
POD_CODEX_TASK="${POD_CODEX_TASK:-Run the shell command echo PONG and reply with exactly its output.}"
POD_CODEX_TIMEOUT="${POD_CODEX_TIMEOUT:-180}"

# Docker gets the key by name, never in argv. `--env-file` keeps it out of the
# command line and out of this transcript.
if [ -n "${POD_CODEX_ENV_FILE:-}" ]; then
  [ -r "$POD_CODEX_ENV_FILE" ] || die "POD_CODEX_ENV_FILE $POD_CODEX_ENV_FILE is not readable"
  grep -q "^${POD_CODEX_ENV_KEY}=" "$POD_CODEX_ENV_FILE" \
    || die "POD_CODEX_ENV_FILE does not define ${POD_CODEX_ENV_KEY}"
  POD_ENV_ARGS=(--env-file "$POD_CODEX_ENV_FILE")
elif [ -n "${POD_CODEX_API_KEY:-}" ]; then
  export "${POD_CODEX_ENV_KEY}=${POD_CODEX_API_KEY}"
  POD_ENV_ARGS=(-e "$POD_CODEX_ENV_KEY")
else
  die "set POD_CODEX_ENV_FILE (KEY=VALUE lines) or POD_CODEX_API_KEY"
fi

# `Config::from_env` reads a token file; the smoke never dials the engine, so
# the value is a placeholder, not a credential.
token_file="$(mktemp)"
printf 'pod-codex-smoke\n' > "$token_file"

line="$(VOGT_SESSION_ID=pod-codex-smoke \
  VOGT_ENGINE_URL=http://127.0.0.1:9 \
  VOGT_SESSION_TOKEN_FILE="$token_file" \
  VOGT_CLI=codex \
  VOGT_SESSION_CWD=/work \
  VOGT_PERMISSION_MODE="${POD_CODEX_PERMISSION_MODE:-}" \
  VOGT_CODEX_APPROVAL_POLICY="${POD_CODEX_APPROVAL_POLICY:-}" \
  cargo run --quiet --manifest-path engine/Cargo.toml -p vogt-session-agent \
    --example print_command_line)"

echo "launch line: ${line}"
# A line without a sandbox posture would fail this smoke for the wrong reason
# (the CLI's own sandbox), so refuse it before starting a container. The line
# quotes each flag, so accept the quoted and the bare spelling of the posture.
case "$line" in
  *"'-s' 'danger-full-access'"*|*"-s danger-full-access"*|*"--dangerously-bypass-approvals-and-sandbox"*) ;;
  *) die "the generated line carries no Codex sandbox posture: $line" ;;
esac

export POD_CODEX_LINE="$line"
export POD_CODEX_TASK POD_CODEX_TIMEOUT POD_CODEX_PROVIDER
export POD_CODEX_MODEL POD_CODEX_BASE_URL POD_CODEX_WIRE_API POD_CODEX_ENV_KEY
export POD_CODEX_ENV_FILE="${POD_CODEX_ENV_FILE:-}"

# The PTY driver is the WI-3839 harness: `docker run -it` under its own pty,
# pyte as the screen, answering the DSR/DA queries the TUI waits for.
uv run --quiet --with pyte python - "$POD_IMAGE" "${POD_ENV_ARGS[@]}" <<'PY'
"""Drive one pinned-pod Codex launch under a PTY and judge its screen."""
import fcntl
import os
import pty
import re
import select
import struct
import sys
import termios
import time

import pyte

image = sys.argv[1]
# Extra docker arguments (the provider key's --env-file or -e) come after the
# image name; the shell passes them through so the value never reaches argv.
pod_env_args = sys.argv[2:]

line = os.environ["POD_CODEX_LINE"]
typed = os.environ["POD_CODEX_TASK"]
timeout = float(os.environ["POD_CODEX_TIMEOUT"])

config = f'''model = "{os.environ["POD_CODEX_MODEL"]}"
model_provider = "{os.environ["POD_CODEX_PROVIDER"]}"

[model_providers.{os.environ["POD_CODEX_PROVIDER"]}]
name = "{os.environ["POD_CODEX_PROVIDER"]}"
base_url = "{os.environ["POD_CODEX_BASE_URL"]}"
wire_api = "{os.environ["POD_CODEX_WIRE_API"]}"
env_key = "{os.environ["POD_CODEX_ENV_KEY"]}"
'''
inner = f'''mkdir -p "$HOME/.codex"
cat > "$HOME/.codex/config.toml" <<'CFG'
{config}CFG
# The pod's /work is a git view; Codex asks its folder-trust question for a git
# cwd, which the launch line answers.
git init -q . && git -c user.email=smoke@example.invalid -c user.name=smoke commit --allow-empty -qm init
exec sh -lc "$POD_CODEX_LINE"
'''
cmd = [
    "docker", "run", "--rm", "-it", "--user", "1000:1000",
    "--cap-drop", "ALL", "--security-opt", "no-new-privileges",
    "-e", "TERM=xterm-256color",
    "-e", "POD_CODEX_LINE=" + line,
    "--tmpfs", "/work:rw,exec,mode=0755,uid=1000,gid=1000",
    "--tmpfs", "/home/vogt/.codex:rw,exec,mode=0755,uid=1000,gid=1000",
    "--entrypoint", "/bin/bash", "-w", "/work",
    *pod_env_args, image, "-lc", inner,
]

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
pid = os.fork()
if pid == 0:
    os.close(master)
    os.setsid()
    fcntl.ioctl(slave, termios.TIOCSCTTY, 0)
    for fd in (0, 1, 2):
        os.dup2(slave, fd)
    os.execvp(cmd[0], cmd)
os.close(slave)

screen = pyte.Screen(120, 40)
stream = pyte.ByteStream(screen)
start = time.time()
sent_at = None
while time.time() - start < timeout:
    ready, _, _ = select.select([master], [], [], 0.3)
    if ready:
        try:
            chunk = os.read(master, 65536)
        except OSError:
            break
        if not chunk:
            break
        stream.feed(chunk)
        text = chunk.decode("utf-8", "replace")
        if "\x1b[6n" in text:
            os.write(master, b"\x1b[1;1R")
        if "\x1b[c" in text:
            os.write(master, b"\x1b[?1;2c")
    display = "\n".join(screen.display)
    if sent_at is None and "Ask Codex to do anything" in display:
        time.sleep(1.0)
        for byte in typed.encode():
            os.write(master, bytes([byte]))
            time.sleep(0.03)
        time.sleep(1.5)
        os.write(master, b"\r")
        sent_at = time.time()
    if sent_at is not None and time.time() - sent_at > 3:
        # The prompt carries the command text; judge the tool result instead.
        if "PONG" in display.replace(typed, ""):
            break
    if sent_at is not None and time.time() - sent_at > timeout - 10:
        break

display = "\n".join(screen.display)
print("=== screen ===")
print("\n".join(row.rstrip() for row in display.splitlines() if row.strip()))
print("=== checks ===")
answer = [row for row in display.splitlines()
          if "PONG" in row and typed not in row and not row.lstrip().startswith("›")]
bwrap = "bwrap" in display
approval = bool(re.search(r"Allow|Approval|approve|Approve", display))
print("tool result with PONG:", bool(answer))
print("bwrap error:", bwrap)
print("approval menu:", approval)
try:
    os.kill(pid, 9)
    os.waitpid(pid, 0)
except Exception:
    pass
if not answer or bwrap or approval:
    sys.exit(1)
PY

echo "pod-codex-smoke: PASS — the generated line ran a shell tool call in the pod image with no bwrap error and no approval menu"
