# MyDevEnv2 — Tooling Baseline

Captured from review of `MyDevEnv/Dockerfile.server` (v1). The pod provides a neutral development baseline for builds under `~/Working/Active/apps/`. Codex and Claude are optional clients and are not installed during container bootstrap.

## Base OS

Ubuntu 26.04 (or newer LTS). Same as v1 — proven to work with all the toolchains below.

## Core system + dev utilities

`ca-certificates`, `curl`, `wget`, `gnupg`, `lsb-release`,
`git`, `git-lfs`, `vim`, `nano`, `less`, `man-db`, `sudo`,
`build-essential`, `pkg-config`, `cmake`, `clang`, `lld`, `nasm`,
`jq`, `ripgrep`, `fd-find`, `bat`, `rsync`,
`openssh-client`, `openssh-server`, `iputils-ping`, `netcat-openbsd`, `dnsutils`, `xdg-utils`,
`htop`, `tree`, `file`, `unzip`, `zip`,
`musl-tools`, `gcc-mingw-w64-x86-64`, `gcc-aarch64-linux-gnu`, `g++-aarch64-linux-gnu`,
`libssl-dev`, `libclang-dev`, `protobuf-compiler`

Notably **not** carried over from v1: `tmux` (no longer needed — server-owned PTYs replace it).

## Language toolchains

### Rust (primary)

- `rustup` stable channel
- Components: `rustfmt`, `clippy`
- Cross-compile targets:
  - `x86_64-unknown-linux-musl` (static Linux binaries)
  - `aarch64-unknown-linux-gnu` (ARM Linux)
  - `x86_64-pc-windows-gnu` (Windows cross-compile via mingw)
- Cargo tools: `cargo-deb`, `cargo-zigbuild`, `cargo-xwin`, `cargo-watch`
- `sccache` v0.10.0+ from GitHub releases (NOT apt — apt package lacks Redis support)
- `SCCACHE_REDIS=redis://100.92.54.45:6380` (Node B Redis instance)

### Python

- `python3`, `python3-pip`, `python3-venv`, `python3-dev`
- Tools via pip: `uv`, `ruff`

### Node.js

- Node 22 (from NodeSource)
- `pnpm` global install

## Container + cloud tools

- Docker CLI + compose plugin (DooD pattern — talks to host daemon via socket mount; container itself doesn't run dockerd)
- `gh` (GitHub CLI)
- `rclone`
- `infisical` (CLI for secret retrieval; already used in CI and at runtime)
- `tailscale` (joins the tailnet at container start)

## Auth keys / secrets needed at pod startup

| Secret | Infisical location | Purpose |
|---|---|---|
| `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` | `apps` project, env `prod` | Auto-join the pod to the tailnet on startup |
| `MYDEVENV2_TOKEN` | `apps` project, env `prod` | Bearer token for MyDevEnv2 server API auth |
| `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` | `apps` project, env `prod` | Optional read-only Universal Auth identity for agent service access |
| `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET` | `apps` project, env `prod` | Secret for the optional agent identity |
| Other app-specific secrets | Infisical `apps` / `cicd` / `infrastructure` | Fetched on demand by `mydevenv2-agent-auth` |

MyDevEnv2 fetches service credentials on demand rather than exporting them at
container startup:

```bash
mydevenv2-agent-auth check
mydevenv2-agent-auth run -- gh api user
mydevenv2-agent-auth shell
```

The machine identity must have read-only access to the `cicd`, `infrastructure`,
and `apps` Infisical projects. The helper uses the direct tailnet endpoint
`http://100.92.54.45:8400` to avoid the browser-facing Caddy auth gate.

## Things v1 had that v2 should reconsider

- **The `xdg-open` shim** — was a workaround for VS Code Remote's browser forwarding. With no code-server in v2, this shim isn't needed. URLs printed in a terminal can be click-handled by the web UI itself (xterm.js link addon → POST to server → server can launch in Sway-Chromium or just copy to client clipboard).
- **The `/home/sprooty → /workspace` symlink** — existed to make VS Code Remote SSH paths line up. Reconsider for v2: simpler to bind-mount the workspace at `/home/sprooty/Working` directly so paths match the host exactly. No symlink wrangling.
- **SSH server on port 2223** — v1 exposed this for emergency / IDE-less access, but v2 currently does not start or expose `sshd`. Prefer the server-owned PTY sessions and the host workspace bind mount unless an explicit emergency SSH path is added later.

## New things v2 needs

- **Sway** (headless Wayland compositor) + dependencies for GUI app rendering
- **Selkies-GStreamer** for WebRTC streaming of the compositor
- **Chromium or Firefox** for in-pod browser-based testing of web apps
- A consistent way to launch GUI apps from a terminal so the output stream picks them up automatically (probably `swaymsg exec` or a thin wrapper)

For the Android emulator KVM VM (separate from the pod), tooling is its own concern — Android Studio + SDK + emulator image, installed inside that VM, not the dev pod.
