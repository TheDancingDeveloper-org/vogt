# Vogt — Deployment & Network Topologies (draft v0.1)

Status: **draft, pre-implementation**. Companion to `DESIGN.md` §7 and
`SCHEMA.md`. Shaped heavily by cadastre's deployment history — see §4 for
the specific lessons encoded here.

## 1. Process model

One server process serves **everything on one port**:

```
vogt serve
  ├── /            GUI (static SPA)
  ├── /api/...     REST (FastAPI, OpenAPI at /api/docs)
  ├── /mcp         MCP streamable HTTP transport
  ├── /health/live, /health/ready, /version
  └── collector scheduler (in-process background sweeps)
```

This is a deliberate inversion of cadastre's production shape, where the
MCP port served *only* `/mcp` and even `/health/ready` and `/version`
answered `-32004` — which broke standard probes and made "is it up?"
require an MCP client. Here, **any port that serves MCP also serves plain
HTTP health and version**, so curl, compose healthchecks, and uptime
monitors work unmodified.

Split-mode (separate MCP process/port) is not a v1 topology. The door stays
open — transports are thin adapters — but it is not built, documented, or
defaulted.

## 2. Topologies

### 2.1 Topology A — local single-user (dev box / laptop)

```
agent (stdio MCP) ──spawn──> vogt-mcp (local, same data-dir)
browser ──────────http────> 127.0.0.1:<port>  (serve, loopback only)
vogt CLI ───────────> data-dir directly (no server needed)
```

- `serve` binds loopback by default; no TLS, no auth required for loopback
  (still audited — principal `local:<os-user>`).
- Local stdio MCP talks to the same data directory the CLI uses; the server
  does not need to be running for stdio or CLI use (SQLite + WAL, single
  writer discipline enforced by the application layer).
- Fully offline-capable: core collectors (`git-local`, `source-markers`,
  `dep-refs`, and the on-demand `contract-checker`) need no network, and
  run only over registered projects. Outbound to `api.github.com` occurs
  only if the optional GitHub adapter is configured.

### 2.2 Topology B — tailnet server (primary self-hosted target)

```
                    tailnet (WireGuard)
agents / browsers ────https────> host:PORT ──> [ caddy or tailscale serve: TLS ]
                                                └──> vogt serve :PORT
                                                       └── volume: /var/lib/vogt
GitHub  <───outbound only─── collectors (optional adapter; no inbound
                                          webhooks required)
```

- Single compose stack: the app container + a TLS front (Caddy sidecar or
  `tailscale serve`); one published port, path-routed as in §1.
- Exposure default: **tailnet/LAN only**. No public ingress in v1.
- Clients connect one of two ways (mirroring cadastre's proven pair):
  1. **Native streamable HTTP** to `https://<host>:<port>/mcp` with a
     bearer token — preferred wherever the agent product supports it.
  2. **`vogt-mcp-remote` stdio bridge** for agent products that can
     only spawn local processes. Config via `VOGT_URL` and
     `VOGT_TOKEN_FILE` (token in a file, never argv or URL).
- Webhooks are an optional later enhancement; the baseline is outbound
  polling sweeps, so the server works behind NAT with zero inbound rules
  beyond the tailnet.

### 2.3 Topology C — deferred (explicit non-goals for v1)

Public internet exposure, multi-node/HA, hosted multi-tenant SaaS, and
split MCP/API processes. Listed so their absence is a decision, not an
omission.

## 3. Identity & auth per topology

| Path | AuthN | Principal derivation |
|---|---|---|
| Loopback (A) | none | `local:<os-user>` |
| HTTPS + bearer (B) | token bound to an Actor | from token — **never** from a request field |
| Trusted proxy (B, optional) | proxy-injected identity header | from header, only when proxy mode explicitly enabled |

Scopes ride on the token (`read`, `work.write`, `project.write`,
`admin`, `writeback`). Scopes are instance-wide in v1 — a token holding
`work.write` can write to every project (a known limitation, see
`REQUIREMENTS.md` §3). MCP `tools/list` is filtered to the caller's
scopes; write tools additionally require the server to be started with
writes enabled. Both allow and deny decisions are audited.

## 4. Configuration & bootstrap rules (lessons encoded)

Cadastre's `:18081` incident: a retired default port shipped in a
downstream image in seven places, and the bootstrap script checked that a
config key *existed* rather than that its *value* was right, so clients
kept a dead URL forever. Additionally a health check pinned an MCP
protocol version the server refused. Therefore:

1. **No default host/port anywhere.** Not in code, docs, images, or
   examples. `serve` requires `--port` (or config); docs use `<port>`
   placeholders. A default can rot in a downstream image; a required value
   cannot.
2. **Bootstrap reconciles values, not key existence.** Any client-setup
   script compares the configured endpoint against the intended one and
   rewrites on mismatch; "key present" is never success.
3. **One connection document.** A single generated `CONNECTING.md` (from
   the running server: `GET /connection-info`) states the canonical URL,
   `/mcp` path, and supported MCP protocol versions. Client configs are
   derived from it, never hand-copied per client.
4. **Health checks are protocol-version-agnostic.** Probes hit
   `/health/ready` (plain HTTP). Nothing outside a real MCP client sends
   `initialize`, and nothing pins a protocol version.
5. **Version skew warns, never blocks.** Bridge↔server version mismatch is
   one stderr line (stdout is MCP framing), startup always proceeds.
6. Config schema (pydantic) is the single source of truth; compose files,
   example configs, and docs are generated from it, and CI fails on drift.

## 5. Storage, backup, upgrade

- One named volume: `/var/lib/vogt` (both SQLite files + backups).
- `vogt backup` produces a consistent snapshot (SQLite backup API,
  both stores + a manifest with schema versions); `restore` verifies the
  manifest before touching anything.
- Upgrade path: pull new image → `migrate` runs forward-only migrations
  under `migration_lock` at startup → `/health/ready` gates traffic until
  migration completes.
- Images: `ghcr.io`, SBOM + signed, tag-triggered releases only
  (`DESIGN.md` §8.1 — docs pushes can never publish an image).
