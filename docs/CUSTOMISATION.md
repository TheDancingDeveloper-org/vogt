# Customising Vogt

Vogt ships as a generic product: one Python core image, one Compose base, and
a configuration schema that decides everything an operator is allowed to
decide. It is also meant to be customised heavily. This document names the
supported extension points, so that a large customisation is a documented
deployment rather than a fork.

The rule underneath all of it: **the base is never edited.** If you find
yourself changing [`deploy/vogt.compose.yml`](../deploy/vogt.compose.yml) or
the root `Dockerfile` to make your deployment work, that is a gap in this
document — please raise it.

## Three layers

| Layer | Use it for | Cost of an upgrade |
|---|---|---|
| **Configuration** | Anything the schema already decides | None. New image, same settings. |
| **Compose overlay** | Extra services, mounts, networks, ports, secrets | None. Your overlay states only differences. |
| **Image extension** | Extra binaries, certificates, collectors | A rebuild against the new base tag. |

Reach for them in that order. Most customisations stop at the first.

## Layer 1 — configuration

Every setting lives in one schema, `src/vogt/config.py`.
[`CONFIG.md`](CONFIG.md) and `config.example.toml` are *generated* from it —
never hand-edit them; run `uv run python scripts/gen_config_docs.py` and
commit the result.

Precedence, highest first:

1. explicit command-line arguments
2. `VOGT_*` environment variables
3. the TOML file named by `VOGT_CONFIG_FILE`
4. schema defaults

Each setting is classified by what it decides, and the class tells you
whether it can carry a default:

- **exposure** — an address, a port, a URL a client will trust. Never
  defaulted anywhere, in code, images, docs or examples. You must state it.
- **allocation** — a path or a slot on a host you own. Always defaulted, so a
  deployment never fails merely because the example did not know your
  filesystem layout.
- **behaviour** — tuning that decides neither. Unconstrained.

`tests/test_config.py` asserts the first two rules against the live schema, so
this is a property of the product rather than a convention.

### Settings worth knowing about

