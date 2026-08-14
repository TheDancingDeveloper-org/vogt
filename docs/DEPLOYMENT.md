# Vogt — Deployment & Network Topologies (v0.2, revision r5)

Status: **built** (M4; reconciled against the delivered v1 on 2026-08-12).
Companion to `DESIGN.md` §7 and `SCHEMA.md`. Shaped heavily by cadastre's
deployment history — see §4 for the specific lessons encoded here.

The compose file this document describes is committed at
`deploy/personal-vogt.compose.yml` and copied to `indexarr/ops` at
`personal/vogt/docker-compose.yml`. §4.2 and §4.3 describe client-side
tooling that is **not built** — see the note in each.

**r4 (2026-08-12): the target deployment state is named.** Vogt runs as a
**Docker Compose stack on Node B (`winrarhost`), deployed by Komodo** from
the `indexarr/ops` GitOps repository, from an image published to GHCR by
tag-triggered GitHub Actions. This replaces the abstract "tailnet server"
of r1 with the concrete estate it actually deploys into. §2.2 is the
target; §2.1 remains the local development shape.

## 1. Process model

One server process serves **everything on one port**:

```
vogt serve
  ├── /            redirect to /ui/
  ├── /ui/...      GUI (static assets; every answer comes from /api)
  ├── /api/...     REST (FastAPI; OpenAPI at /openapi.json, UI at /docs)
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

It is also one *container* and one published port, where cadastre's stack
runs `cadastre` and `cadastre-mcp` as two services on two ports. One
process is the whole reason a compose healthcheck can speak for the MCP
surface too.

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

- `serve` binds loopback; no TLS, no auth required for loopback
  (still audited — principal `local:<os-user>`).
- Local stdio MCP talks to the same data directory the CLI uses; the server
  does not need to be running for stdio or CLI use (SQLite + WAL, single
  writer discipline enforced by the application layer).
- Fully offline-capable: core collectors (`git-local`, `source-markers`,
  `dep-refs`, and the on-demand `contract-checker`) need no network, and
  run only over registered projects. Outbound to `api.github.com` occurs
  only if the optional GitHub adapter is configured.

This is the M0–M3 working shape, and it stays supported after M4 — it is
how the product runs for anyone who is not this estate.

### 2.2 Topology B — Node B compose stack via Komodo (**the target**)

```
                    tailnet (WireGuard)
agents / browsers ──https──> 100.92.54.45:<port>   (Tailscale address only)
                                    │
                                    └──> vogt (single container, single port)
                                           ├── TLS terminated in-process
                                           │   (host Tailscale LE cert, ro)
                                           └── volume: /var/lib/vogt
GitHub  <───outbound only─── collectors (optional adapter; no inbound
                                          webhooks required)
