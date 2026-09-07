#!/usr/bin/env bash
# Vogt pod entrypoint — the supervisor of the merged container.
#
# Responsibilities (in order):
#   1. Optionally start sway headless in the background (if START_SWAY=1).
#   2. Optionally start and supervise vogt-core on loopback (if VOGT_CORE_URL
#      names a loopback address).
#   3. Exec the engine. PID 1's child = the engine, so signals propagate.
#
# Configurable via env (compose passes these through):
#   ENGINE_TOKEN              required
#   ENGINE_BIND               default 0.0.0.0:8910
#   START_SWAY                   "1" → spawn sway in background with WAYLAND_DISPLAY=wayland-1
#   GUI_STREAM_URL               passed through to the server; web UI iframes it
#   VOGT_CORE_URL                where the engine proxies /api/vogt, /mcp and
#                                /ui-legacy. Loopback → this script also *runs*
#                                the core there. Unset → no core, and the
#                                engine runs alone, as it shipped.
#   VOGT_DATA_DIR                the core's SQLite + backups (default from the
#                                image: /var/lib/vogt)
#
# ── Why supervision lives in this script ───────────────────────────────────
#
# The obvious answers are s6-overlay and supervisord, and both are wrong for
# *this* container, which is not a service image. It is a development pod: it
# carries agent CLIs, an Android SDK, and sway, it runs as `sprooty` with
# passwordless sudo, and its startup order is already an ordered script — sway
# needs XDG_RUNTIME_DIR, agent auth is validated before anything can use it.
#
# Adopting a supervision framework would mean:
#   * a second init system. The compose sets `init: true`, so tini is already
#     PID 1 and already reaps the orphans that agent sessions leave behind —
#     that is why it is there, and s6 wanting PID 1 would displace it.
#   * PID 1 as root. This image deliberately ends `USER sprooty`; s6-overlay's
#     supported shape is root-owned stage scripts.
#   * startup order in two places — some of it here, some of it in a service
#     directory — for a container with exactly two long-lived processes.
#
# What is actually needed is smaller than a framework: start one more
# background process, keep restarting it when it dies, and never let its death
# take the container with it. That last clause is the requirement, not a
# convenience: a missing core must not cost the running PTYs, and
# `api::readyz` deliberately reports the core's outage without failing
# readiness for the same reason. A supervisor that restarts the *container*
# when the core exits would undo both.
#
# The engine is still what `exec` replaces this shell with, so the container's
# lifetime is the engine's lifetime. That is the right coupling: the engine is
# the published port, and if it dies there is nothing to be ready *for*.
#
# One consequence, stated rather than hidden: on `docker stop`, tini signals
# the engine and the core is torn down with the namespace rather than asked
# politely. That is safe here — the core opens a SQLite connection per
# transaction and closes it (DEPLOYMENT.md), so at any instant there is
# usually no open write, and WAL makes the worst case crash-consistent rather
# than corrupt. It is the same deal sway has always had in this image.

set -euo pipefail

