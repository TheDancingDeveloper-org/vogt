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
    # The container needs the TUN device passed through and CAP_NET_ADMIN;
    # see deploy/docker-compose.yml.
    sudo tailscaled --tun=userspace-networking >/var/log/tailscaled.log 2>&1 &
    sleep 1
    sudo tailscale up \
        --authkey="${TAILSCALE_AUTH_KEY}" \
        --hostname="${TAILSCALE_HOSTNAME:-mydevenv2}" \
        --accept-routes \
        --accept-dns=false || echo "tailscale up failed (continuing)"
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