```

| Fact | Value |
|---|---|
| Host | Node B, `winrarhost` — TS `100.92.54.45`, LAN `192.168.1.75` |
| Komodo server (periphery) | `Local` |
| Komodo stack | `vogt` (the ops directory is `personal/vogt`; the stack name is not prefixed) |
| Desired state | `indexarr/ops` → `personal/vogt/docker-compose.yml` |
| Image | `ghcr.io/thedancingdeveloper-org/vogt`, digest-pinned |
| Exposure | tailnet only — bound to the Tailscale address, never `0.0.0.0` |
| TLS name | `winrarhost.tailc7d3c.ts.net` (Tailscale-issued Let's Encrypt) |
| App data | named volume `vogt-komodo-data` → `/var/lib/vogt` |
| Operator material | `/mnt/2tnvme/docker/volumes/vogt/{tls,auth}`, mounted `:ro` |
| Estate | `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` → `/home/sprooty/Working`, writable, as `VOGT_UID` (1000) |
| Forge credential | `VOGT_GITHUB_TOKEN_FILE` → `/run/secrets/github_token` (see 2.2a) |

#### 2.2a Delivering the GitHub token to a read-only container

The forge adapter is off unless `github_token_file` names a readable file, and
off is silent by design (NFR-PO1): forge subjects read as *not collected*
rather than absent, so a stack deployed without the token looks healthy and
answers `notifications` with an empty list forever. This happened — r6 shipped
notifications and import, and both sat inert on Node B until 2026-08-14.

The token cannot arrive as an environment variable (FR-S7), and the obvious
Compose shape for turning one into a file — `secrets: { github_token: {
environment: VOGT_GITHUB_TOKEN } }` — is refused for this service:

```
cannot create secret "vogt_github_token" in read-only service vogt:
`file` is the sole supported option
```

Docker materialises an environment-sourced secret by writing into the
container, which `read_only: true` forbids. The read-only root is worth more
than the tidier compose, so the stack's `pre_deploy` hook extracts the one
variable it needs from the `.env` Komodo writes beside the compose file:

```sh
umask 077
sed -n 's/^VOGT_GITHUB_TOKEN=//p' .env > github-token
test -s github-token
chown 1000:0 github-token   # Compose ignores secret uid/mode outside swarm,
chmod 640 github-token      # and a bind mount keeps the host's ownership
```

and the compose references it as `secrets: { github_token: { file:
./github-token } }`. The value never appears on a command line, and the stack
stays reproducible from the ops repository plus Komodo's stack environment.

Upstream of the copy is Infisical `apps/prod/HOMELAB_VOGT_GITHUB_TOKEN`.
The copy is point-in-time: rotating upstream does not reach the stack until
someone redeploys it.

`personal/` rather than `prod/` because this is home-homelab infrastructure
on Node B, matching `personal/cadastre` — the ops repo's top-level
directory *is* the Komodo stack-name prefix.

### 2.1a The estate mount

Vogt shares MyDevEnv2's workspace directory rather than getting a copy of
it. A copy would mean every drift proposal is about a tree nobody edits,
which is worse than not looking: the answers would be confidently wrong
instead of visibly absent.

Two properties of that mount are load-bearing.

**The in-container path is identical to the host-side development path.**
A project's `root_path` is stored absolute (FR-P5) and collectors read it
verbatim — there is no path translation, and adding one would be a second
source of truth about where a project lives. Register
`/home/sprooty/Working/Active/apps/vogt` from inside MyDevEnv2 and this
container must find it at exactly that path. If it does not, every source
collector reports nothing, and *nothing found renders as an empty view, not
as "could not look"* — the FR-O4 failure arriving through the filesystem.

**It is writable.** Collectors only ever read, so `:ro` is tempting — and
wrong, because `project create` scaffolds a contract-compliant skeleton on
disk (FR-G11). A read-only mount removes exactly one operation, and removes
it as a runtime permission error rather than a clear refusal.

That matters more than the one operation. Splitting capability by topology
is what this product is built not to do: one operation registry, the same
answers over CLI, REST and MCP, on every deployment. A mount that makes
`project create` work from a laptop and fail on the server is the parity
rule (FR-A1) broken by infrastructure instead of by code, where no parity
test can see it.

Write access is bounded by the mechanisms designed for it rather than by the
filesystem: `project.write` is its own token scope (FR-S3), writes are
double-gated (FR-S4), `serve --read-only` refuses every write whatever scope
a token holds, and each one lands in the audit log with the reason its
author gave.

**The container runs as uid 1000 — and the uid is a deploy-time value.**

Which uid is right depends on who owns the estate being observed, which only
the host knows. So it lives in the compose file as `${VOGT_UID:-1000}`, and
changing it must never require a release. That matters more the moment
anyone else self-hosts this: their uid is not ours, and an image that has
decided the answer is an image they cannot run.

The obstacle is Docker rather than policy. A fresh named volume is seeded
from the ownership of `/var/lib/vogt` *in the image*, so a data directory
owned by one specific uid is unwritable by any other — and it fails on
volume *recreation*, which usually means during a restore. The image
therefore owns that directory `root:0` and makes it group-writable, and runs
with gid 0. Deployers set `user: "<their-uid>:0"` and rebuild nothing. The
gid is plumbing; the uid is the only part that has to match the host.

On Node B that uid is 1000, because MyDevEnv2 owns the workspace as 1000.

It began as `10001` with `group_add: [1000]`, on the reasoning that a service
should not run as a human's uid. The first sweep settled it. The workspace is
written with umask 077 — mode 700/600 throughout — and a supplementary group
buys nothing against `rwx------`: `git-local` failed with `Permission denied`
on `.git`, and every other collector returned nothing. Nothing is what an
empty view looks like, so the instance would have reported a clean estate it
had never read (FR-O4, arriving through the filesystem).

The alternative was `chmod -R g+rX` across the whole estate — a wider and far
more permanent change to a developer's tree, made to preserve a uid boundary
the shared bind mount had already dissolved. A container that can read every
file you own is not contained by running under a different number.

What actually does the containing is unchanged, and none of it is the uid: a
`read_only` root filesystem, `cap_drop: [ALL]`, `no-new-privileges`,
tailnet-only exposure, scoped bearer tokens, double-gated writes, and an
audit row carrying a reason for every one.

**Exposure is a tailnet allocation, not an ingress decision.** The
published port binds `100.92.54.45`, so it publishes nothing to the LAN or
the internet. There is no public DNS record, no Cloudflare entry, and **no
block in Node B's Caddyfile** — Node B's Caddy is host infra (a plain
compose project at `/mnt/2tnvme/docker/volumes/caddyv2/`, not a Komodo
stack), and a tailnet-only service has no reason to couple to it. This
revises NFR-D6: TLS is terminated **in-process** from the host's
Tailscale-issued certificate, bind-mounted read-only, rather than by a
fronting Caddy or `tailscale serve`. `serve` therefore needs `--tls-cert`
/ `--tls-key` at M4.

**Port allocation.** One port, allocated when the stack is created and
**verified free on Node B at that moment** — not inherited from this
document. Cadastre holds `18090` (HTTP API) and `18092` (MCP) in the same
block. **Allocated: `18094`**, verified free on Node B before the compose
file was committed (`18095` was already in use). It is a **default in the
compose file**, overridable from the Komodo stack environment — see §4.1,
which is the rule this replaces.

**Container shape** (following the cadastre stack, which is the hardened
precedent on this host):

- non-root (`user: "${VOGT_UID:-1000}:0"`), `read_only: true` root filesystem,
  `tmpfs` for `/tmp`, `security_opt: [no-new-privileges:true]`,
  `cap_drop: [ALL]`.
- Stateful data on a **named volume**; operator-owned material (TLS key
  pair, token file) as **absolute** bind mounts, read-only. Never a
  `./relative` bind mount for state — Komodo clones stack directories to
  fresh paths on every deploy, so a relative mount silently points at a
  new empty directory.
- TLS key mode `0640 root:0`, so only this container can read
  it (FR-S7: tokens by file reference, never argv or URL).
- `healthcheck` hits `/health/ready` over plain HTTP (§4.4).

**Clients** connect one of two ways (mirroring cadastre's proven pair):

1. **Native streamable HTTP** to `https://<host>:<port>/mcp` with a bearer
   token — preferred wherever the agent product supports it.