# Runtime-pinned agent CLIs. The image bakes a baseline; a deployment
# may name a different version per tool and it is installed here, into a
# versioned prefix under /opt/vogt/agent-clis that PATH prefers, before any
# session can start. Unset means the image's copy. A failed install is logged
# and the pod starts on whatever was current before — the pin is a request
# for a version, not a reason to refuse to boot — and `vogt-verify-agent-clis`
# below then judges the result against the manifest the installer wrote.
# Which tools, and which variable names each, is the image's table
# (`agent-clis.tools`: tool, package, binary, env var — `VOGT_CLAUDE_CODE_VERSION`
# and `VOGT_CODEX_VERSION` for the two every build carries).
agent_cli_tools="${VOGT_AGENT_CLI_TOOLS:-/usr/local/share/vogt/agent-clis.tools}"
if [[ -x /usr/local/bin/vogt-agent-cli-install && -r "$agent_cli_tools" ]]; then
    while IFS=$'\t' read -r tool _ _ var; do
        [[ -n "$tool" && -n "$var" && "$tool" != \#* ]] || continue
        if ! /usr/local/bin/vogt-agent-cli-install "$tool" "${!var:-image}"; then
            echo "agent-clis: ${tool} runtime pin (${var}=${!var:-}) was not applied; continuing on the previous version" >&2
        fi
    done < "$agent_cli_tools"
fi

# Agent CLIs are checked before any optional service starts: the active copy
# must be the one the manifest names, and a stale persisted-home copy must not
# be able to shadow it (exit 78 if it would).
if [[ -x /usr/local/bin/vogt-verify-agent-clis ]]; then
    /usr/local/bin/vogt-verify-agent-clis
fi

# Agent CLIs are deliberately not installed at container startup; the image
# carries neutral infrastructure tooling and optional agents can be added by
# the user. Service credentials for agent commands are brokered on demand by a
# pluggable *agent-auth helper*, so tokens never reach PID 1.
#
# `ENGINE_AGENT_AUTH_HELPER` names that helper. The image ships one reference
# implementation, `vogt-agent-auth` (Infisical), auto-selected when a
# secrets-manager machine identity is present so a deployment need not name it
# explicitly. With neither a helper nor an identity, agent auth is simply not
# configured and is skipped — a clean clone booting with just a token gets a
# working engine and plain shells.
agent_auth_required="${ENGINE_AGENT_AUTH_REQUIRED:-0}"
agent_auth_helper="${ENGINE_AGENT_AUTH_HELPER:-}"
if [[ -z "$agent_auth_helper" \
      && -n "${INFISICAL_CLIENT_ID:-}" && -n "${INFISICAL_CLIENT_SECRET:-}" ]]; then
    agent_auth_helper="vogt-agent-auth"
fi
if [[ -n "$agent_auth_helper" ]]; then
    echo "agent service auth available via ${agent_auth_helper##*/}"
    if [[ "$agent_auth_required" == "1" || "$agent_auth_required" == "true" ]]; then
        echo "validating required agent service auth"
        if ! "$agent_auth_helper" run -- true; then
            echo "agent service auth validation failed" >&2
            exit 1
        fi
    fi
elif [[ "$agent_auth_required" == "1" || "$agent_auth_required" == "true" ]]; then
    echo "agent service auth required but no ENGINE_AGENT_AUTH_HELPER is configured" >&2
    exit 1
else
    echo "agent auth not configured; skipping"
fi

if [[ "${START_SWAY:-0}" == "1" ]]; then
    # Headless sway needs XDG_RUNTIME_DIR. Selkies talks to it via WAYLAND_DISPLAY.
    export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp/runtime-$(id -u)}"
    mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
    export WAYLAND_DISPLAY=wayland-1
    sway --unsupervised >/tmp/sway.log 2>&1 &
    echo "sway started (PID $!)"
fi

# ── vogt-core ───────────────────────────────────────────────────────────────
#
# One value turns this on: VOGT_CORE_URL, which is *also* the value the engine
# reads to know where to proxy. Deriving the listen address from the proxy
# target rather than configuring them separately removes the failure this pair
# would otherwise invite — a front door pointed confidently at a port nothing
# is listening on, which reads to a client as "vogt-core did not answer" and
# to an operator as a mystery.
#
# A non-loopback URL is not an error and does not start anything: it means the
# core lives elsewhere (the two-service compose that §5.2 allows as a
# fallback), and this container is only its front door. A loopback URL means
# the core is ours to run — and the rule that the core binds loopback only and
# is never published is then enforced here, at the one place that can actually enforce
# it, rather than trusted to a comment in a compose file.

vogt_core_listen() {
    # Echo "host port" for a loopback URL; echo nothing for anything else.
    local url="$1" authority host port
    authority="${url#*://}"
    authority="${authority%%/*}"

    if [[ "$authority" == \[*\]* ]]; then
        host="${authority%%\]*}]"       # [::1]
        port="${authority##*\]}"
        port="${port#:}"
    else
        host="${authority%%:*}"
        port="${authority#"$host"}"
        port="${port#:}"
    fi

    if [[ -z "$port" ]]; then
        # Not a topology, a typo. The engine will proxy to this URL whatever
        # happens here, so carrying on means a front door pointed at a port
        # nothing is listening on — and because an absent core is deliberately
        # not fatal to readiness, the stack would come up, pass its
        # healthcheck, and answer 502 on every Vogt request. Fail at boot
        # instead, where somebody is reading.
        echo "vogt-core: VOGT_CORE_URL (${1}) has no port — refusing to start" \
             "a front door that can never reach a core" >&2
        exit 78  # EX_CONFIG
    fi

    case "$host" in
        127.0.0.1|localhost|'[::1]')
            # `vogt serve --host` wants a bare address; the brackets are URL
            # syntax, not part of the address.
            printf '%s %s\n' "${host//[\[\]]/}" "$port"
            ;;
        *)
            # A legitimate topology (§5.2's two-service fallback), and the one
            # case in this function that is not an error: the core lives
            # elsewhere and this container is only its front door. Said at
            # boot anyway, because "no core started" and "core started
            # elsewhere" look identical in a process list.
            echo "vogt-core: VOGT_CORE_URL names ${host}, not loopback —" \
                 "proxying to a core this container does not run" >&2
            return 1
            ;;
    esac
}

