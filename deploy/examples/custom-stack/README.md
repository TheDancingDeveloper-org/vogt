# Worked example: a custom stack image

This is the whole public extension contract (#614) in one place — the exact
shape the private `vogt-dev` / `vogt-prod` derivatives take. You derive
`FROM` the published `vogt-stack` digest, add what your sessions need, and
deploy the result. Everything estate-specific is a line in *your* Dockerfile,
a mount, or an environment value — never a default baked into a file a stranger
clones.

## What it demonstrates

| Extension point | Owned by | In this example |
|---|---|---|
| Tools in the image | the image (`Dockerfile`) | installs `ripgrep` `FROM` the stack digest, `USER root` → install → `USER sprooty` |
| Lifecycle hooks | the deployment (mounted) | `hooks/pre-start.d/10-example.sh`, mounted read-only at `/run/vogt/hooks`, **never baked into the image** |
| Settings | the deployment (env) | `VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES` in `overlay.yml` |
| The voice pair | the release | `voice.image` pinned to the release matching the stack digest (#616) |

## Image-owned vs. persisted / deployment-owned paths

- **Image-owned** — everything the `Dockerfile` writes (a tool in `/usr/local`
  or `/usr/bin`, files under the image's `$HOME`). A new release replaces these
  wholesale; do not expect edits to them to survive an image change.
- **Persisted** — the named volumes the base declares: `engine-home`
  (`/home/sprooty`, the writable pod home and the `Working` tree) and
  `vogt-data` (`/var/lib/vogt`, the core's SQLite stores and backups). These
  outlive the container and carry your data across upgrades.
- **The shadowing trap**: a named volume mounted over a path the image
  populated **hides** the image's copy. `engine-home` covers `/home/sprooty`,
  so a file your `Dockerfile` writes *inside* `$HOME` is invisible once the
  volume mounts — the volume's contents win. Put tools in `/usr/local`
  (outside any volume), and let the deployment own what lives under `$HOME`.

## Build and run

```bash
cd deploy/examples/custom-stack
docker build -t my-vogt-stack:local .           # pin --build-arg STACK_IMAGE=…@sha256:… for a real build
openssl rand -hex 32 > vogt-core-token
ENGINE_TOKEN=$(openssl rand -hex 24) COMPOSE_PROFILES=voice \
  docker compose --project-directory . \
    -f ../../stack.compose.yml -f overlay.yml up -d --wait
```

`--project-directory .` resolves every relative path in both files here, so the
secret and the hook mount sit beside this overlay.

## Upgrades, migrations, and rollback

- **Upgrade**: change the `STACK_IMAGE` digest in the `Dockerfile` (and the
  matching `voice.image` digest in `overlay.yml`), rebuild, and redeploy. The
  core runs its forward-only migrations against the persisted `vogt-data`
  volume on start; readiness (`/readyz`) reports the core healthy once they
  complete.
- **Rollback is not symmetric.** Migrations are forward-only: once a newer
  image has migrated `vogt-data`, an older image may refuse that schema. Roll
  back only to a release whose schema the data still matches, or restore the
  `vogt-data` volume from a backup taken before the upgrade. See
  [`docs/DEPLOYMENT.md` §7.5](../../../docs/DEPLOYMENT.md).

This example is built and booted in CI with no estate integrations, to keep the
public contract honest.