2. **`vogt-mcp-remote` stdio bridge** for agent products that can only
   spawn local processes. Config via `VOGT_URL` and `VOGT_TOKEN_FILE`
   (token in a file, never argv or URL).

Webhooks are an optional later enhancement; the baseline is outbound
polling sweeps, so the server works behind NAT with zero inbound rules
beyond the tailnet.

### 2.3 From commit to running container

```text
push to main ──> `build.yml`     → image tagged sha-<commit>, signed  (r5)
tag v*       ──> `release.yml`   → semver + latest + wheel, signed
             runs-on: [self-hosted, node-b, linux, x64, docker, publish]
             build → run the image → push GHCR (buildx SBOM + provenance
             attestations) → keyless cosign sign over the digest
  ──> ops repo: pin the new digest in personal/vogt/docker-compose.yml
  ──> Komodo POST /execute/DeployStack {"stack":"personal-vogt"}
             └── periphery clones ops, compose pull/up on Node B
```

Either path produces a signed, digest-addressable image; **neither
deploys**. The digest a release publishes and the digest a merge publishes
are pinned the same way, which is the point of r5 — a fix reaches production
without a version number being invented to carry it.

Rules this flow obeys:

- **Never `ssh … docker compose up -d`.** Desired state is the ops compose;
  Komodo is the only thing that applies it. Deployed containers are never
  hand-edited.
