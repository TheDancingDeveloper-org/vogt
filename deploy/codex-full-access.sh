#!/usr/bin/env bash
# MyDevEnv2 is itself the trusted isolation boundary. Run Codex without its
# nested filesystem/network sandbox so it can work across the complete shared
# workspace tree (the pod's $HOME/Working) and use the pod's authenticated
# service access.
set -euo pipefail

exec /usr/local/libexec/codex-real \
    --dangerously-bypass-approvals-and-sandbox \
    "$@"
