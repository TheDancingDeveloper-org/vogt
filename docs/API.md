# API, MCP, and the permissions model

Vogt exposes **one operation registry** (`src/vogt/registry`) through three
transports — HTTP, the CLI, and MCP — sitting behind a session **engine / front
door**. Every capability is generated from that one registry, so the three
transports never drift.

The endpoints themselves are self-describing (FastAPI emits OpenAPI), but *which
process, which port, and which token* was never written down in one place. This
is that map. Read it before reaching for a token — a request to the wrong
surface, or with the wrong bearer, is a `401`/`404` that looks like a bug and
is not.

## The two processes

| Process | What it is | Port | Serves |
|---|---|---|---|
| **Core** (`vogt serve`) | The tracker: projects, work items, tokens, the store. A FastAPI app. | `8000` (container) | `/api/*` operations, `/mcp`, and the generated docs `/openapi.json`, `/docs`, `/redoc` |
| **Engine / front door** | The session + PWA host that fronts the core in a deployment. A Rust server. | e.g. `8910` on the estate | the PWA, `/api/status`, `/api/auth/check`, `/api/sessions`, `/healthz`, `/readyz`, the assistant — **and it proxies `/api/vogt/*`, `/mcp`, `/api/auth/login` and `/api/install/*` through to the core** |

In a deployed stack the engine is the front door: the core is not exposed
directly, and callers reach it through the engine's `/api/vogt` and `/mcp`
proxies (`engine/server/src/app.rs`, `MACHINE_NAMESPACES = ["/api", "/mcp"]`).
Everything else on the engine is the engine's own.

## Three ways to call one operation

1. **HTTP** — on the **core**, at `{API_PREFIX=/api}/<operation route>`. Because
   the core is FastAPI, you get for free (FR-A4):
   - **`/openapi.json`** — the machine-readable spec.
   - **`/docs`** — interactive Swagger UI.
   - **`/redoc`** — ReDoc.

   These live at the **core root** (`http://<core>:8000/docs`), *not* under
   `/api`, and the front door proxies `/api/vogt`, not `/docs` — so Swagger is
   normally reachable only from inside the deployment.
