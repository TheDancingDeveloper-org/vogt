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
| **Engine / front door** | The session + PWA host that fronts the core in a deployment. A Rust server. | e.g. `8910` on the estate | the PWA, `/api/status`, `/api/auth/check`, `/api/sessions`, `/healthz`, `/readyz`, the assistant — **and it proxies `/api/vogt/*` and `/mcp` through to the core** |

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

Different surfaces take different tokens. This is the trap: **a core token 401s
the engine, and an engine token 401s the core.**

| Token | Authenticates | Typical scopes | Where it comes from |
|---|---|---|---|
| **Core token** (bootstrap / front-door) | the **core** — `/api/*`, `/mcp` | `read,work.write,project.write` | `VOGT_BOOTSTRAP_CORE_TOKEN_FILE`, adopted at `init` (#199) |
| **Engine token** (`ENGINE_TOKEN`) | the **engine** — `/api/status`, `/api/auth/check`, `/api/sessions` | — (engine gate, not core scopes) | the front door's own bearer |
| **Session token** (`session.start`) | the **core**, per session | `agent_session_scopes` — default everything except `admin` | minted per session (#726) |
| **Brokered agent token** | the **core**, as a shell/agent session's `VOGT_HTTP_TOKEN` | deployment-chosen | `ENGINE_AGENT_AUTH_VOGT_SECRET_NAME`, brokered from the secrets manager at session launch |

Concretely: `GET /api/status` and `/api/auth/check` on `:8910` want the
`ENGINE_TOKEN` and return the *engine's* status; `POST /api/projects`,
`/api/work/*`, etc. on the core want a core or session token. Sending one to the
other is the `401` that looks like a broken token and is only a wrong door.

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
| `admin` | token minting, actor creation, and instance ops — `init`, `migrate`, `backup`, `restore`, `serve` |

- The default session scope set is **`read,work.write,project.write,writeback`**
  (`agent_session_scopes`, #726) — everything except `admin`, applied to every
  session however it was launched. Narrow it in config only if this instance
  truly wants to.
- **Loopback is admin.** The CLI acting on the core's own store bypasses the
  bearer gate, so `vogt token issue` inside the core container is the way to mint
  or widen a token when no admin bearer is available.
- **A `403` names the requirement and the holding.** `"project.register requires
  the 'project.write' scope; this token holds read, work.write, writeback"` — the
  fastest way to read a token's actual grants live.

## Quick reference

```text
# Core (inside the deployment)
GET  http://<core>:8000/api/status            # core token
GET  http://<core>:8000/docs                   # Swagger UI (internal)
GET  http://<core>:8000/openapi.json           # raw spec

# Engine / front door
GET  http://<engine>:8910/api/status           # ENGINE_TOKEN
GET  http://<engine>:8910/readyz               # unauthenticated health
POST http://<engine>:8910/api/vogt/...         # proxied to the core (core token)
POST http://<engine>:8910/mcp                  # proxied to the core (MCP)

# Mint / widen a token (admin, via loopback in the core container)
docker exec <core-container> \
  vogt token issue --actor <ref> --name <n> --scopes read,work.write,project.write,writeback --reason "<why>"
```

## See also

- `DESIGN.md` §4 — the security model (FR-S*), scopes, and per-project scope
  deferral.
- `ENGINE.md` — the engine, the agent-auth broker, and identity passthrough.
- `CONFIG.md` — generated config reference (`agent_session_scopes`,
  `bootstrap_*_token_*`, `ENGINE_AGENT_AUTH_*`).