**Storage and identity.** `data_dir` holds both SQLite stores and the
backups; one instance per directory. `import_root` is where imported
repositories are cloned, defaulting to `<data_dir>/repos` — see
[Observing an estate](#observing-an-estate-on-a-host-path) before pointing it
elsewhere.

**What the contract means.** `contract_required_files`,
`contract_required_dirs` and `contract_required_meta` define the project
contract `contract check` reports against. This is a value you read, never a
barrier you pass — changing it changes what is reported and gates nothing. If
you change the rules and leave `contract_version` alone, Vogt appends a digest
of your rules to the version, so a recorded status can never claim to be the
stock `v1` when it is not.

**What counts as work.** `marker_promotion_patterns` decides which source
markers enter backlog and bug views; the default is `TODO(vogt):`. Every other
marker is still observed, still queryable, still counted — it just does not
claim to be work. Widening this is how you drown the ranked view.
`marker_file_extensions` decides which files the marker collector reads,
because which extensions hold source is your estate's business, not Vogt's.

**Collection cadence.** `sweep_interval_seconds`, `verify_horizon_hours` and
`retention_days`. Collection scope is always the projects you registered —
there is no setting that makes Vogt crawl, and that is deliberate.

## Layer 2 — Compose overlays

The base file is designed to be layered on. Compose merges files
left-to-right, so an overlay states only its difference:

```console
docker compose -f deploy/vogt.compose.yml -f my-deployment.yml up -d
```

The repository ships the smallest possible example of this,
[`deploy/vogt.build.yml`](../deploy/vogt.build.yml), which does nothing but
swap the published image for a build from the checkout.

An overlay is the right place for extra services, host mounts, a different
network, additional secrets, TLS material, or a reverse proxy. Keep your
overlay in your own repository beside the rest of your deployment
configuration — publishing an image and moving a deployment stay separate
acts, and a digest or configuration change is what moves one.

### Observing an estate on a host path

Vogt's collectors read the repositories you register, so a deployment that
observes a host directory has to mount it into the container *and* be able to
read it:

```yaml
services:
  vogt:
    user: "1000:0"          # the uid that owns the files being observed
    environment:
      VOGT_IMPORT_ROOT: /workspace/repos
    volumes:
      - /srv/repos:/workspace:rw
```

The uid matters. The image runs as any uid provided the gid is 0 — that is
why `/var/lib/vogt` is owned by group 0 and group-writable — but a bind mount
or a fresh named volume carries the *host's* ownership, not the image's. A
directory the container user cannot write to will fail at `project import`
rather than at startup, which is a slow way to find out.

### Putting your own front door in front of Vogt

Vogt supports running behind something that publishes it at a different
address and different mount points. Set `fronted = true`, and your front door
states the identity per request with three headers:

- `X-Vogt-Public-Url`
- `X-Vogt-Api-Path`
- `X-Vogt-Mcp-Path`

`connect` and `/connection-info` then render client configuration against what
the door says, because the door is the only thing that knows where clients
arrive. This is off by default and never inferred: an instance that has not
been told it is fronted ignores those headers entirely, so nobody who can
reach it can make `connect` render a configuration document against an address
they chose.

The two-service shape this enables — your front door on the published port,
the core reachable only on the Compose network — is a supported topology, not
a workaround.

## Layer 3 — extending the image

The published image is built to be a base:

- `ENTRYPOINT ["vogt"]` with `CMD ["--help"]`, so it has no default listen
  address to inherit;
- runs as any uid with gid 0;
- `/var/lib/vogt` owned by `root:0`, mode `0770`;
- the virtualenv at `/opt/vogt/.venv`, already on `PATH`;
- `git` installed, because `project import` and the git collector shell out
  to it.

```dockerfile
FROM ghcr.io/thedancingdeveloper-org/vogt:v0.2.0
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends ripgrep \
    && rm -rf /var/lib/apt/lists/*
USER 1000:0
```

Keep the uid/gid contract and the data directory's ownership; everything else
in the image is yours to add to. Preserve `ENTRYPOINT` unless you have a
reason — the absence of a default host and port is a safety property, not an
oversight.

## Optional integrations

Each of these is absent by default, and its absence is reported honestly
rather than being fatal. A Vogt installation with all of them off is a
complete, supported product.

| Integration | Turned on by | What its absence means |
|---|---|---|
| **GitHub** | `github_token_file` | "GitHub was not collected", never "there are no GitHub subjects" |
| **MCP** | running `vogt-mcp`, or `/mcp` on a running server | Agents cannot connect; nothing else changes |
| **Remote MCP** | `vogt-mcp-remote` with `VOGT_URL` and `VOGT_TOKEN_FILE` | As above |
| **Session engine** | `engine_url`, `engine_token_file`, `engine_state_dir` | `session.*` operations report that no engine is configured; nothing else is affected |

Tokens are always given as a *file path*, never as a value in the
environment: a token in the environment is a token in every `docker inspect`.

## What is not an extension point

- **The generated configuration reference.** `CONFIG.md` and
  `config.example.toml` come from the schema. Editing them to hide a schema
  change is the one thing CI will not let you do.
- **The operation registry.** The CLI, REST and MCP surfaces are generated
  from one registry, and parity between them is asserted by tests. Adding a
  capability to one adapter alone is not a customisation, it is a bug.
- **The audited write path.** Every write requires a principal and a reason.
  There is no setting that turns this off.
- **Collection scope.** Vogt observes the projects you registered. Nothing
  widens that to a filesystem crawl.

## A worked example

The maintainer's own deployment is the largest customisation that exists: a
Rust session engine as the front door, the core detached behind it, a
tailnet-only published port, a secret broker, and a pod full of agent
tooling. It is built entirely from the extension points above — front door,
detached core, host mounts, uid selection, optional integrations — and is
documented in [`DEPLOYMENT.md`](DEPLOYMENT.md) as the reference customisation
rather than as the way Vogt is meant to be run.

You can read it as a diff. [`deploy/estate.overlay.yml`](../deploy/estate.overlay.yml)
is that deployment expressed against this same base:

```console
docker compose -f deploy/vogt.compose.yml -f deploy/estate.overlay.yml up -d
```

It adds one service and some configuration. It does not rebuild, repin, or
restate the core — that is the published image, unmodified, which is what
makes "the private path is the public path plus configuration" a claim you
can check rather than one you have to believe.

If your deployment needs something none of these layers reach, that is worth
an issue. The generic base is only generic if the customisations people
actually need are supported ones.
