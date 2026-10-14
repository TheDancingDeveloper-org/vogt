#!/usr/bin/env bash
# The pod is itself the trusted isolation boundary. Run Codex without its
# nested filesystem/network sandbox so it can work across the complete shared
# workspace tree (the pod's $HOME/Working) and use the pod's authenticated
# service access.
#
# Which Codex runs is the runtime pin's decision: when
# `vogt-agent-cli-install` has flipped `codex/current` under the agent CLI
# root, that copy is preferred; otherwise the image's own, installed under
# libexec by engine/Dockerfile. Codex gets no PATH-first `bin/` link of its
# own for exactly this reason — this wrapper is the single entry point, so
# the posture flags the engine adds apply to whichever copy is active.
#
# The sandbox is chosen per session (WI-1143), not forced here. The pod
# cannot create a user namespace, so the engine asks for danger-full-access
# rather than workspace-write, and a bypass is a person's grant. Forcing the
# bypass flag in this wrapper would apply it to a session that asked for the
# default posture, and a session whose policy could not load must not start
# that way either.
set -euo pipefail

runtime="${VOGT_AGENT_CLI_ROOT:-/opt/vogt/agent-clis}/codex/current/bin/codex"
if [[ -x "$runtime" ]]; then
    codex_real="$runtime"
else
    codex_real=/usr/local/libexec/codex-real
fi

# The app-server daemon (WI-1142) copies this binary into $CODEX_HOME and
# execs that copy, which then sits outside the pin. `--no-daemon` keeps the
# process the one just resolved; `daemon_auto_start=false` is written into
# Codex config by mcp-bootstrap so a client that omits the flag still does not
# start one.
exec "$codex_real" \
    --no-daemon \
    "$@"
