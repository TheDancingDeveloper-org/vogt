# Vogt — Deployment & Network Topologies (v0.2, revision r5)

Status: **built** (M4; reconciled against the delivered v1 on 2026-08-12;
consolidated with the engine's deployment documents on 2026-08-15).
Companion to `DESIGN.md` §7 and `SCHEMA.md`. Shaped heavily by cadastre's
deployment history — see §4 for the specific lessons encoded here.

**Two documents were folded in on 2026-08-15** and no longer exist separately:
`docs/engine/TOOLING.md` is §10 (what the runtime image carries) and
`engine/deploy/KOMODO.md` is §11 (the engine's own standalone stacks). Both
described MyDevEnv2 as a product with its own deployment story; it has one
deployment story, and this is it.

> **This document is the reference customisation, not the generic path.**
>
> It describes the maintainer's own estate deployment on Node B — Tailscale,
> Komodo, Infisical, host mounts, a Rust session engine as the front door. It
> is retained deliberately, because it is the largest customisation of Vogt
> that exists and therefore the best evidence that the extension points are
> real. Every estate-specific choice below is annotated with the extension
> point it exercises.
>
> If you are installing Vogt, you want
> [`GETTING_STARTED.md`](GETTING_STARTED.md), which takes a new operator from
> checkout to a running instance with none of this. If you are adapting Vogt
> to a deployment of your own, you want
> [`CUSTOMISATION.md`](CUSTOMISATION.md), which names the supported extension
> points. Nothing here is a prerequisite for either.
>
> Hostnames, tailnet addresses, stack names and host paths appear throughout.
> They are topology, not credentials: no secret value is recorded in this
> document, and the two that matter are brokered as files (§2.2a, §7.2).

This document covers three deployments, and confusing them is the most
expensive mistake available here:

| | Compose file | Stack | What it runs |
|---|---|---|---|
| **The merged product** (§9) | `deploy/vogt-stack.compose.yml` | `dev-vogt` | engine + core, one port — the target |
| Core-only (§2.2) | `deploy/personal-vogt.compose.yml` | `personal/vogt` | vogt-core alone, digest still a placeholder |
| Standalone engine (§11) | `engine/deploy/docker-compose.yml` | `prod/dev-mydevenv2` | the engine with no core — being retired |

§4.2 and §4.3 describe client-side tooling that is **not built** — see the note
in each, and `REQUIREMENTS.md` §7.

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

### 1.1 The merged product: two processes, still one port *(r9)*

From M9 the repository also contains the session engine (`engine/`, merged
from MyDevEnv2's `dev` branch at `2214a7d`), and the deployed shape gains a
second process — not a second port (NFR-D11):

```
one container, one published port
  ├── engine (Rust/Axum) ← the only listener
  │     ├── /                     the PWA
  │     ├── /api/sessions|files|git|assistant|…   its own surface
  │     ├── /api/sessions/{id}/attach             WebSocket
  │     ├── /api/vogt/...   → core /api/...   (core token injected)
  │     ├── /mcp            → core /mcp       (caller's own token)
  │     ├── /ui-legacy/...  → core /ui/...    (FR-U9)
  │     └── /healthz, /readyz  ← aggregate, incl. a probe of the core
  └── vogt-core (`vogt serve`) ← loopback only, never published
```

Everything §1 says about the core is still true of the core; what changed
is which process the world reaches first. The engine fronts rather than the
core because it already embeds and serves the PWA and already speaks
WebSocket on the hottest path there is — terminal I/O — and proxying that
through FastAPI would add a fragile hop to it.

Three properties are load-bearing, and each is asserted in
`engine/server/tests/vogt_core.rs`:

- **No core token leaves the process, and each front-door token is its own
  actor.** A caller presents a *front-door* token; the engine injects the
  core token that token is **paired** with, so Vogt's audit rows name the
  actor who acted rather than "the proxy" (FR-S9). A pairing is
  `vogt_core_token_file` on the token's `extra_tokens` entry — a path to a
  brokered file, the same form `VOGT_CORE_TOKEN_FILE` takes and for the same
  reason: the front-door token beside it usually arrives through
  `MYDEVENV2_EXTRA_TOKENS_JSON`, and a paired value written there would put
  a second credential in the environment, where `docker inspect` shows it. A
  deployment that keeps its whole token table in the config file may write
  the pairing as `vogt_core_token` on the entry instead; the file wins where
  both are set, and a file that is missing or empty fails the boot rather
  than silently reverting that token to the shared actor.

  A token with no pairing of its own — including the primary one — falls
  back to the deployment-wide `vogt_core_token`, so a deployment
  provisioned with one core token and no pairings, which is what
  `deploy/vogt-stack.compose.yml` describes, is unaffected: pairings are how
  a deployment opts in to named actors, one token at a time. For that
  fallback `VOGT_CORE_TOKEN_FILE` is still the preferred form and still
  takes precedence over `VOGT_CORE_TOKEN` — a deployment that brokered the
  value into a file meant to keep it out of the environment. A caller with
  neither a pairing nor a fallback gets a 503 naming the token whose pairing
  is missing; the request is not forwarded to the core uncredentialed.
- **`/mcp` is a pass-through, deliberately.** An MCP client already holds a
  core token bound to an actor (`vogt token issue`) — that is how agents in
  a MyDevEnv2 container reach Vogt today, §7 — so rewriting its credential
  would replace a real actor with a shared one. The actor mapping above
  reaches `/api/vogt` only: `/mcp` forwards what the caller sent even when
  that caller's front-door token has a pairing, and works with no core token
  configured at all. The per-session tokens of FR-S10 arrive with the
  session work.
- **An absent core does not make the container unready.** `/readyz` reports
  the core's state in full — including its applied schema version, read
  from the core's own `/health/ready` — and excludes it from the verdict,
  because restarting the engine cannot revive the core and would kill every
  live PTY doing it (FR-E9). The Vogt routes then answer 503 naming the
  reason rather than an empty result (FR-U21). Do not "fix" a red
  `vogt_core` check by making it fatal; fix the core.

A wildcard route segment matches at least one character, so each proxied
prefix is registered in three shapes — bare, trailing-slash, and wildcard.
Missing the middle one sent `/ui-legacy/` to the PWA's catch-all, which
answered with the engine's "web bundle not present" placeholder: a 404 that
mentions neither Vogt nor the proxy, and reads like a front-door
misconfiguration rather than a routing miss.

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

| Fact | Value | Extension point |
|---|---|---|
| Host | Node B, `winrarhost` — TS `100.92.54.45`, LAN `192.168.1.75` | — |
| Komodo server (periphery) | `Local` | — |
| Komodo stack | `vogt` (the ops directory is `personal/vogt`; the stack name is not prefixed) | — |
| Desired state | `indexarr/ops` → `personal/vogt/docker-compose.yml` | Compose overlay |
| Image | `ghcr.io/thedancingdeveloper-org/vogt`, digest-pinned | the published base image, unmodified |
| Exposure | tailnet only — bound to the Tailscale address, never `0.0.0.0` | `ports` + `VOGT_BIND_IP`; `public_url` states the address clients use |
| TLS name | `winrarhost.tailc7d3c.ts.net` (Tailscale-issued Let's Encrypt) | `serve --tls-cert` / `--tls-key`, operator-owned and mounted `:ro` |
| App data | named volume `vogt-komodo-data` → `/var/lib/vogt` | `data_dir` |
| Operator material | `/mnt/2tnvme/docker/volumes/vogt/{tls,auth}`, mounted `:ro` | Compose overlay |
| Estate | `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` → `/home/sprooty/Working`, writable, as `VOGT_UID` (1000) | `import_root` + the uid/gid contract — see *Observing an estate on a host path* in `CUSTOMISATION.md` |
| Forge credential | `VOGT_GITHUB_TOKEN_FILE` → `/run/secrets/github_token` (see 2.2a) | `github_token_file`, an optional integration |

None of the right-hand column is special-cased for this estate. A different
operator makes the same set of choices with different values, which is the
claim this document exists to support.

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

### 4.7 A second instance on one host collides on every allocation default *(r21)*

Written after standing a production instance up beside an existing one on
the same host, where every item below was found the hard way.

4.1's rule — allocation values carry concrete defaults — is right, and this
is the bill that comes with it: **every one of those defaults is a
host-wide singleton, and the second instance is where you find that out.**
None of these are faults in the compose file. They are faults in a
particular *deploy*, and they differ mainly in how honestly they announce
themselves.

| Value | Default | What the collision looks like |
|---|---|---|
| `VOGT_STACK_SUBNET` | `10.59.0.0/16` | network creation fails, and Compose blames the *network* rather than the variable that chose it |
| `VOGT_CORE_IP` | `10.59.0.10` | must sit inside the subnet above — change one and you must change the other |
| `VOGT_CONTAINER_NAME` | `vogt-engine` | Docker refuses a duplicate name rather than replacing it |
| `VOGT_CORE_VOLUME` | `vogt-core-data` | **no error at all**: the second instance quietly opens the first one's database |
| `VOGT_PORT`, `VOGT_BIND_IP` | none | the bind fails — the honest one |
| `VOGT_WORKSPACE_DIR`, `VOGT_HOME_DIR`, `VOGT_TAILSCALE_DIR` | none | two instances writing one tree; sharing the Tailscale directory also means two nodes contending for one machine identity |
| `TAILSCALE_HOSTNAME` | `vogt` | the tailnet suffixes the second node, so the name you configured is not the name it has |

The volume row is the one to fear. Every other collision stops the deploy;
that one completes it. Two instances sharing `vogt-core-data` are two
writers on one SQLite database, and the symptom arrives later and looks
like corruption rather than like configuration.

**Choosing a subnet: look, do not increment.** The neighbouring instance
held `10.59.0.0/16`, so the obvious choice was `10.60.0.0/16` — which an
unrelated stack already held. Docker's default address pool is
`10.0.0.0/8` handed out in `/16`s, and it allocates the *lowest free block
first*, so gaps below the high-water mark are precisely the blocks most
likely to be taken while you are still setting up. Enumerate what is
actually in use, then pick above the highest:

```bash
docker network ls --format '{{.Name}}' | while read -r n; do
  docker network inspect "$n" --format '{{range .IPAM.Config}}{{.Subnet}}{{end}}'
done | grep -E '^10\.' | sort -t. -k2 -n
ip route show | grep -E '^10\.'   # routes the daemon does not know about
```

Check the routing table too: a subnet no Docker network claims can still be
reachable through a VPN route or a peer's advertised subnet, and the
collision then shows up as traffic silently going to the wrong place rather
than as a failed deploy.

**A fronted instance on a CGNAT address cannot be proxied.** If you point a
DNS record at a Tailscale address (`100.64.0.0/10`), a CDN cannot route to
it — Cloudflare answers `522` on every request. Pointing at a tailnet
address and proxying are mutually exclusive: leave the record unproxied and
you get the registrar and DNS authority, not the proxy. The corollary is
worth knowing before somebody reports an outage: the name resolves for
everyone, and connects only from the tailnet. A client on the LAN, with no
Tailscale, sees a name that resolves and a connection that hangs. That is
the design working.

**The first boot answers `401` on `/api/vogt`, and that is correct.** The
front door injects a *paired core token* on those routes, and the core is
what mints it — so it cannot exist before the instance does. Until it is
set, sessions, terminals and `/mcp` all work and `/api/vogt` refuses with a
named reason. That is FR-E9 behaving, not an outage, and treating it as one
leads people to widen a token scope to "fix" it.

**A dead Tailscale auth key fails the whole instance, not just Tailscale.**
The engine's tailscale check is fatal, so an expired or already-consumed
key leaves `/readyz` at `503` with both halves running perfectly — an
alarming shape for a trivial cause. Auth keys are also frequently
single-use: reusing the key that stood up the first instance will not
enrol the second.

**Server-side speech is off until something serves it.** The speech routes
proxy to OpenAI-compatible audio endpoints named by
`MYDEVENV2_ASSISTANT_STT_BASE_URLS` / `_TTS_BASE_URLS`, whose defaults are
a local speech server first and a cloud endpoint second. With neither a
local server nor a valid key, those routes `404` and clients fall back to
browser-native speech — the honest absence (FR-T6), not a fault. Turning
them on is a matter of running a speech container or supplying a key; note
that an OpenAI-*compatible chat* provider is not necessarily an audio
provider, so a working chat key is not evidence that speech will work.

## 5. Storage, backup, upgrade

- One named volume mounted at `/var/lib/vogt` (both SQLite files +
  backups), on Node B's root NVMe (`/mnt/2tnvme`) unless capacity says
  otherwise — `/mnt/4tnvme` is the same speed class and is where bulkier
  stacks live, so moving later is a capacity decision, not a performance
  one.
- `vogt backup` produces a consistent snapshot (SQLite backup API,
  both stores + a manifest with schema versions); `restore` verifies the
  manifest before touching anything.
- Upgrade path: bump the pinned digest in ops → `DeployStack` → forward-only
  migrations run under `migration_lock` at startup → `/health/ready` gates
  traffic until migration completes. **This is what happens, from r13**
  (2026-08-15); before that it was the *intent*, and the paragraph describing
  it was the one thing in this document that had never been true.

  What was wrong, since a deployer who read the old text may still be carrying
  the workaround: migrations were applied by `init` and by nothing else,
  `serve` did not migrate, there was no `vogt migrate` verb, and
  `/health/ready` reported the applied schema version without comparing it to
  the version the image expects — so it answered `ready` against a store that
  was behind. The compose `command:` is `serve`, so an image carrying a new
  migration came up, passed its healthcheck, and failed later on a missing
  table as a SQL error at whatever touched it first.

  **The `vogt init` workaround after a digest bump is no longer needed** and
  can be dropped from any runbook that carries it. If you want to run it
  anyway it stays idempotent and harmless; `vogt migrate` is the verb that now
  says what it does, and it refuses a data directory holding no instance.

  A store *ahead* of the build — a rollback across a migration — keeps
  answering `ready`, and that is deliberate. `migrate` refuses it with the
  forward-only message naming the migration; a probe going red there would
  make a deliberate rollback look like a broken container, and would send an
  operator to the healthcheck for a diagnosis only the migrator can give. The
  next bullet is still the rule for that case.
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
`engine/deploy/mcp-bootstrap.sh` — invoked from `engine/deploy/agent-auth.sh`,
so every `mydevenv2-agent-auth run|check|shell` re-runs it idempotently —
settles two questions that looked open. (Those paths were
`MyDevEnv2/deploy/…` when this section was written; the merge brought that
tree into this repository under `engine/`.)

- **An index is used where one exists, and is not required where one does
  not.** Cadastre's bridge is installed at *build time from PyPI*
  (`pip3 install "cadastre[mcp-client]"` in `engine/Dockerfile`), which
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

**The five prerequisites.** None required a change to Vogt itself; four landed
in what was then the `MyDevEnv2` repository and is now this repository's
`engine/` subtree, and one in the estate's `AGENTS.md`. They are stated here
because Vogt is what they make reachable, and because an integration nobody
wrote down is one that gets rebuilt from memory.

| # | Prerequisite | Where it landed | Done |
|---|---|---|---|
| 1 | A scoped token exists and is brokered: `vogt token issue`, stored in Infisical `apps` as `HOMELAB_VOGT_AGENT_TOKEN`, exported by `mydevenv2-agent-auth`. | Infisical + `engine/deploy/agent-auth.sh` | ☑ |
| 2 | `VOGT_PUBLIC_URL` is set in the Komodo stack environment. The compose gates it (`${VOGT_PUBLIC_URL:?}`) because it is an exposure value (NFR-D2), so **`DeployStack` fails until it is set** — set it before deploying r7, not after. | Komodo stack env | ☑ |
| 3 | A wrapper `/usr/local/bin/mydevenv2-vogt-mcp` mirroring `mydevenv2-cadastre-mcp`, so Claude Code and OpenCode registrations carry no bearer value. | `engine/deploy/vogt-mcp-auth.sh`, installed by `engine/Dockerfile` | ☑ |
| 4 | `mcp-bootstrap.sh` registers Vogt for every client present — editable install of the bridge from the workspace, `codex mcp add --url … --bearer-token-env-var`, and the wrapper for Claude/OpenCode. | `engine/deploy/mcp-bootstrap.sh` | ☑ |
| 5 | A row in `~/Working/AGENTS.md`'s service table saying what Vogt is and when to reach for it. Without it an agent has the tools and no idea what they are for. | Estate `AGENTS.md` | ☑ |

3 and 4 were image changes, so they went to the `dev` branch, deployed to
`dev-mydevenv2`, and were validated from inside a container there before
promotion to prod — which also meant applying them restarted the container an
agent session was running in.

### 7.1 As delivered

All five are done, verified from inside a running `mydevenv2-dev` container.
Recorded in this shape — prediction beside outcome — because the differences
are the part worth keeping.

**The secret is `HOMELAB_VOGT_AGENT_TOKEN`, not `HOMELAB_VOGT_TOKEN`.** The
table above has been corrected. `agent-auth.sh` reads it under
`MYDEVENV2_VOGT_SECRET_NAME`, defaulting to `HOMELAB_VOGT_AGENT_TOKEN`, from
Infisical project `apps` (`prod`); the token is bound to actor
`agent:mydevenv2` with scopes `read` and `work.write`, which is deliberately
not `admin` — issuing tokens still needs `docker exec vogt vogt token issue` on
Node B. The name says which of Vogt's tokens it is, and there is more than one
(a read-only `VOGT_GUI_TOKEN` exists for a browser session), so the qualified
name is the better one. Anything still saying `HOMELAB_VOGT_TOKEN` is wrong.

**An absent Vogt secret is not fatal; an absent cadastre secret is.**
`agent-auth.sh` exports `CADASTRE_HTTP_TOKEN` and dies if it is empty, but
exports `VOGT_HTTP_TOKEN` with a trailing `|| true` and only writes
`VOGT_TOKEN_FILE` when the value is non-empty. That asymmetry is deliberate and
is commented as such: an instance may legitimately not be deployed yet, and
agent auth must keep working for `git` and `gh` regardless. The same
best-effort rule governs registration — the four `install_vogt_*` functions run
after cadastre's, so a Vogt failure cannot cost an agent its git credentials.

**The token exists in two forms at once, on purpose.** It is exported as
`VOGT_HTTP_TOKEN` *and* written to a `umask 077` temp file exported as
`VOGT_TOKEN_FILE`, cleaned up on exit. Two forms because the two clients need
different ones: codex takes `--bearer-token-env-var VOGT_HTTP_TOKEN` and reads
the variable; `vogt-mcp-remote` reads `VOGT_TOKEN_FILE`, a file rather than an
argument so the token never reaches a process listing.

**`VOGT_PUBLIC_URL` is set, and `/connection-info` is the proof.** The deployed
instance answers with
`{"name":"vogt","version":"0.2.0","url":"https://winrarhost.tailc7d3c.ts.net:18094",…}`
and `/health/ready` reports
`{"status":"ready","declared_schema_version":6,"observed_schema_version":2}`.
That endpoint reports a *configured* URL, never
an inferred one (§4.3), so a URL coming back at all is the evidence the stack
environment carries it.

**Registration stores an endpoint and a command, never a bearer.** Confirmed in
`mcp-bootstrap.sh`: codex gets the URL plus an env-var name; Claude Code and
OpenCode get `/usr/local/bin/mydevenv2-vogt-mcp`, which `engine/Dockerfile`
installs from `engine/deploy/vogt-mcp-auth.sh` and which execs
`mydevenv2-agent-auth run -- env VOGT_URL=… vogt-mcp-remote`. The codex path
also reconciles the URL rather than checking the key exists — the lesson from
the cadastre `:18081` → `:18092` move, applied to Vogt before it could bite.

**The bridge installs from the workspace, as §7 predicted it would have to.**
`install_vogt_bridge` guards on `command -v vogt-mcp-remote`, then does an
editable `pip3 install --user -e` of this repository from
`$HOME/Working/Active/apps/vogt`. Verified present at `~/.local/bin`. Its
comment already names the exit: when Vogt goes public this moves into the
Dockerfile and the runtime install becomes the same no-op fallback cadastre's
is today (NFR-PO4).

**Where it landed changed under the plan's feet.** The table predicted "MyDevEnv2
image" for 3 and 4, meaning a separate repository. The MyDevEnv2 merge arrived
first, so all four code-side prerequisites are now files in this repository's
`engine/` subtree, built by `engine/Dockerfile` and shipped by
`.github/workflows/build.yml` — which is a happier place for them: the
integration and the thing it integrates with are now reviewable in one diff,
and the build that ships them is the one whose logs this repository can read.
(The sentence here used to name `engine/.woodpecker/server.yml`. That was the
vendored copy of the engine's Woodpecker pipeline, it never ran against this
repository, and it has been deleted; the standalone stacks in §11 are still
built by the *forge's* copy of it.) The Komodo stacks the engine's own image
deploys to are still the engine's, not Vogt's; see `docs/MERGE_MYDEVENV2.md`
§10 for the stack consolidation that has not happened yet.

**One thing is still open, and it is in a file this repository must not edit.**
The estate `AGENTS.md` Vogt row (prerequisite 5, done) still says the token is
"not yet exported by `mydevenv2-agent-auth` — read it from Infisical directly
until the image ships that". The image has shipped it: `VOGT_TOKEN_FILE` is set
in a live container and the file is non-empty. That sentence should be struck
by whoever next edits `~/Working/AGENTS.md`. It is noted here rather than fixed
here because an estate-wide file is not this repository's to change, and
because stale remediation advice is worse than none — it sends an agent to
Infisical for a value it already has in its environment.

### 7.2 Inside a coding session, the credential is the session's *(r9)*

M10 mints a token per coding session, bound to an actor that exists for that
session alone, so that what an agent writes is attributable to the session it
ran in (FR-S10). Everything in §7 above describes how an agent in a
*container* gets the pod's shared token — and those two mechanisms met badly.

Three places replaced the session's credential with the pod's, and all three
did it silently: `agent-auth.sh` fetched the shared token unconditionally,
and with `MYDEVENV2_AUTO_AGENT_AUTH=1` — the deployed setting — it is what
launches a session's shell; the MCP wrapper Claude Code and OpenCode are
registered with re-brokers through that same helper; and the stdio bridge
read only `VOGT_TOKEN_FILE`, which the broker rewrites. The agent still
authenticated, still wrote, still got a 200. Only the audit log was wrong.

The rule now, in all three: **`VOGT_SESSION_ID` present means the session's
token wins.**

| Where | Behaviour |
|---|---|
| `engine/deploy/agent-auth.sh` | Keeps `VOGT_HTTP_TOKEN` as it found it inside a session; brokers everything else as before |
| `engine/deploy/vogt-mcp-auth.sh` | Skips brokering entirely and execs the bridge with the session's token |
| `vogt-mcp-remote` (`adapters/mcp/bridge.py`) | `VOGT_HTTP_TOKEN` inside a session, else the brokered `VOGT_TOKEN_FILE`, else the bare variable — the shape codex passes |

Codex was already correct by accident: it registers the URL natively with
`--bearer-token-env-var VOGT_HTTP_TOKEN` and reads what the session set.

**The failure class is worth remembering, because the next integration can
reproduce it.** A credential that is silently replaced produces working
writes and a false audit trail. Nothing errors, no test that stubs the
transport can see it, and the only signal is an actor name nobody reads until
they need it — which is exactly when it has to be right.

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

## 9. Deploying the merged stack, the first time *(r9)*

This section is the sequence, in order, with what to check at each step. It was
written before the first deploy, because everything below is either a
precondition somebody would otherwise discover at the wrong moment or a failure
mode that reports success.

### 9.0 Three things that are true before you start

*Two of these read the other way round at r9 and were corrected on 2026-08-15,
when the pipeline that had never run, ran.*

1. **The merged image exists and is pinned.** `build.yml`'s `stack-image` job
   publishes `engine/Dockerfile` to
   `ghcr.io/thedancingdeveloper-org/vogt-stack` — a different repository from
   the core-only `ghcr.io/thedancingdeveloper-org/vogt`, because they are
   different artefacts for different deployments and one repository holding
   both is how somebody eventually pins the wrong one.
   `deploy/vogt-stack.compose.yml` carries a real digest. **Step 9.1 is only
   needed for a build made before that job first ran**; after it, pin what the
   job reports.
2. **The core-only stack is the one with a placeholder.**
   `deploy/personal-vogt.compose.yml` still pins
   `ghcr.io/thedancingdeveloper-org/vogt` at an all-zero digest. That stack is
   deprioritised in favour of the merged one and the placeholder is why: it
   cannot be deployed by accident. Do not read the two files as if they were in
   the same state.
3. **A front door with nothing behind it reports itself ready.** The core's
   readiness probe is deliberately non-fatal (§1.1, FR-E9), and
   `entrypoint.sh` declines to start a core when `VOGT_CORE_URL` is not
   loopback without failing. Those two are individually right and together
   they mean `/readyz` can answer `ok` for a stack that serves no Vogt at
   all. **Never accept `/readyz`'s top-level `ok` as proof; read the
   `vogt_core` check.**

### 9.1 Build the merged image

From a host with a Docker daemon, at the repository root — the context is the
root, not `engine/`, because `rust-embed` pulls `web/dist/` and the core is
`pyproject.toml` and `src/`:

```bash
cd web && pnpm install --frozen-lockfile && pnpm build && cd ..
docker build -f engine/Dockerfile -t <registry>/vogt:<sha> .
```

**The registry is settled (M14)** — see §9.0. This step is the hand-build, for
a change that has not been through `build.yml` yet.

The job also smoke-tests both halves before pushing, for the reason the
core-only job records: two releases shipped images that had never been
started. It proves the halves start. It does **not** prove the front door
reaches a core — that is §9.2, and it needs a running pair.

### 9.2 Smoke-test it locally, before any stack sees it

```bash
scripts/smoke_merged_stack.sh https://vogt.sprooty.com "$FRONT_DOOR_TOKEN"
```

Five checks, each naming what its failure means, exiting non-zero if any
fails. It exists because the failure worth catching is not a crash: it is a
front door that comes up, passes its healthcheck and serves no Vogt. Run it
against the container before any stack sees it, and again after the first
deploy.

What it asks, and why each one:

| Check | What proves it |
|---|---|
| `GET /readyz` | `checks[].name == "vogt_core"` says `ready`, **not** the top-level `ok` |
| `GET /api/config` | `vogt.configured` is `true` — this is what makes the GUI offer its Vogt tabs |
| `GET /api/vogt/status` with a front-door token | answers with a `principal` that is the actor you paired, not a 401 or a 503 |
| `POST /mcp` `initialize` with a core token | answers identically to the core's own port (not automated: it needs a *core* token, which the script deliberately does not take) |
| `workspace_agreement` in `/readyz` | the import root is inside the engine's workspace root, or imported projects are invisible to sessions (FR-E3) |
| `backup_agreement` in `/readyz` | `VOGT_ENGINE_STATE_DIR` and the engine's own `state_dir` are one directory, or `vogt backup` succeeds and contains no session history, push subscriptions or VAPID keypair (NFR-I6) |

Two of those were wrong at some point during the merge and neither failed
loudly: a missing pairing answers 503 naming the setting, and an unconfigured
core answers 503 naming `VOGT_CORE_URL`. Both are *correct* answers that look
like outages if you are not expecting them.

### 9.3 The stack environment

Deploy-blocking, gated with `:?` in the compose: `MYDEVENV2_TOKEN`,
`VOGT_PUBLIC_URL`, `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID`,
`HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET`. New for the merged stack:
`VOGT_CORE_TOKEN` (Infisical `HOMELAB_VOGT_CORE_TOKEN`), consumed by an
extended `pre_deploy` hook that writes *two* files, both `chown 1000:1000` —
not the core-only stack's `1000:0`. The compose header carries the exact hook.

The core token is chicken-and-egg on a first deploy and that is fine: bring
the stack up without it, `docker exec` a `vogt token issue`, store it, and
redeploy. Until then `/api/vogt` answers 401 and everything else works, which
is FR-E9 rather than an outage.

**Two files carry the rest**: `deploy/vogt-stack.env.example` is the paste-in
environment with every value the compose gates or defaults, and
`deploy/vogt-stack.komodo.md` is the stack's shape — ops path, file paths, the
`pre_deploy` hook, the socket-overlay trust decision, and the first-deploy
order the chicken-and-egg forces.

**Read the template's header before filling it in.** The compose file's
defaults are production's: port `18094` is what `personal/vogt` is serving on,
and the three bind mounts point at the volumes the running MyDevEnv2 stack
owns. Those defaults are correct for the file they are in — §4.1 says an
allocation carries a default so a deploy cannot fail on an unset variable —
and wrong for a second instance of the same product, which would not stand
beside production but on top of it. The template sets each one explicitly, and
`tests/test_deploy.py` fails if it stops doing so or if a newly-required value
never reaches it.

### 9.4 Dev stack first, and what "carried the load" means

NFR-D12 exists because mobile, voice and push are verifiable nowhere else:
`dev` builds `:dev` images for a dev stack, and only `main` reaches prod. The
merged image needs the same split before it deploys to anything that matters.

Two acceptance tests are outstanding and both need this stack: **M11's demo**
(a board drag round-tripping a `work.transition`, including a refused one
rolling back with the server's reason; a filtered board URL restoring its view
after reload) needs a browser, and **M13's** (a push arriving on a phone,
opened, and the session unblocked) needs a device and the APK. Neither can be
run from a build environment. They are the reason a dev stack is a
prerequisite and not a nicety.

### 9.5 Retiring the old stacks, in an order that matters

Only after the merged stack has carried the load, and in this order:

1. **Republish the APK against the new host first.** The Android build now
   requires `VOGT_ANDROID_SERVER_URL` and has no default; an installed app
   still points at whatever it was built with. Retiring the old host before
   the new APK is out breaks every installed phone.
2. Move DNS/ingress for `vogt.sprooty.com` and confirm the merged stack
   answers on it (M14's naming decision, `MERGE_MYDEVENV2.md` §11.1).
3. Retire `prod-mydevenv2` and `dev-mydevenv2`, then `personal/vogt`.
4. **Then** alias and sunset the `MYDEVENV2_*` config names — a stack-env
   migration, deliberately not done in the same cutover as the host move.

Rollback at every step is the digest line in the ops repo plus `DeployStack`,
with §5's caveat: a rollback across a migration needs a restore, not an older
image.

### 9.6 The signing keystore, and why it is an errand with a deadline

`release.yml`'s `android` job builds and signs a release APK, verifies the
signature with `apksigner` rather than trusting Gradle — an absent signing
config produces a silently *unsigned* release APK, which is the artefact the
job exists to stop — and uploads it. It is inert until four secrets exist, and
says which are missing rather than shipping a debug-signed build under a
release tag.

| Secret | What it is |
|---|---|
| `MYDEVENV2_ANDROID_KEYSTORE_B64` | the keystore, `base64 -w0` |
| `MYDEVENV2_ANDROID_KEYSTORE_PASSWORD` | store password |
| `MYDEVENV2_ANDROID_KEY_ALIAS` | key alias |
| `MYDEVENV2_ANDROID_KEY_PASSWORD` | key password |
| `VOGT_ANDROID_SERVER_URL` | the front door the shell wraps — no default, on purpose (FR-M1) |

**The keystore is the one in the retired forge, and that is a deadline rather
than a task.** A new key is a different app identity: Android refuses to
upgrade an install signed by the old one, so every device already carrying
this app would need an uninstall and a reinstall. Recovering the existing
keystore before that forge goes away preserves the upgrade path; losing it
does not stop signing, it stops *upgrading*, permanently and for every device
at once.

Store it wherever the other stack secrets live, and treat the base64 as what
it is — a signing key that anybody with a job on the runner can read while the
job holds it. The job writes it under `RUNNER_TEMP`, `umask 077`, and removes
it on the way out whether the build succeeded or not.

Where a signed APK is *published* is still an untaken decision. The job
uploads it as a workflow artifact, which is a place to get one from without
inventing a release channel this product has not chosen.

## 10. What the runtime image carries *(consolidated 2026-08-15)*

This section is the source of truth for the toolchain the merged image ships,
and replaces `docs/engine/TOOLING.md`. It describes the image, not the stack:
stack shape, environment and rollout are §2.2, §9 and §11.

Since #184 the toolchain this section lists is installed by
**`engine/Dockerfile.pod`**, not `engine/Dockerfile`. That file is built once
by `.github/workflows/pod-base.yml` as the `vogt-pod-base:{lean,full}` base
image and passed into the merged build by digest as `POD_BASE_IMAGE`, so a push
pulls the toolchain rather than rebuilding it. `engine/Dockerfile` now installs
only the per-commit halves on top of that base — the agent CLIs, the cadastre
bridge, the server binary and the core. **Adding a tool to the pod means
editing `engine/Dockerfile.pod`**; adding one of the per-commit, per-stream
opt-ins means editing `engine/Dockerfile`. Flutter is the difference between
the `lean` (main, releases) and `full` (dev) pod variants. Nothing about the
published image name, its tag streams or the digest-pin deploy mechanism
changed.

The pod is a **neutral development baseline** for builds under
`~/Working/Active/apps/`. It is not tuned for one language, because the estate
it serves is not.

### 10.1 Base and system tools

Ubuntu 26.04 (or newer LTS). On top of it: `ca-certificates`, `curl`, `wget`,
`gnupg`, `lsb-release`, `git`, `git-lfs`, `vim`, `nano`, `less`, `man-db`,
`sudo`, `build-essential`, `pkg-config`, `cmake`, `clang`, `lld`, `nasm`,
`jq`, `ripgrep`, `fd-find`, `bat`, `rsync`, `openssh-client`, `openssh-server`,
`iputils-ping`, `netcat-openbsd`, `dnsutils`, `xdg-utils`, `htop`, `tree`,
`file`, `unzip`, `zip`, `musl-tools`, `gcc-mingw-w64-x86-64`,
`gcc-aarch64-linux-gnu`, `g++-aarch64-linux-gnu`, `libssl-dev`,
`libclang-dev`, `protobuf-compiler`.

`ripgrep` is not a convenience: the engine's `GET /api/search` shells out to
`rg`, and its absence is a deliberate `500` rather than a silent empty result,
because an empty result reads as "no matches".

**Deliberately not carried over from MyDevEnv v1**: `tmux` (server-owned PTYs
replace it), the `xdg-open` shim (a code-server workaround, and there is no
code-server), the `/home/sprooty → /workspace` symlink (the workspace is
bind-mounted at the path it has on the host, so paths match exactly), and
`sshd` on 2223 (the PTY sessions and the workspace bind mount are the access
path; add an explicit emergency SSH route if one is ever needed).

### 10.2 Language toolchains

**Rust** — `rustup` stable with `rustfmt`, `clippy`, `rust-analyzer`;
cross-compile targets `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-gnu`; cargo tools `cargo-deb`, `cargo-zigbuild`,
`cargo-xwin`, `cargo-watch`; `rust-analyzer-mcp`; `sccache` from GitHub
releases, not apt — the apt package lacks Redis support.

**Installed to `/opt/rust`** via `RUSTUP_HOME=/opt/rust/rustup` and
`CARGO_HOME=/opt/rust/cargo`, never `~/.rustup` and `~/.cargo`. This is not a
preference. Until 2026-07-31 they lived under `$HOME`, and because the home
directory is a bind mount at runtime, the pod silently had none of the cargo
tools, none of the cross targets and no `sccache` — only whatever the home
volume happened to contain. The consequence of the fix is that the crate cache
lives in the image, so crates re-download after a redeploy; that is the cheaper
half of the trade.

**Python** — `python3`, `python3-pip`, `python3-venv`, `python3-dev`; `uv` and
`ruff` via pip.

**Node** — Node 22 from NodeSource, `pnpm` global.

**Android** — JDK 21 (`openjdk-21-jdk-headless`,
`JAVA_HOME=/usr/lib/jvm/java-21-openjdk-amd64`); Android SDK at
`/opt/android-sdk` for the same bind-mount reason as Rust, with `ANDROID_HOME`
and `ANDROID_SDK_ROOT` pointing there and `sdkmanager`, `adb`, `apkanalyzer` on
`PATH`. Packages: `cmdline-tools;latest`, `platform-tools`,
`platforms;android-35`, `platforms;android-36`, `build-tools;35.0.0`,
`build-tools;36.0.0`. Builds run through the committed wrapper
(`mobile/android/gradlew`), never system gradle. The Capacitor project targets
compile/target SDK 35; 36 is installed ahead of the bump.

**Flutter (dev image only)** — 3.44.9 stable with its bundled Dart 3.12.2, at
`/opt/flutter`, behind `INSTALL_FLUTTER=true`. Production does not carry it.

### 10.3 Container, cloud and agent tooling

Docker CLI and compose plugin (installed always; *daemon access* is
deployment-selected, §11.2), `gh`, `rclone`, `infisical`, `tailscale`, and
`step` (Smallstep — self-issues short-lived SSH certs against Node B's step-ca,
and is the only host-shell SSH path for in-pod agents).

Agent-facing: `opencode` in every image; `mydevenv2-rust-analyzer-mcp` and
`github-mcp-server` for MCP — registration commands are [`ENGINE.md`](ENGINE.md)
§4. **Codex, Claude and theclawbay are not installed in the production image**;
they are user-managed there. The dev image bakes all three in with
`INSTALL_AI_CLIENTS=true`, and in that trusted pod the system `codex` command
always runs with `--dangerously-bypass-approvals-and-sandbox` (§11.4).

`theclawbay` is baked in rather than left to the home volume, which is where it
used to live: `vogt-dev` binds a different home than `mydevenv2-dev`, so the
tool was absent from the merged stack and nothing reported it (WI-17). All four
dev-image tools — `claude`, `codex`, `flutter`, `theclawbay` — are executed in
the built image by `build.yml` before it is pushed, per NFR-Q7.

### 10.4 Secrets the pod needs at startup

| Secret | Infisical location | Purpose |
|---|---|---|
| `HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY` | `apps` / `prod` | Join the pod to the tailnet at startup |
| `MYDEVENV2_TOKEN` | `apps` / `prod` | The engine's primary bearer token |
| `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` | `apps` / `prod` | Read-only Universal Auth identity for agent shells |
| `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET` | `apps` / `prod` | Secret for that identity |
| `HOMELAB_CADASTRE_HTTP_TOKEN` | `apps` / `prod` | Child-process bearer for the private cadastre MCP endpoint |
| `MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON` | `apps` / `prod` | Optional; empty disables native FCM while web push still works |
| `MYDEVENV2_ASSISTANT_API_KEY` | `apps` / `prod` | Optional; empty hides the assistant entirely (FR-T6) |
| `HOMELAB_VOGT_CORE_TOKEN` | `apps` / `prod` | The core token the front door injects (§9.3) |

VAPID keys for browser web push are **generated by the engine on first use** and
persisted under `state_dir`; they are not injected as stack secrets, which is
part of why `state_dir` must be inside what `vogt backup` covers (NFR-I6).

### 10.5 How credentials reach a child, and never PID 1

The pod fetches service credentials **into child shells** rather than exporting
service tokens from PID 1. Production default sessions are wrapped
automatically; these remain useful for validation:

```bash
mydevenv2-agent-auth check
mydevenv2-agent-auth run -- gh api user
mydevenv2-agent-auth shell
```

The helper fetches `HOMELAB_CADASTRE_HTTP_TOKEN` on demand and exports it only
inside that child. The bearer appears in no Dockerfile, no compose environment,
no command line, no log and no persisted home — the same rule FR-S7 states for
Vogt's own tokens, applied one layer out.

On the first authenticated session it performs an idempotent client bootstrap
when `MYDEVENV2_AUTO_CADASTRE_MCP=1` (the default), registering Codex's native
HTTP transport and pointing Claude Code and OpenCode at
`/usr/local/bin/mydevenv2-cadastre-mcp`, which obtains a fresh token per bridge
process. Existing registrations are preserved. Set it to `0` to disable.

The machine identity needs read-only access to the `cicd`, `infrastructure` and
`apps` Infisical projects, and the helper uses the direct tailnet endpoint
`http://100.92.54.45:8400` to avoid the browser-facing Caddy auth gate.

### 10.6 GUI streaming: installed, not switched on

Sway (headless Wayland), Selkies-GStreamer and Chromium are in the runtime
image. The image records what it actually got in `/etc/mydevenv2/features.json`
— Selkies is installed best-effort and the file says `{"selkies": null}` when it
was unavailable — and the engine serves that through `GET /api/config`, so a
client can tell a missing feature from a broken one.

**Production runs with `START_SWAY=0`, `GUI_STREAM_URL=""` and
`GUI_STREAM_VERIFIED=0`.** The launch APIs remain installed, but the normal
GUI affordances are deliberately hidden. `GET /api/config` owns that
visibility decision as `gui_stream_available`: a configured URL, a non-null
shipped Selkies feature and an operator-set verification bit are all required.
Set the bit only after launching a process and watching it render through the
configured stream; a package manifest is not a runtime proof.
Old/direct `#/gui` links render a named unavailable state. Operability remains
a gap with the requirement and [`REQUIREMENTS.md`](REQUIREMENTS.md) §7 carries
it; hiding the unverified surface does not claim the stream works.

## 11. The engine's own stacks *(consolidated 2026-08-15)*

Replaces `engine/deploy/KOMODO.md`. These are the **standalone** MyDevEnv2
stacks — the engine deployed without a Vogt core — which predate the merge and
are scheduled for retirement in the order §9.5 gives. They are documented here
rather than deleted because they are still running, and because `dev-mydevenv2`
is where several engine behaviours were validated.

| Stack | Ops path | Port | Host | Image |
|---|---|---|---|---|
| `prod-mydevenv2` | `personal/mydevenv2/` | 8910 | `mydevenv2.sprooty.com` | `repo.indexarr.net/indexarr/mydevenv2:<sha>` |
| `dev-mydevenv2` | `personal/mydevenv2-dev/` | 8911 | `mydevenv2-dev.sprooty.com` | `…/mydevenv2:dev-<sha>` |

Both are Komodo stacks on Node B, desired state in `indexarr/ops`, Caddy in
front, built by Woodpecker rather than by this repository's GitHub Actions —
and the pipeline that builds them lives in the pre-merge Forgejo repository
`indexarr/MyDevEnv2`, which is where Woodpecker's repository list points. It
is not in this tree. A vendored copy of it sat under `engine/.woodpecker/`
from the merge until it was deleted; reading that copy to
understand these stacks was always reading the wrong file, since nothing here
ever triggered it and the forge's copy is the one that has been changing.
**The merge did not merge the deployments**: a change under `engine/` does not
reach these stacks by merging or tagging this repository, and the reverse is
equally untrue.

```text
push to dev  -> build-and-push-dev  -> :dev / :dev-<sha>  -> personal/mydevenv2-dev  -> dev-mydevenv2
push to main -> build-and-push      -> :latest / :<sha>   -> personal/mydevenv2      -> prod-mydevenv2
```

Both ops paths live on the ops repository's **`main`** branch, including the dev
one. That is not symmetric with this repository's dev/main split, and the reason
is mechanical: `ops/scripts/komodo-deploy.sh` clones ops's default branch
unconditionally with no branch parameter, so any stack it manages has to sit on
`main`. An ops `dev` branch was tried first and abandoned for exactly this.
Komodo's own periphery pull is unaffected either way — it reads the stack's own
`branch` config directly.

Manual redeploy of the currently pinned image is safe when Komodo needs to
recreate a stack without a new image: use `ops/scripts/komodo-deploy.sh` with
`IMAGE_TAG` set to the tag already pinned, or call Komodo `DeployStack`
directly. **Never invent an SSH or `docker compose up` deploy step** (NFR-D7).

### 11.1 Recreating a stack from nothing

Copy `engine/deploy/docker-compose.yml` (and the socket overlay, if wanted) to
`indexarr/ops` under the stack's path, mint the secrets §10.4 lists, then create
the stack through the Komodo API with `server_id: node-b`, `git_provider:
repo.indexarr.net`, `repo: indexarr/ops`, `branch: main`, `run_directory` set to
the ops path, `file_paths` listing the compose files, and the runtime values in
`environment`. Komodo does not read Infisical: it writes its environment field
into a `.env` at deploy time, which is why the compose rejects empty identity
values and the entrypoint validates credential retrieval before starting the
server. Then `DeployStack` once by hand; CI handles redeploys after that.

Caddy on Node B routes the host to the port:

```caddyfile
mydevenv2.sprooty.com { reverse_proxy localhost:8910 }
```

Tailnet liveness and readiness are reachable without Caddy at
`http://100.92.54.45:8910/healthz` and `http://100.92.54.45:8910/readyz`.

### 11.2 The Docker socket is a trust decision, not a default

The base compose is **socketless on purpose**.
`engine/deploy/docker-compose.docker-socket.yml` is an overlay that mounts
`/var/run/docker.sock` and adds the host socket group (`DOCKER_SOCKET_GID`,
currently `984` on Node B — it must match `stat -c %g /var/run/docker.sock`).

Production uses the overlay, because authenticated agent sessions and release
workflows rely on host-daemon access. Understand what that buys: **any shell or
API flow that can drive Docker inside the pod can control the host daemon.**
Scoped non-admin API tokens reduce the HTTP blast radius and do nothing about an
interactive shell. For personal homelab use that may be acceptable; for anything
shared, keep the socketless base or interpose a socket proxy.

### 11.3 Disk layout, and the incident behind it

Prod bind-mounts `home` and `tailscale` under `/mnt/2tnvme/docker/volumes/` and
**does not bind-mount `/tmp` at all**. That gap is why prod's `/tmp` silently
accumulated ~184 GB of stale build and test scratch inside the writable layer on
the root disk, contributing to a near-full-root-disk incident on Node B.

The dev stack validates the fix before prod adopts it:

| Inside the container | Host path | Disk |
|---|---|---|
| `/home/sprooty` | `/mnt/sdg/mydevenv2-dev/home` | `sdg3`, ext4, ~457 G |
| `/var/lib/tailscale` | `/mnt/sdg/mydevenv2-dev/tailscale` | same |
| `/tmp` | `/mnt/sdg/mydevenv2-dev/tmp` | same |
| `/home/sprooty/Working` | `/mnt/2tnvme/docker/volumes/mydevenv2/workspace` | **same as prod, deliberately** |

The workspace mount is deliberately *not* moved: it is the same live `~/Working`
data prod uses — two PTY servers against the same files is no different from two
terminal windows — and migrating a live, actively-edited dataset is a separate
and higher-stakes step from moving per-stack state and scratch. When prod adopts
the layout, note whether it moves to `sdg` alongside dev or gets its own disk;
`sdg` was sized for dev-only validation.

**`init: true` is load-bearing.** The server execs as PID 1 and only `wait()`s
on children it spawns directly, so orphans reparented to it are never reaped.
Agent sessions fork many short-lived subprocesses, and on 2026-08-07 this
exhausted `mydevenv2-dev`'s pids limit at ~106k zombies — the container could not
fork anything, including its own diagnostic `ps`. Docker's built-in `tini` takes
the PID 1 slot and reaps them for free. Both compose templates set it; prod's
copy in the ops repository still needs the line.

### 11.4 Codex full-access mode, in the dev pod only

The dev pod *is* the isolation boundary, so its image exposes `codex` through
`engine/deploy/codex-full-access.sh`, which always starts the real CLI with
`--dangerously-bypass-approvals-and-sandbox`. That gives Codex the same
filesystem and network reach as the container user, across every repository
under `/home/sprooty/Working`, and a persisted user config cannot silently
narrow it back to one launch directory.

Do not add repository-specific `--add-dir` entries or rely on Codex project
trust for this deployment; neither solves cross-repository release work.
Existing chats keep the policy they started with, so validate with a new chat
after each image deployment. The old nested-Bubblewrap configuration is no
longer needed — remove `seccomp=unconfined` and `apparmor=unconfined` from the
dev stack when this image deploys.

### 11.5 Workspace synchronisation

The workspace-root `mutagen.yml` targets Node B at
`sprooty@100.92.54.45:/mnt/2tnvme/docker/volumes/mydevenv2/workspace`. Do not
target the retired MyDevEnv v1 endpoint. Synchronisation is handled outside the
app; nothing in the engine or the core performs it.

## 12. Reading the logs *(r19)*

Written the day the product could not diagnose itself. On 2026-08-19 the GUI
took fifteen seconds to load and `/health/ready` intermittently exceeded a
25-second timeout on a request that does no database work, and answering
*which endpoint is slow* took host metrics, container metrics, a hand count of
three thousand access-log lines and a read of the PWA's source (#139).
NFR-OB1–OB5 exist so the next one is answerable from the logs alone.

### 12.1 What a line looks like

Both processes log to **stderr**, so `docker logs` is the whole story and
`vogt-mcp`'s JSON-RPC stdout stays a protocol stream rather than a log.

The core, in the default text rendering:

```text
2026-08-19T11:02:03.418+00:00 INFO    vogt.http request request_id=8f2c1ab34d90e771 method=GET path=/api/backlog status=200 duration_ms=812.4 ttfb_ms=811.9 bytes=1841 query=limit=1 client=100.109.218.11 actor=agent:vogt-dev
```

`VOGT_LOG_FORMAT=json` renders the same fields as one JSON object per line,
for a log that is queried rather than read. **Two durations**, deliberately:
`ttfb_ms` is time to the response starting and is what says an endpoint is
slow; `duration_ms` is the whole exchange. The slow-request warning is judged
on the first, so a long-lived `/mcp` stream does not report itself as
pathological every time a client disconnects.

The engine's line is `tracing`'s, with the same field names:

```text
2026-08-19T11:02:03.402Z  INFO request: request_id=8f2c1ab34d90e771 method=GET path=/api/vogt/backlog status=200 duration_ms=815
```

### 12.2 One request, across both runtimes

The engine assigns the id — the caller's `X-Request-Id` when it is a usable
identifier, one it mints when it is not — puts it in its own line, sends it to
the core on the proxied request, and returns it to the client. The core
attaches it to **every** line it writes while serving that request, not only
the access line. So a slow answer can be quoted in a bug report:

```bash
curl -sS -D- -o/dev/null https://vogt-dev.sprooty.com/api/vogt/backlog?limit=1 \
  -H "authorization: Bearer $TOKEN" | grep -i x-request-id
docker logs vogt-dev 2>&1 | grep 8f2c1ab34d90e771
```

A caller may state the id, which is how a script correlates a batch. It is
checked before it is logged — identifier characters, 64 bytes — and replaced
when it is not, because a value that reaches a log line unchecked is a
log-injection primitive.

### 12.3 The knobs, and what they are for

| Setting | Default | What it decides |
|---|---|---|
| `VOGT_LOG_LEVEL` | `info` | Verbosity of the `vogt.*` namespace. Dependencies are left where they are, so `debug` is a decision about Vogt. |
| `VOGT_LOG_FORMAT` | `text` | `json` for Loki; identical fields either way. |
| `VOGT_LOG_REQUESTS` | `true` | The access line itself. Off still honours and echoes correlation ids. |
| `VOGT_LOG_SLOW_REQUEST_MS` | `1000` | Above this, `WARNING` instead of `INFO`. |
| `VOGT_LOG_QUIET_PATHS` | probes | Paths dropped to `DEBUG`. |

The engine takes its level from `RUST_LOG` as it always has; its quiet paths
and slow threshold are constants in `engine/server/src/observability.rs`.

**Probe suppression is not probe silence.** `/health/live`, `/health/ready`
and `/version` log at `DEBUG` because an orchestrator calls them every few
seconds forever, and a log that is 100% `/healthz` with no application output
in it is not a log — a neighbouring container on this host produced exactly
that, three thousand consecutive lines of it. But the slow check runs first,
so the `/health/ready` that took 25 seconds still warns. That was the single
most diagnostic fact of the incident and suppressing it would defeat the
purpose of having the setting.

### 12.4 Answering "which endpoint was slow at 11:30"

```bash
# Everything the core thought was slow or failed, with its duration:
docker logs vogt-dev --since 11:25 2>&1 | grep 'WARNING vogt.http'

# The endpoints by request volume — the count that found #138:
docker logs vogt-dev --since 1h 2>&1 | grep 'vogt.http request' \
  | sed -n 's/.*path=\([^ ]*\).*/\1/p' | sort | uniq -c | sort -rn | head
```

With `VOGT_LOG_FORMAT=json` both of those are a `jq` filter or a LogQL query
instead, which is the reason the format exists.
