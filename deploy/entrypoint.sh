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

# ─── User CLI bootstrap ─────────────────────────────────────────────────────
# /home/sprooty is bind-mounted from the host, which shadows anything we
# installed into it during the image build. Install the user-level CLIs the
# pod always wants here, on every boot, idempotently. Each install is
# best-effort and runs in the background so it doesn't block the server.
bootstrap_user_clis() {
    local log=/tmp/mydevenv2-bootstrap.log
    : >"$log"
    {
        echo "[bootstrap] starting $(date -Is)"
        mkdir -p /home/sprooty/.npm-global /home/sprooty/.local/bin

        if ! command -v codex >/dev/null 2>&1 || ! command -v theclawbay >/dev/null 2>&1; then
            echo "[bootstrap] npm install -g @openai/codex theclawbay"
            npm install -g @openai/codex theclawbay \
                && echo "[bootstrap] npm install ok" \
                || echo "[bootstrap] npm install failed (continuing)"
        else
            echo "[bootstrap] codex + theclawbay already present"
        fi

        if ! command -v claude >/dev/null 2>&1; then
            echo "[bootstrap] installing claude CLI"
            curl -fsSL https://claude.ai/install.sh | bash \
                && echo "[bootstrap] claude install ok" \
                || echo "[bootstrap] claude install failed (continuing)"
        else
            echo "[bootstrap] claude already present"
        fi

        echo "[bootstrap] done $(date -Is)"
    } >>"$log" 2>&1
}
bootstrap_user_clis &

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
