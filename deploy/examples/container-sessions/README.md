# Bring your own terminal image (per-session containers)

Vogt runs every terminal session as a process **inside the one pod**, so all
sessions share the pod's toolchains. If you want a session to run in a *different*
image — your own, with whatever languages you need, and one or more available at
once — you do not need a vogt feature for it: a session template's command is
just argv run in a PTY, and the pod has a Docker CLI. So a template whose command
is `docker run -it <your image>` opens a terminal **inside that image**, and the
session picker becomes an image picker.

This is a power-user recipe, not a managed feature — you own the image, the mount,
and the lifecycle. Read the caveats.

## Files here

- `engine.container-sessions.toml` — example engine config adding the session
  templates (a plain in-pod `Shell`, plus `Go dev (container)` and
  `Node dev (container)`). **Providing `session_templates` replaces the built-in
  templates** — re-add any you still use.
- `docker-socket.overlay.yml` — a Compose overlay that mounts the host Docker
  socket into the pod and points `ENGINE_CONFIG` at the file above.

## Run it

```bash
cd deploy/examples/container-sessions
openssl rand -hex 32 > vogt-core-token
cat > .env <<'ENV'
ENGINE_TOKEN=<openssl rand -hex 24>
COMPOSE_PROJECT_NAME=vogt
DOCKER_GID=<getent group docker | cut -d: -f3>
ENV
docker compose --project-directory . \
  -f ../../stack.compose.yml -f docker-socket.overlay.yml up -d --wait
```

Open the PWA, start a session with the **Go dev (container)** template, and you
are in your Go image with the repository tree already there. Edit the `.toml` to
point at your own images; add one `[[session_templates]]` block per image.

A user who only wants it for themselves can skip the config file entirely and
create the same `docker run …` command as a **Workspace Preset** in the PWA
(stored in that browser) — the socket overlay is still required.

## How the workspace is shared

The overlay puts the `Working` tree on a **named volume** (`vogt-work`, mounted
at `/home/sprooty/Working`), and the templates mount that same volume
(`-v vogt_vogt-work:/home/sprooty/Working`) into the session container — so the
container and the pod see the identical repository tree, and the engine's
file/git APIs and the session stay in agreement. The `vogt_` prefix is
`COMPOSE_PROJECT_NAME`; that is why `.env` sets `COMPOSE_PROJECT_NAME=vogt`.
Confirm the real name with `docker volume ls`.

Why a named `vogt-work` volume rather than the pod's `engine-home`? The stack
image declares an anonymous `VOLUME /home/sprooty/Working`, so by default the
work tree lands on a random anonymous volume that no session container can mount
by name (and that `docker compose down` orphans). Mounting a named volume at that
exact path overrides the anonymous one, making the tree stable, persisted, and
shareable. (The underlying `VOLUME` is being removed upstream; this overlay works
either way.)

## Caveats (you own these)

1. **The Docker socket is root-equivalent on the host.** Mounting it lets anyone
   who can open a terminal in the pod control the host's Docker daemon. Only do
   this where you already trust that person. It is opt-in and never in the base.
2. **Mount the volume, not the in-pod path.** The socket talks to the *host*
   daemon, so a `-v /home/sprooty/Working:…` bind would resolve on the host and
   miss the pod's data. Bind the **named volume** (`vogt_engine-home`) or a real
   host path.
3. **uid alignment.** The shared tree is owned by uid 1000 (the pod's `sprooty`).
   An image whose user is a different uid will hit permission errors — build your
   image to run as uid 1000, or accept the ownership friction.
4. **Lifecycle is yours.** Killing the session kills the `docker run` client;
   `--rm` cleans up on normal exit, but an orphaned container is possible. Vogt
   does not manage these containers for you.

If you would rather vogt managed all of that (an image field, reaped containers,
no raw socket), that is a first-class feature request, not this recipe.
