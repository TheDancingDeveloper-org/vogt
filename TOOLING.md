# MyDevEnv2 — Tooling Baseline

Captured from review of `MyDevEnv/Dockerfile.server` (v1). The new pod must provide at least this tooling so existing workflows (Claude Code, Codex, builds for all the apps under `~/Working/Active/apps/`) work without reinstalling on every spin-up.

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
| `MYDEVENV_TAILSCALE_AUTH_KEY` | `apps` project, env `prod` | Auto-join the pod to the tailnet on startup |
| `MYDEVENV_TOKEN` | (new — generate per deployment) | Bearer token for MyDevEnv2 server API auth |
| Other app-specific secrets | Infisical `apps` / `cicd` / `infrastructure` | Fetched on demand via `infisical secrets get` inside terminals |

Fetch the Tailscale key at pod start:

```bash
infisical secrets get MYDEVENV_TAILSCALE_AUTH_KEY \
    --domain https://se.sprooty.com \
    --projectId 76b1ebe1-3656-4cef-952c-30d5d489c6e7 \
    --env prod --plain
```

Then `tailscale up --authkey="$KEY" --hostname=mydevenv2 --accept-routes`.

## Things v1 had that v2 should reconsider

- **The `xdg-open` shim** — was a workaround for VS Code Remote's browser forwarding. With no code-server in v2, this shim isn't needed. URLs printed in a terminal can be click-handled by the web UI itself (xterm.js link addon → POST to server → server can launch in Sway-Chromium or just copy to client clipboard).
- **The `/home/sprooty → /workspace` symlink** — existed to make VS Code Remote SSH paths line up. Reconsider for v2: simpler to bind-mount the workspace at `/home/sprooty/Working` directly so paths match the host exactly. No symlink wrangling.
- **SSH server on port 2223** — useful for emergency / IDE-less access. Keep. Same config (key-only, no passwords, no X11).

## New things v2 needs

- **Sway** (headless Wayland compositor) + dependencies for GUI app rendering
- **Selkies-GStreamer** for WebRTC streaming of the compositor
- **Chromium or Firefox** for in-pod browser-based testing of web apps
- A consistent way to launch GUI apps from a terminal so the output stream picks them up automatically (probably `swaymsg exec` or a thin wrapper)

For the Android emulator KVM VM (separate from the pod), tooling is its own concern — Android Studio + SDK + emulator image, installed inside that VM, not the dev pod.