- **Publishing is automatic; deploying is a separate, deliberate act.**
  *(revised r5)* A push to `main` publishes a commit image and a tag
  publishes a release; **neither moves production**. What a merge still
  cannot do is cut a release — no semver tag, no `latest`, no wheel — and
  what nothing in CI can do is deploy (NFR-D10). The digest bump in ops
  stays the human-or-agent decision point. Automating it via
  `ops/scripts/komodo-deploy.sh` remains available and is not v1 scope.
- **Runners are self-hosted, always.** `runs-on: ubuntu-latest` is
  prohibited estate-wide; the repository must be added to the
  `public-node-b` runner group **before** its first workflow exists, or CI
  is red with no reachable runner. `docker`/`publish` is advertised only by
  the two Docker-in-Docker-backed workers. The self-hosted image has no
  language runtimes preinstalled — `astral-sh/setup-uv` is explicit.
- **Signing is keyless** (`id-token: write` → cosign via Fulcio/Rekor), so
  there is no signing key to store or rotate and the signature binds to
  this repository and workflow. *As built*: the SBOM and provenance are
  buildkit attestations (`sbom: true`, `provenance: true` on
  `docker/build-push-action`) rather than a separate syft run plus `cosign
  attest`. Cosign signs the digest and nothing else; one attestation
  producer is enough, and a second would need a reason for disagreeing with
  the first.
- **The image is run before it is pushed.** v0.1.0 published an image whose
  entrypoint did not exist — the build was green, the signature valid, and
  the artefact had never been executed. The publish gate now runs
  `--version` and asserts `serve` still refuses to invent a listen address
  (NFR-D2).
- **Pin the digest, not the alias.** `:latest` and even `:sha-…` are
  lookup conveniences; the ops compose carries the digest so a deploy is
  reproducible and a rollback is a one-line revert.

### 2.4 Topology C — deferred (explicit non-goals for v1)

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

Runtime secrets live in Infisical `apps` under `HOMELAB_VOGT_*` names and
are **copied** into the Komodo stack environment — Komodo has no external
secret-manager connector and interpolates its own values as `[[NAME]]`.
That copy is a standing drift risk: a credential rotated or revoked
upstream keeps working in Komodo until something notices. Alert on the
*symptom* (auth failures at the consumer), never on the freshness of the
copy.

## 4. Configuration & bootstrap rules (lessons encoded)

Cadastre's `:18081` incident: a retired default port shipped in a
downstream image in seven places, and the bootstrap script checked that a
config key *existed* rather than that its *value* was right, so clients
kept a dead URL forever. Additionally a health check pinned an MCP
protocol version the server refused. Therefore:

### 4.1 Defaults: forbidden for exposure, required for allocation *(revised r4)*

r1 stated this as "**no default host/port anywhere** — not in code, docs,
images, or examples". That rule, applied literally to a compose file, cost
cadastre every deploy from `cadastre#42` onward: the port, TLS path, and
token path were `:?`-gated required values, `verify` and `publish` went
green, and the deploy step went red because three values were never set.
The gate was protecting against nothing — the ports bind a Tailscale
address, so choosing them is an allocation inside the tailnet, not an
exposure decision, and the certificate and token file are real paths that
already exist on the host.

The rule that survives, split by what the value actually decides:

- **No default may encode exposure or identity.** Public hostnames, a
  `0.0.0.0` bind, a published LAN port, or a URL a client will trust: these
  have no defaults in code, images, docs, or examples, and `<port>`
  placeholders stay placeholders. A wrong default here is the `:18081`
  incident.
