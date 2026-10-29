# Stack image: Python core vs Rust core (WI-1078)

Measured 2026-10-19 on a 24-CPU host with Docker 29.9.1.

- **Rust:** `engine/Dockerfile` at `origin/rust-core` 5c16ec62.
- **Python:** `engine/Dockerfile` at `origin/main` f45128ea.

Both were built with `docker buildx build -f engine/Dockerfile --build-arg
INSTALL_AI_CLIENTS=false` (the default lean pod base). Each was run with the
default entrypoint and the core settings `deploy/stack.compose.yml` sets:
`VOGT_CORE_URL=http://127.0.0.1:8000` and a core token file.

| Metric | Python core | Rust core | Change |
| --- | --- | --- | --- |
| Image size (`docker image inspect .Size`) | 7,620,979,615 B (7.62 GB) | 7,536,275,581 B (7.54 GB) | −84.7 MB (−1.1 %) |
| `docker run` → core `/health/ready` 200, run 1 | 16,655 ms | 14,538 ms | |
| run 2 | 16,855 ms | 16,748 ms | |
| run 3 | 30,209 ms | 19,778 ms | |
| median | 16,855 ms | 16,748 ms | ≈ same |
| Core process RSS, 60 s idle, run 1 / 2 / 3 | 81.1 / 79.7 / 81.4 MB | 18.3 / 22.7 / 18.5 MB | ≈ −77 % |
| Container memory (`docker stats`), 60 s idle | 96–123 MiB | 23–73 MiB | |

Notes:

- **Cold start is dominated by the entrypoint, not the core.** The
  entrypoint first runs `vogt-agent-cli-install`, the cargo cache setup and
  the agent-auth check, and only then starts the core. That is why the two
  medians match; the Rust core's own boot is a small part of the total.
  Run 3's Python outlier (30 s) was not reproduced.
- **Image size barely moves.** The pod base and the agent tooling make up
  most of the image. Dropping the venv saves about 85 MB.
- **RSS** is the `ps` RSS of `vogt serve` (Python, under `/opt/vogt/.venv`)
  or `vogt-core serve` (Rust).

## Deployment files

The port (8000), the bind host (127.0.0.1) and the health URL
(`/health/ready`) are identical in both images. `deploy/stack.compose.yml`
and `docs/DEPLOYMENT.md` therefore need no change.