2. **CLI** — `vogt <group> <op>` operates on the **local store directly**, with
   no bearer. Inside the core container that store *is* the core's store, and
   loopback access is effectively admin — which is how `vogt token issue` mints a
   token when no admin bearer exists. (A pod wired to a remote core still defaults
   to the local store and will say `not_initialized`; set `VOGT_CORE_URL` +
   `VOGT_HTTP_TOKEN` and call the HTTP API instead — see the CLI hint added in
   #729.)
3. **MCP** — the same operations as tools over `/mcp` (JSON-RPC, streamable
   HTTP), proxied by the engine to the core. The `vogt-mcp-remote` stdio bridge
   fronts it for agent clients (`src/vogt/adapters/mcp/`).

## Tokens — which bearer each surface accepts

The core is the only identity authority. Every credential a client holds is a
**core token** bound to an actor, and the engine accepts all of them: it keeps
no token table of its own and resolves a bearer by asking the core
(`GET /api/auth/whoami`, cached for seconds), so one credential opens both
doors. The one exception is the engine's optional break-glass `ENGINE_TOKEN`,
a static credential the core has never heard of: **it 401s the core**, and on
`/api/vogt` the engine substitutes the stack secret for it.

| Token | `tokens.kind` | Authenticates | Typical scopes | Where it comes from |
|---|---|---|---|---|
| **Session** (a person's login) | `session` | the **core** — `/api/*`, `/mcp` — and the **engine** | the scopes on the login: `read,work.write,project.write` by default, `admin` for the first operator | `POST /api/auth/login` with a username and password; expires after `session_ttl_days` (30) or at `auth.logout` |
| **API token** (an agent's credential) | `api` | the core and the engine | as issued | `vogt token issue` (admin) |
| **Stack secret** (`deploy/vogt-core-token`) | `api` | the core, as the front-door actor; the engine, as `vogt-core` (`sessions`, `agent-clis-write`) | `VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES` | the file both halves read, adopted at `init` (#199); server-side only, never in a browser |
| **Coding-session token** (`session.start`) | `agent` | the core, per session | `agent_session_scopes` — default everything except `admin` | minted per session (#726) |
| **Brokered agent token** | — | the core, as a shell/agent session's `VOGT_HTTP_TOKEN` | deployment-chosen | `ENGINE_AGENT_AUTH_VOGT_SECRET_NAME`, brokered from the secrets manager at session launch |
| **Break-glass token** (`ENGINE_TOKEN`, optional) | none | the **engine only** — full capability, no actor | — | `deploy/.env`; its Vogt calls are made with the stack secret |

Concretely: `GET /api/auth/check` on `:8910` accepts a session or an API
token and answers with `identity: {name, scopes, capabilities}` — the core
actor's `identity_ref`, or `primary` / `vogt-core` for the two static
credentials — and `/api/vogt/*` forwards the same bearer to the core, so
`POST /api/vogt/projects` is audited to the person or agent who sent it. A
`401` from the engine means nobody recognises the bearer; a `503` means the
core could not be asked, which is an outage rather than a wrong credential,
and the engine says so instead of collapsing the two.

## The permissions model (scopes)

Every core operation passes two gates (FR-S5): **authenticate** (who is this)
then **authorize** (may they do it), and the decision is recorded either way.
Scopes are **instance-wide** — an agent with `work.write` can write to every
project (DESIGN.md §4.1).

| Scope | Gates |
|---|---|
| `read` | every read |
| `work.write` | work-item writes — create, transition, comment |
| `project.write` | project register/import and project-level writes |
| `writeback` | exactly `forge.writeback` (arming forge write-back) |
| `admin` | token minting, actor creation, password logins (`user.*`), and instance ops — `init`, `migrate`, `backup`, `restore`, `serve` |

- The default session scope set is **`read,work.write,project.write,writeback`**
  (`agent_session_scopes`, #726) — everything except `admin`, applied to every
  session however it was launched. Narrow it in config only if this instance
  truly wants to.
- **Loopback is admin.** The CLI acting on the core's own store bypasses the
  bearer gate, so `vogt token issue` inside the core container is the way to mint
  or widen a token when no admin bearer is available.
- **A `403` names the requirement and the holding.** `"project.register requires
  the 'project.write' scope; this token holds read, work.write, writeback"` — the
  fastest way to read a token's actual grants live. `auth.whoami` (`GET
  /api/auth/whoami`, `vogt auth whoami`) is the other: it answers with the
  effective scope set, implications applied.
- **The engine derives its capabilities from these scopes.** A bearer the core
  resolves gets, on the engine, what its scopes imply
  (`auth::capabilities_for_scopes` in `engine/server/src/auth.rs`): `admin`
  holds all eleven capabilities; `work.write` or `project.write` holds
  everything except `gui-control` and `agent-clis-write`; `read` alone holds
  only `push-write`, so a viewer can subscribe to notifications; `writeback`
  adds nothing there. There is no separate engine grant to ask for.

## Quick reference

```text
# Core (inside the deployment)
GET  http://<core>:8000/api/status            # core token
GET  http://<core>:8000/docs                   # Swagger UI (internal)
GET  http://<core>:8000/openapi.json           # raw spec

# Engine / front door
GET  http://<engine>:8910/api/status           # any core token (session or API), or ENGINE_TOKEN
GET  http://<engine>:8910/readyz               # unauthenticated health
POST http://<engine>:8910/api/auth/login       # {username, password} -> {actor, token, secret}; unauthenticated
GET  http://<engine>:8910/api/vogt/auth/whoami # who this bearer is, with effective scopes
POST http://<engine>:8910/api/vogt/auth/logout # revoke the bearer this call carries
POST http://<engine>:8910/api/vogt/...         # proxied to the core, caller's bearer forwarded
POST http://<engine>:8910/mcp                  # proxied to the core (MCP)

# People (admin, via loopback in the core container); the password is read
# from stdin, a file, or a hidden prompt — never from argv
docker exec -i <core-container> \
  vogt user create --username ada --display-name "Ada Lovelace" \
  --scopes read,work.write,project.write --password-stdin --reason "<why>" < ada-password
docker exec -i <core-container> vogt user passwd --username ada --password-stdin --reason "<why>" < new-password
docker exec <core-container> vogt user list
docker exec <core-container> vogt user remove --username ada --reason "<why>"

# Agents: mint / widen an API token (admin, via loopback in the core container)
docker exec <core-container> \
  vogt token issue --actor <ref> --name <n> --scopes read,work.write,project.write,writeback --reason "<why>"
```

## See also

- `DESIGN.md` §4 — the security model (FR-S*), scopes, and per-project scope
  deferral.
- `ENGINE.md` — the engine, the agent-auth broker, and identity passthrough.
- `CONFIG.md` — generated config reference (`agent_session_scopes`,
  `session_ttl_days`, `bootstrap_*_token_*`, `ENGINE_AGENT_AUTH_*`).
