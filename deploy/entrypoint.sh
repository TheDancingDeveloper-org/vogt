#!/usr/bin/env bash
# MyDevEnv2 pod entrypoint.
#
# Responsibilities (in order):
#   1. Optionally join the tailnet (if TAILSCALE_AUTH_KEY is set).
#   2. Optionally start sway headless in the background (if START_SWAY=1).
#   3. Exec the MyDevEnv2 server. PID 1 = the server, so signals propagate.
#
# Configurable via env (compose passes these through):
#   MYDEVENV2_TOKEN              required
#   MYDEVENV2_BIND               default 0.0.0.0:8910
#   TAILSCALE_AUTH_KEY           if set, runs `tailscale up` with --hostname=mydevenv2
#   TAILSCALE_HOSTNAME           default mydevenv2
#   START_SWAY                   "1" → spawn sway in background with WAYLAND_DISPLAY=wayland-1
#   GUI_STREAM_URL               passed through to the server; web UI iframes it

set -euo pipefail

if [[ -n "${TAILSCALE_AUTH_KEY:-}" ]]; then
    # Userspace networking — no TUN device passthrough required from the host.
    # Logs go to /tmp because /var/log is root-owned (we run as `sprooty`).
    sudo -b sh -c 'tailscaled \
        --tun=userspace-networking \
        --state=/var/lib/tailscale/tailscaled.state \
        --socket=/var/run/tailscale/tailscaled.sock \
        >/tmp/tailscaled.log 2>&1'
    # Wait for the socket to appear before `tailscale up` — racing it gives
    # the "doesn't appear to be running" error.
    for i in $(seq 1 30); do
        [[ -S /var/run/tailscale/tailscaled.sock ]] && break
        sleep 0.2
    done
    sudo tailscale up \
        --authkey="${TAILSCALE_AUTH_KEY}" \
        --hostname="${TAILSCALE_HOSTNAME:-mydevenv2}" \
        --accept-routes \
        --accept-dns=false || echo "tailscale up failed (continuing)"
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

# `exec` so the server becomes PID 1 from here; SIGTERM from `docker stop`
# reaches it cleanly. Pass through any args.
exec /usr/local/bin/mydevenv2-server "$@"