supervise_vogt_core() {
    local host="$1" port="$2"
    local backoff=1 started=0 uptime=0

    # `init` before every `serve`, because nothing else migrates. `serve` does
    # not, and there is no `vogt migrate` verb, so an image carrying a new
    # migration would otherwise come up, pass its healthcheck, and fail later
    # as a SQL error at whatever operation first touched the missing table —
    # the one deployment gap DEPLOYMENT.md records against this product, and
    # the manual step ("run `vogt init` in the container after a digest bump")
    # nobody remembers under pressure. Owning the container's startup is what
    # finally lets it be closed: `init` is idempotent and brings an existing
    # instance forward, so paying for it every boot costs a no-op.
    #
    # It is inside the loop rather than before it so that a failure — a volume
    # not yet writable, a migration that needs a moment — is retried on the
    # same backoff as everything else, instead of leaving the core dead until
    # somebody restarts the container.
    while :; do
        started=$SECONDS
        if ! vogt init; then
            echo "vogt-core: init failed — refusing to serve a store this" \
                 "build does not understand" >&2
        else
            echo "vogt-core: serving on ${host}:${port}"
            if vogt serve --host "$host" --port "$port"; then
                echo "vogt-core: exited cleanly" >&2
            else
                echo "vogt-core: exited with status $?" >&2
            fi
        fi
        uptime=$(( SECONDS - started ))

        # A core that ran for a while and then died is a different animal from
        # one that cannot start: reset the backoff for the first so a restart
        # is quick, and let it grow for the second so a crash-loop does not
        # bury the log line that says why.
        if (( uptime >= 60 )); then
            backoff=1
        fi
        echo "vogt-core: restarting in ${backoff}s (ran for ${uptime}s)" >&2
        sleep "$backoff"
        if (( backoff < 30 )); then
            backoff=$(( backoff * 2 ))
        fi
    done
}

if [[ -n "${VOGT_CORE_URL:-}" ]]; then
    if core_listen="$(vogt_core_listen "${VOGT_CORE_URL}")"; then
        read -r core_host core_port <<<"$core_listen"

        # Both halves must agree about where the workspace is (§6.3): vogt's
        # import root and the engine's workspace_root are the same tree, so a
        # session opened "for" a project opens in the path the registry
        # recorded. A mismatch does not fail anything — it just means every
        # collector reports nothing about a tree nobody edits, which renders as
        # an empty workspace rather than as "could not look". Say so loudly at
        # boot, because that is the only moment anyone is reading.
        workspace_root="${HOME:-}/Working"
        import_root="${VOGT_IMPORT_ROOT:-}"
        if [[ -n "$import_root" && "$import_root" != "$workspace_root"* ]]; then
            echo "vogt-core: WARNING VOGT_IMPORT_ROOT (${import_root}) is not" \
                 "under the engine's workspace root (${workspace_root});" \
                 "imported projects will be invisible to sessions (§6.3)" >&2
        fi

        supervise_vogt_core "$core_host" "$core_port" &
        echo "vogt-core: supervisor started (PID $!)"
    fi
else
    echo "vogt-core: VOGT_CORE_URL unset; running the engine alone"
fi

# `exec` so the server becomes the container's foreground process from here;
# SIGTERM from `docker stop` reaches it cleanly. Pass through any args.
exec /usr/local/bin/vogt-engine "$@"
