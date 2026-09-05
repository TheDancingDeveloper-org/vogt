#!/usr/bin/env bash
# MyDevEnv2 is itself the trusted isolation boundary. Run Codex without its
# nested filesystem/network sandbox so it can work across the complete shared
# workspace tree (the pod's $HOME/Working) and use the pod's authenticated
# service access.
#
# Which Codex runs is the runtime pin's decision (#590): when
# `vogt-agent-cli-install` has flipped `codex/current` under the agent CLI
# root, that copy is preferred; otherwise the image's own, installed under
# libexec by engine/Dockerfile. Codex gets no PATH-first `bin/` link of its
# own for exactly this reason — this wrapper is the single entry point, so the
# bypass flags below apply to whichever copy is active.
set -euo pipefail

runtime="${VOGT_AGENT_CLI_ROOT:-/opt/vogt/agent-clis}/codex/current/bin/codex"
if [[ -x "$runtime" ]]; then
    codex_real="$runtime"
else
    codex_real=/usr/local/libexec/codex-real
fi

exec "$codex_real" \
    --dangerously-bypass-approvals-and-sandbox \
    "$@"
