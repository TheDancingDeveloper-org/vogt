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

The supported public product is the **two-container stack** (this replaces the
earlier boundary that named the Python core alone as the artefact):

- the Python **core** — the `vogt` package and CLI, the FastAPI listener (REST,
  health, optional MCP transport), SQLite storage with migrations and backups,
  the local collectors, and the optional GitHub adapter; and
- the Rust **engine** — which serves the Solid PWA at `/`, owns the terminal
  sessions a work item runs in, hosts the voice assistant, and fronts the core
  on one published port.

The core is built from the repository-root `Dockerfile` and run from
`deploy/vogt.compose.yml` (published image) or that file plus
`deploy/vogt.build.yml` (build from the checkout). The engine is built from
`engine/Dockerfile`; the generic `deploy/engine.overlay.yml` builds it beside
the core, while tagged releases also publish the signed combined
`vogt-stack` image for the maintainer's development-pod deployment. That
combined image is a published convenience, not the generic supported
self-hosting path. Building the core needs Python 3.11+ and nothing else;
building the engine needs its Rust and Node toolchain, which its Dockerfile
provides, so an operator with Docker needs neither installed.

The core is also a complete, supported product **on its own**, over the CLI,
REST, and MCP: run without the engine it starts, passes `/health/ready`, and
performs every core workflow — no Rust, Node, forge token, AI provider, or MCP
client required. What the core alone does not serve is the browser experience —
the PWA, terminals, and assistant — which is the engine's half of the stack.
Absence of any optional integration is reported as "not collected" or "not
configured", never as an error or an empty result pretending to be a true one.

## What is optional

Beyond the two halves above, these add capability when configured and are
absent-safe when not:

| Component | Status | Where it is documented |
|---|---|---|
| GitHub adapter | optional; enabled by `VOGT_GITHUB_TOKEN_FILE` or `VOGT_FORGE_TOKEN_FILES` | [`docs/CONFIG.md`](docs/CONFIG.md), [`docs/USER_GUIDE.md`](docs/USER_GUIDE.md) |
| MCP (`vogt-mcp`, `vogt-mcp-remote`) | optional; ships in the package, never required for startup or health | [`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) |
| Voice/chat assistant | optional; part of the engine; any OpenAI-compatible chat endpoint | [`docs/ENGINE.md`](docs/ENGINE.md) §6 |
| Mobile shell (`mobile/`) | optional; a Capacitor WebView over a deployed PWA | [`docs/ENGINE.md`](docs/ENGINE.md) |
| External MCP integrations (e.g. Cadastre) | optional; configured in your agent client, not in Vogt | — |

The engine and mobile shell are a separate toolchain and a separate image from
the core, and that separation is load-bearing: the core must never come to
require the engine. The dependency runs one way — the engine consumes the
core's HTTP adapter like any other client, and the core reaches the engine only
through `VOGT_ENGINE_URL`, `VOGT_ENGINE_TOKEN_FILE`, and `VOGT_ENGINE_STATE_DIR`,
all of which default to unset — which is exactly what keeps the core-alone
deployment above a real, supported one.

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
- **PWA compatibility state.** Browser local-storage names have migrated to
  `vogt.*` with one-shot reads of the historical keys. The IndexedDB terminal
  cache, Android notification-channel id, and native-insets event still keep
  their historical internal literals because they cannot be renamed in place
  without losing cached output, user channel settings, or compatibility with
  an installed native shell. These are pinned in product-identity and storage
  migration tests and are not user-visible branding.
- **Engine state directory.** An existing engine state directory is still
  read from its old location when the operator points
  `VOGT_ENGINE_STATE_DIR` at it; nothing relocates data silently.

## Retained compatibility literals

Public examples use Vogt-neutral names, but a few implementation literals are
intentionally retained because they are identities or paths inside a shipped
artifact rather than references to an operator's host:

- **`/home/sprooty` and the `sprooty` container account.** The engine's pod
  image, volume declarations, PATH, persisted home, and ownership defaults
  form an image ABI used by existing volumes and overlays. A rename is a
  versioned container migration, not a documentation cleanup. Host-side paths
  remain variables and the generic deployment never assumes an operator has
  this home directory.
- **`SPROOTY_UID` / `SPROOTY_GID`.** These build arguments select the numeric
  owner of that same retained account and therefore move with the image ABI.
- **`com.sprooty.vogt` and `com.sprooty.vogt.dev`.** These are Android
  application identities registered with Firebase and signing/install state.
  Renaming them would create another application and require users to
  reinstall and re-register for push.
- **`MYDEVENV2_*` engine, task, and Android build names.** These are deprecated
  compatibility inputs or wire values from the pre-merge engine. New public
  configuration uses `ENGINE_*` / `VOGT_*`; the aliases remain until a
  documented breaking release can remove them. The mobile workspace package
  name is not such an identity and has been renamed to `vogt-mobile`.
- **`mydevenv2-alerts`, `mydevenv2-terminal-cache`, and
  `mydevenv2:native-insets`.** Android notification-channel ids and IndexedDB
  databases cannot be renamed in place, and the event remains an accepted
  native-shell wire alias. Keeping these internal ids preserves user settings,
  cached terminal output, and older installed shells while their visible
  surfaces use Vogt naming.

Estate hostnames, private IPs, secret-manager projects, and operator checkout
paths are not compatibility literals. They belong in deployment variables or
private operator material and must not be added to the generic surfaces.

Removing any alias is a documented, versioned change with a migration note,
never a silent rename.

## Where private material goes

Operator-specific notes, requirements history, and deployment records that
are not part of the public product belong in the git-ignored `docs/local/`.
Nothing in the repository may link to them, and nothing a public user needs
to run Vogt may live only there.
