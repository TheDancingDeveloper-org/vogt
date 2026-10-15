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

# A runtime copy must be the fork (WI-1146). The fork is what reads the
# gitguard hook in /etc/codex/requirements.toml; stock Codex ignores the hook
# table and would run with nothing guarding the checkout. The installer writes
# `source` (`<repository>@<commit>`) only for a fork release, so a prefix
# without it — an npm copy an older image flipped to — is refused rather than
# run. When the image's tools table is readable, the repository must be the
# fork it names.
#
# This is an accident guard, not a security boundary: `codex/current` and its
# `source` file live under /opt/vogt/agent-clis, which the session user owns,
# so a session can write a fork-shaped `source` beside any binary. It stops a
# stale or mistaken pin (an npm copy an older image flipped to) from running
# unguarded; it does not stop a session that sets out to. The gitguard hook
# itself does not depend on this file: it runs the root-owned
# /usr/local/bin/klaudia by absolute path, and the fork reads the root-owned
# /etc/codex/requirements.toml (WI-1146).
prefix="${VOGT_AGENT_CLI_ROOT:-/opt/vogt/agent-clis}/codex/current"
runtime="$prefix/bin/codex"
if [[ -x "$runtime" ]]; then
    provenance="$(head -n 1 "$prefix/source" 2>/dev/null || true)"
    fork="$(awk -F'\t' '$1 == "codex" && $5 == "github-release" { print $2; exit }' \
        /usr/local/share/vogt/agent-clis.tools 2>/dev/null || true)"
    if [[ ! "$provenance" =~ ^[^@[:space:]]+@[0-9a-f]{40}$ ]] \
            || [[ -n "$fork" && "${provenance%@*}" != "$fork" ]]; then
        echo "codex: refusing $runtime: it is not a Codex fork release (source '${provenance:-none}'), so the gitguard hook would not run. Pin VOGT_CODEX_SHA, or set it to 'image' and restart" >&2
        exit 78
    fi
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