- **Values that are pure host allocation carry concrete defaults**, in the
  compose file, overridable from the Komodo stack environment, with a
  comment naming *which host they describe*. A tailnet-bound port, an
  operator-owned certificate path, a token path: gating these produces
  broken deploys, not safety.

`vogt serve` still requires its listen address to be configured — the
compose file is what supplies it, and the compose file is allowed to know
the answer for Node B.

### 4.2 Bootstrap reconciles values, not key existence

Any client-setup script compares the configured endpoint against the
intended one and rewrites on mismatch; "key present" is never success.

**Not built (v1).** Vogt ships no client-setup script, so NFR-D3 currently
constrains nothing: `vogt-mcp-remote` reads `VOGT_URL` and
`VOGT_TOKEN_FILE` from its own environment, and the one client config in
this repository's README is hand-written. The rule stands as the
requirement on any such script the moment one exists — which is the
condition under which the `:18081` incident happened at all.

### 4.3 One connection document

A single generated `CONNECTING.md` (from the running server: `GET
/connection-info`) states the canonical URL, `/mcp` path, and supported MCP
protocol versions. Client configs are derived from it, never hand-copied
per client.

**Built at r7 (FR-A8), and not as a committed file.** `vogt connect` renders
the document from the running instance — `--format markdown > CONNECTING.md`
if you want one on disk, `--format json` for a client config you can paste.
A `CONNECTING.md` tracked in the repository would be one more copy of a URL
going stale against the instance it describes, which is the failure this
section was written to prevent.

The half that was actually missing was smaller than "a generator". Between
them, `/health`, `/version` and `/connection-info` reported every fact about
an instance **except where it is**: the response carried paths and no URL.
That absence was not an oversight so much as an unanswerable question — the
process binds `0.0.0.0:8000` in a container and is published at a tailnet
address on another port, so the address a client should use is a fact only
the operator holds.

It is therefore `public_url`: an **exposure** value, so no default, ever
(NFR-D2). An instance without one reports that nobody has said, rather than
inventing an answer — a guessed URL would be wrong in exactly the deployment
the field exists for, and from a client a wrong URL and an unreachable one
are the same symptom.

**Prefer the HTTP client.** `connect --client http` needs nothing installed,
which is why it is the default: a client holding a copy of Vogt's code has a
version that can skew (FR-A6 exists because of it) and a second place to
upgrade. `--client bridge` is for agent products that can only spawn a local
process, and the result says `requires_install: true` rather than letting
that cost go unmentioned.

### 4.4 Health checks are protocol-version-agnostic

Probes hit `/health/ready` (plain HTTP). Nothing outside a real MCP client
sends `initialize`, and nothing pins a protocol version. This is what makes
the compose healthcheck in §2.2 possible at all.

### 4.5 Version skew warns, never blocks

Bridge↔server version mismatch is one stderr line (stdout is MCP framing);
startup always proceeds.

### 4.6 The config schema is the single source of truth

Pydantic; compose files, example configs, and docs are generated from it,
and CI fails on drift (NFR-Q4). The committed `personal/vogt/` compose in
the ops repo is a *consumer* of that schema — a generated-then-reviewed
artifact, not a hand-maintained parallel truth.

## 5. Storage, backup, upgrade

- One named volume mounted at `/var/lib/vogt` (both SQLite files +
  backups), on Node B's root NVMe (`/mnt/2tnvme`) unless capacity says
  otherwise — `/mnt/4tnvme` is the same speed class and is where bulkier
  stacks live, so moving later is a capacity decision, not a performance
  one.
- `vogt backup` produces a consistent snapshot (SQLite backup API,
  both stores + a manifest with schema versions); `restore` verifies the
  manifest before touching anything.
