#!/usr/bin/env bash
# Vogt pod entrypoint — the supervisor of the merged container (NFR-D11).
#
# Responsibilities (in order):
#   1. Optionally join the tailnet (if TAILSCALE_AUTH_KEY is set).
#   2. Optionally start sway headless in the background (if START_SWAY=1).
#   3. Optionally start and supervise vogt-core on loopback (if VOGT_CORE_URL
#      names a loopback address).
#   4. Exec the engine. PID 1's child = the engine, so signals propagate.
#
# Configurable via env (compose passes these through):
#   MYDEVENV2_TOKEN              required
#   MYDEVENV2_BIND               default 0.0.0.0:8910
#   TAILSCALE_AUTH_KEY           if set, runs `tailscale up` with --hostname=mydevenv2
#   TAILSCALE_HOSTNAME           default mydevenv2
#   TAILSCALE_EXIT_NODE          if set, routes all egress via this exit node
#                                (kernel mode; requires /dev/net/tun + NET_ADMIN
#                                from the compose). LAN + tailnet stay direct.
#   MYDEVENV2_TAILSCALE_ACCEPT_DNS  "1" (default) enables Tailscale MagicDNS.
#                                Set to "0" only with an equivalent resolver.
#   START_SWAY                   "1" → spawn sway in background with WAYLAND_DISPLAY=wayland-1
#   GUI_STREAM_URL               passed through to the server; web UI iframes it
#   VOGT_CORE_URL                where the engine proxies /api/vogt, /mcp and
#                                /ui-legacy. Loopback → this script also *runs*
#                                the core there. Unset → no core, and the
#                                engine is MyDevEnv2 as it shipped (FR-E9).
#   VOGT_DATA_DIR                the core's SQLite + backups (default from the
#                                image: /var/lib/vogt)
#
# ── Why supervision lives in this script ───────────────────────────────────
#
# The obvious answers are s6-overlay and supervisord, and both are wrong for
# *this* container, which is not a service image. It is a development pod: it
# carries agent CLIs, an Android SDK, sway, and kernel-mode Tailscale, it runs
# as `sprooty` with passwordless sudo, and its startup order is already an
# ordered script — tailscaled must be up before `tailscale up`, sway needs
# XDG_RUNTIME_DIR, agent auth is validated before anything can use it.
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
# convenience: FR-E9 says a missing core must not cost the running PTYs, and
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
# transaction and closes it (DEPLOYMENT.md §8), so at any instant there is
# usually no open write, and WAL makes the worst case crash-consistent rather
# than corrupt. It is the same deal tailscaled and sway have always had in
# this image.

set -euo pipefail

