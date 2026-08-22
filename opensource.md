# Public boundary and compatibility policy

This is the short statement of what the open-source Vogt delivery supports,
what is optional, and which legacy names are still honoured. The reasoning
behind the product itself is in [`docs/DESIGN.md`](docs/DESIGN.md); how to run
it is in [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) and
[`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).

## Identity

Vogt is the only product name. The canonical source is
`https://github.com/TheDancingDeveloper-org/vogt`; the published container is
`ghcr.io/thedancingdeveloper-org/vogt`. No image or local installation assumes
a remote endpoint: anything that encodes exposure — a public URL, a bind
address beyond loopback — must be set by the operator.

## What is supported

The supported public artefact is the **Python core**: the `vogt` package and
CLI, the FastAPI listener (REST, GUI, health, optional MCP transport), SQLite
storage with migrations and backups, the local collectors, and the optional
GitHub adapter. It is built from the repository-root `Dockerfile` and run from
`deploy/vogt.compose.yml` (published image) or that file plus
`deploy/vogt.build.yml` (build from the checkout). Building or running the
core needs Python 3.11+ and nothing else — no Rust, Cargo, Node, pnpm, a forge
token, an AI provider, or an MCP client.

A core with every optional integration absent is a complete, supported
product: it starts, passes `/health/ready`, and performs every core workflow.
Absence is reported as "not collected" or "not configured", never as an error
or an empty result pretending to be a true one.

## What is optional

| Component | Status | Where it is documented |
|---|---|---|
| GitHub adapter | optional; enabled by `VOGT_GITHUB_TOKEN_FILE` or `VOGT_FORGE_TOKEN_FILES` | [`docs/CONFIG.md`](docs/CONFIG.md), [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) |
| MCP (`vogt-mcp`, `vogt-mcp-remote`) | optional; ships in the package, never required for startup or health | [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) |
| Rust session engine + PWA (`engine/`, `web/`) | optional; built from source with `engine/Dockerfile`; not in the core image | [`docs/ENGINE.md`](docs/ENGINE.md) |
| Voice/chat assistant | optional; part of the engine; any OpenAI-compatible chat endpoint | [`docs/ENGINE.md`](docs/ENGINE.md) §6 |
| Mobile shell (`mobile/`) | optional; a Capacitor WebView over a deployed PWA | [`docs/ENGINE.md`](docs/ENGINE.md) |
| External MCP integrations (e.g. Cadastre) | optional; configured in your agent client, not in Vogt | — |

The engine and mobile shell are real parts of the project and are maintained
in this repository, but they are a separate toolchain and a separate image.
The core must never come to require them; they consume the core's HTTP
adapter like any other client, and the core reaches the engine only through
`VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, and `VOGT_ENGINE_STATE_DIR`, all
of which default to unset.

The public Compose example and Dockerfile must stay self-contained: no private
registries, absolute home-directory paths, secret brokers, VPN assumptions, or
external MCP services. Tests in `tests/test_public_delivery.py` pin this.

## Compatibility policy

- **Operations.** The operation registry is the contract. An operation is
  reachable through CLI, REST, and MCP with tested parity; removing or
  renaming one is a breaking change and is recorded in the release notes.
- **Storage.** Schema changes ship as forward-only migrations. `vogt migrate`
  applies them; `/health/ready` reports schema drift but never migrates on
  its own. Keep a `vogt backup` and the previous image until the new build's
  readiness check passes.
- **Configuration.** Every core setting is a `VOGT_*` environment variable,
  a key in the TOML file named by `VOGT_CONFIG_FILE`, or an explicit
  argument, in that precedence (lowest to highest). `docs/CONFIG.md` and
  `config.example.toml` are generated from `src/vogt/config.py`, and CI fails
  if they drift, so the reference cannot quietly hide a change.
- **Images.** A tag publishes an image; deploying it is a separate act of
  pinning a digest in your own configuration. Publishing never moves a
  running instance.

## Legacy names accepted as aliases

Vogt absorbed an earlier session-engine project, and a handful of its names
survive as compatibility aliases so existing installations keep working
across an upgrade. They are aliases, not the documented way to do anything
new; public examples use Vogt names only.

- **Engine environment variables.** The engine reads `ENGINE_*`. If an
  `ENGINE_` name is unset and the matching legacy `MYDEVENV2_*` name is set,
  the legacy value is used and a startup warning names both. Rename at your
  convenience; the fallback is scheduled for removal.
- **PWA install identity and storage keys.** The web manifest, service-worker
  cache name, and two browser custom-event names keep their original
  literals so an installed PWA upgrades in place instead of appearing as a
  second app. These are pinned in `tests/test_product_identity.py` and are
  not user-visible.
- **Engine state directory.** An existing engine state directory is still
  read from its old location when the operator points
  `VOGT_ENGINE_STATE_DIR` at it; nothing relocates data silently.

Removing any alias is a documented, versioned change with a migration note,
never a silent rename.

## Where private material goes

Operator-specific notes, requirements history, and deployment records that
are not part of the public product belong in the git-ignored `docs/local/`.
Nothing in the repository may link to them, and nothing a public user needs
to run Vogt may live only there.