- Upgrade path *(intended)*: bump the pinned digest in ops → `DeployStack`
  → forward-only migrations run under `migration_lock` at startup →
  `/health/ready` gates traffic until migration completes.

  **As built, this does not happen, and it is the one deployment gap in
  v1.** Migrations are applied by `init` — which is idempotent and brings an
  existing instance forward — and by nothing else. `serve` does not migrate,
  there is no `vogt migrate` verb (FR-L1), and `/health/ready` reports the
  *applied* schema version without comparing it to the version the running
  image expects, so it answers `ready` against a store that is behind
  (NFR-I3's third clause). The compose `command:` is `serve`, so an image
  carrying a new migration would come up, pass its healthcheck, and fail
  later on a missing table — as a SQL error, at whatever operation touched
  it first.

  Until that is closed: after a digest bump that crosses a migration, run
  `vogt init` in the container before trusting the stack. Both halves are
  recorded in `REQUIREMENTS.md` §5.
- Rollback is a revert of the digest line in ops plus a `DeployStack`.
  Forward-only migrations mean a rollback across a schema change needs a
  restore, not just an older image — check `SCHEMA.md` before assuming the
  digest revert is sufficient.
- Images: GHCR, SBOM + signed, tag-triggered releases only (`DESIGN.md`
  §8.1 — docs pushes can never publish an image).

## 6. Operating the stack (Node B specifics)

Known failure modes on this host, recorded before they cost a session:

- **GHCR `denied: denied` on compose pull.** Node B's periphery holds a
  stored `ghcr.io` credential, and Docker sends it on *every* ghcr.io pull
  — so a stale token turns even a public image into a permission error.
  Diagnose by testing the stored `auths["ghcr.io"]` entry against the ghcr
  token endpoint (`403` = stale), then update Komodo's `ghcr.io` registry
  account. This is the single most likely first-deploy failure for a
  GHCR-hosted stack here.
- **`DeployStack` git-pull conflicts.** Komodo keeps its own disposable
  clone per stack at
  `/mnt/2tnvme/docker/volumes/komodo/periphery/stacks/personal-vogt/`. It
  is deploy-time scratch space — never `cd` into it to edit or store
  anything. An untracked-file conflict there aborts the deploy mid-pull;
  the fix is to delete the offending junk so it re-clones cleanly.
- **`Missing git token` for `git account sprooty`.** The ops stacks
  reference `git_account: sprooty`; the Komodo Git provider account
  username must stay `sprooty` even though Forgejo git remotes use
  `https://git:$TOKEN@…`.
- **A successful deploy can still report null state.** Verify the deployed
  digest *and* an actual `/health/ready` response — not merely that the
  container started.
- **No SSH is not a blocker.** Komodo's `/execute/RunStackService` and the
  `Local`-server terminal API are both remote-exec paths into this host
  when Tailscale SSH is unavailable.

Komodo API shape (2.2.0): each request is its own path — `POST
/read/GetStack`, `POST /write/UpdateStack`, `POST /execute/DeployStack` —
with bare JSON params. A `400` with an empty body means the request shape
is wrong; bad credentials return `401`.

## 7. Reaching an instance from an agent environment *(r7)*

FR-A8 makes an instance able to say where it is and hand out a client
configuration. It does not put that configuration in front of an agent, and
those are different problems: an LLM running in a MyDevEnv2 container has no
way to learn Vogt exists, and would have no credential if it did.

The precedent is cadastre, reached from the same containers today. Reading
`MyDevEnv2/deploy/mcp-bootstrap.sh` — invoked from `agent-auth.sh`, so every
`mydevenv2-agent-auth run|check|shell` re-runs it idempotently — settles two
questions that looked open:

- **An index is used where one exists, and is not required where one does
  not.** Cadastre's bridge is installed at *build time from PyPI*
  (`pip3 install "cadastre[mcp-client]"` in MyDevEnv2's `Dockerfile`), which
  became possible when cadastre went public. The earlier mechanism — an
  editable install from the mounted workspace, in
  `mcp-bootstrap.sh`'s `install_bridge` — is deliberately retained behind a
  `command -v` guard, both as a fallback for a workspace newer than the
  image and because it is the only path available to a package with no
  index.

  Vogt is that package today, and its source sits in the same mount. So the
  supported mechanism exists and is proven; what it is not is the *preferred*
  one. NFR-PO4 defers the wheel to PyPI at the public milestone and builds
  no private index in the meantime — not because an index is unnecessary,
  but because the interim index would have one user and a migration to undo,
  while the workspace path already works.
- **The stdio bridge is doing token hygiene, not just transport.** Codex
  registers the URL natively (`--bearer-token-env-var`); Claude Code and
  OpenCode register a *wrapper command*, so that — in the script's words —
  "registration stores only the endpoint and wrapper command; no bearer
  value". A bearer token in `~/.claude.json` is the thing being avoided, and
  it is why `--client bridge` is not merely a fallback for clients that
  cannot speak HTTP.

**The five prerequisites.** None require a change to Vogt itself; four land
in `MyDevEnv2` and one in the estate's `AGENTS.md`. They are stated here
because Vogt is what they make reachable, and because an integration nobody
wrote down is one that gets rebuilt from memory.

| # | Prerequisite | Where it lands | Done |
|---|---|---|---|
| 1 | A scoped token exists and is brokered: `vogt token issue`, stored in Infisical `apps` as `HOMELAB_VOGT_TOKEN`, exported by `mydevenv2-agent-auth`. Nothing brokers a `VOGT_*` secret today. | Infisical + `mydevenv2-agent-auth` | ☐ |
| 2 | `VOGT_PUBLIC_URL` is set in the Komodo stack environment. The compose gates it (`${VOGT_PUBLIC_URL:?}`) because it is an exposure value (NFR-D2), so **`DeployStack` fails until it is set** — set it before deploying r7, not after. | Komodo stack env | ☐ |
| 3 | A wrapper `/usr/local/bin/mydevenv2-vogt-mcp` mirroring `mydevenv2-cadastre-mcp`, so Claude Code and OpenCode registrations carry no bearer value. | MyDevEnv2 image | ☐ |
| 4 | `deploy/mcp-bootstrap.sh` registers Vogt for every client present — editable install of the bridge from the workspace, `codex mcp add --url … --bearer-token-env-var`, and the wrapper for Claude/OpenCode. | MyDevEnv2 image | ☐ |
| 5 | A row in `~/Working/AGENTS.md`'s service table saying what Vogt is and when to reach for it. Without it an agent has the tools and no idea what they are for — `grep -i vogt` over that file currently returns nothing. | Estate `AGENTS.md` | ☐ |

3 and 4 are image changes, so they go to the `dev` branch, deploy to
`dev-mydevenv2`, and are validated from inside a container there before
promotion to prod — which also means applying them restarts the container an
agent session is running in.

## 8. Known: every write costs a WAL checkpoint

Measured against the deployed data volume on Node B, one declared write
takes **~25ms**, almost all of it fsync:

| `synchronous` | ms per write |
|---|---|
| `full` | 28.7 |
| `normal` | 24.7 |
| `off` | 0.1 |

The pragma barely moves it, which is the tell. `connection.py` opens a
connection per transaction and closes it, and closing the last connection to
a WAL database checkpoints the log — and a checkpoint fsyncs whatever
`synchronous` says about commits. The cost is the checkpoint, not the commit.

Per-transaction connections were chosen deliberately, so that a CLI process,
a server process and a stdio MCP process can share one data directory
(§2.1). That requirement is real; closing after every transaction is not the
only way to meet it. A connection held per store instance, or per thread,
would keep multi-process safety while letting the WAL checkpoint on SQLite's
own schedule instead of hundreds of times a sweep.

Not fixed here because it is a storage-layer change and wants its own
testing. Recorded because 25ms per write is invisible on a laptop and
obvious on a sweep that records thousands of observations — and because the
number will otherwise be rediscovered by whoever next wonders why a large
estate sweeps slowly.

**Do not "fix" this with `VOGT_SQLITE_SYNCHRONOUS=off` in production.** That
trades durability for speed in a product whose declared store is an audit
log. The knob exists for test runs, where nothing outlives the process.