if [[ -n "${TAILSCALE_AUTH_KEY:-}" ]]; then
    # Kernel networking (real TUN) so an exit node can transparently capture
    # this pod's egress. The compose must pass --device=/dev/net/tun and
    # cap_add NET_ADMIN; without them tailscaled falls back and the exit node
    # is a no-op. Logs go to /tmp because /var/log is root-owned (we run as
    # `sprooty`). iptables is present in the image for the NAT/filter rules.
    sudo -b sh -c 'tailscaled \
        --state=/var/lib/tailscale/tailscaled.state \
        --socket=/var/run/tailscale/tailscaled.sock \
        >/tmp/tailscaled.log 2>&1'
    # Wait for the socket to appear before `tailscale up` — racing it gives
    # the "doesn't appear to be running" error.
    for i in $(seq 1 30); do
        [[ -S /var/run/tailscale/tailscaled.sock ]] && break
        sleep 0.2
    done
    # Join the tailnet WITHOUT the exit node. This always establishes the
    # tailnet + accepted subnet routes (e.g. 192.168.0.0/23), which is the
    # lockout-proof control path — so a down exit node can never sever access.
    # Private service names (including the Cadastre MCP endpoint) are resolved
    # through tailnet DNS. Keep this enabled by default; disabling it without
    # an equivalent resolver makes native MCP clients fail before auth.
    tailscale_accept_dns="${MYDEVENV2_TAILSCALE_ACCEPT_DNS:-1}"
    sudo tailscale up \
        --authkey="${TAILSCALE_AUTH_KEY}" \
        --hostname="${TAILSCALE_HOSTNAME:-mydevenv2}" \
        --accept-routes \
        --accept-dns="${tailscale_accept_dns}" || echo "tailscale up failed (continuing)"

    # `tailscale up` may retain state from an earlier boot. Apply the setting
    # explicitly so a restarted container converges on the configured policy.
    sudo tailscale set \
        --accept-dns="${tailscale_accept_dns}" \
        || echo "tailscale accept-dns set failed (continuing)"

    # Apply the exit node as a separate, best-effort step. Split from `up` on
    # purpose: `up --exit-node=<down node>` fails the whole join, but `set`
    # degrades to direct egress if the node is unreachable and self-heals on a
    # later boot. --exit-node-allow-lan-access keeps the container's own LAN
    # (10.x, host services) reachable; accepted subnet routes like
    # 192.168.0.0/23 keep working because a /23 beats the exit node's 0.0.0.0/0.
    #
    # Important: tailscaled state is persisted, so a blank env var does NOT
    # implicitly clear a previously selected exit node. When the env is empty,
    # explicitly clear any old RouteAll/exit-node preference to restore direct
    # egress on boot.
    if [[ -n "${TAILSCALE_EXIT_NODE:-}" ]]; then
        sudo tailscale set \
            --exit-node="${TAILSCALE_EXIT_NODE}" \
            --exit-node-allow-lan-access \
            || echo "tailscale exit-node set failed (continuing without exit node)"
    else
        sudo tailscale set \
            --exit-node= \
            --exit-node-allow-lan-access=false \
            || echo "tailscale exit-node clear failed (continuing)"
    fi
fi

# Agent CLIs are deliberately not installed at container startup. The image
# carries neutral infrastructure tooling; optional agents can be installed by
# the user. A dedicated Infisical machine identity enables on-demand service
# auth through `mydevenv2-agent-auth` without exporting tokens to PID 1.
agent_auth_required="${MYDEVENV2_AGENT_AUTH_REQUIRED:-0}"
if [[ -n "${INFISICAL_CLIENT_ID:-}" && -n "${INFISICAL_CLIENT_SECRET:-}" ]]; then
    echo "agent service auth available via mydevenv2-agent-auth"
    if [[ "$agent_auth_required" == "1" || "$agent_auth_required" == "true" ]]; then
        echo "validating required agent service auth"
        if ! mydevenv2-agent-auth run -- true; then
            echo "agent service auth validation failed" >&2
            exit 1
        fi
    fi
else
    if [[ "$agent_auth_required" == "1" || "$agent_auth_required" == "true" ]]; then
        echo "agent service auth required but Infisical machine identity is not configured" >&2
        exit 1
    else
        echo "agent service auth unavailable: Infisical machine identity not configured" >&2
    fi
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
# the core is ours to run — and NFR-D11's "binds loopback only and is never
# published" is then enforced here, at the one place that can actually enforce
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
        echo "vogt-core: VOGT_CORE_URL has no port; not starting a core" >&2
        return 1
    fi

    case "$host" in
        127.0.0.1|localhost|'[::1]')
            # `vogt serve --host` wants a bare address; the brackets are URL
            # syntax, not part of the address.
            printf '%s %s\n' "${host//[\[\]]/}" "$port"
            ;;
        *)
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
    # the one deployment gap DEPLOYMENT.md §5 records against this product, and
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

        # Both halves must agree about where the estate is (§6.3): vogt's
        # import root and the engine's workspace_root are the same tree, so a
        # session opened "for" a project opens in the path the registry
        # recorded. A mismatch does not fail anything — it just means every
        # collector reports nothing about a tree nobody edits, which renders as
        # an empty estate rather than as "could not look". Say so loudly at
        # boot, because that is the only moment anyone is reading.
        workspace_root="${HOME:-/home/sprooty}/Working"
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
    echo "vogt-core: VOGT_CORE_URL unset; running the engine alone (FR-E9)"
fi

# `exec` so the server becomes the container's foreground process from here;
# SIGTERM from `docker stop` reaches it cleanly. Pass through any args.
exec /usr/local/bin/mydevenv2-server "$@"
