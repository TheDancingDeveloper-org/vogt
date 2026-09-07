# Vogt

Vogt is a self-hosted product development environment for teams and the
agents that work with them. It keeps your projects, work items, backlogs and
what your repositories actually contain in one place, and it gives the coding
agents you already use a terminal, a task queue and a voice inside that same
place — so a person and an agent are looking at the same work, with the same
history, and every answer says where it came from and how fresh it is.

It is for small teams and solo developers who run their own tools, want their
agents to work *through* a product rather than around it, and would rather
read "verified four minutes ago from GitHub" than trust a dashboard.

## Live demo

Two public demo sites run the current build against seeded, read-only data.
Nothing there persists and no sign-in is needed:

- [**vogt-demo.thedancingdeveloper.com**](https://vogt-demo.thedancingdeveloper.com/)
  — the desktop app.
- [**vogt-mobile-demo.thedancingdeveloper.com**](https://vogt-mobile-demo.thedancingdeveloper.com/)
  — the same app in a phone frame, as the Android shell wraps it.

## Run it

One published image pair, no build. Copy the settings file, mint two
secrets, start it:

```console
git clone https://github.com/TheDancingDeveloper-org/vogt.git
cd vogt
cp deploy/stack.env.example deploy/.env      # set ENGINE_TOKEN
openssl rand -hex 32 > deploy/vogt-core-token
docker compose -f deploy/stack.compose.yml up -d --wait
```

Open `http://localhost:8910/` and paste the `ENGINE_TOKEN` you chose into
**Settings (⚙)**. Voice is on out of the box — the bundled sidecar transcribes
the microphone and speaks replies with no account. From there,
[Getting started](docs/GETTING_STARTED.md) covers the first project, tokens for
agents, backup and upgrade, and [Deployment](docs/DEPLOYMENT.md) covers running
it somewhere real: digest pinning, a reverse proxy, TLS, data.

## What you get

- **Projects and work.** Register a folder or a repository, and Vogt keeps its
  work items, a ranked backlog, boards, an inbox and an audit trail of every
  change — who did it, and why.
- **What is actually there.** Collectors read your repositories — branches,
  TODO markers, dependencies, sessions — and GitHub issues and pull requests
  when you give it a token. Found work is visible by default; declaring it
  raises its trust, never gates it.
- **Declared and observed, kept apart.** Where the two disagree, Vogt shows the
  drift and lets you decide. It reports; it never enforces.
- **Terminals for the work.** Every work item can open a session — a real
  shell, in a real checkout, with the `claude` and `codex` CLIs installed —
  from the browser or the phone.
- **One API, three ways in.** The CLI, the REST API and the MCP server are
  thin adapters over one operation registry, and the web app is a client of
  that same API.

## Agents

An agent connects to Vogt the way a person does: with a scoped token, through
MCP or the REST API. It can pick up a work item, open a session for it, run a
scheduled task, ask for a push notification when it needs a human, and link
the branch and pull request back to the item — every write attributed and
audited. [The agent guide](docs/AGENT_GUIDE.md) is written for the agent, and
includes a drop-in block for your own repository.

## Make it yours

The published image is never edited; your deployment states only its
difference from it. Settings go in `deploy/.env`, extra services and mounts go
in a Compose overlay of your own, and extra tools go in an image of your own
that starts `FROM` the published digest.
[Customisation](docs/CUSTOMISATION.md) names every supported extension point.

## Know what you are running

The stack image is a **development pod**, not a hardened service: a writable
home, passwordless `sudo`, an SSH server and the agent CLIs, because an agent
session needs a machine and this is the machine. It publishes on loopback until
you say otherwise. Put it where you would put a dev box, put something that
terminates TLS in front of it, and read [SECURITY.md](SECURITY.md) first.

## Documentation

- [Getting started](docs/GETTING_STARTED.md) — install, run, configure, and
  make the first project visible.
- [Deployment](docs/DEPLOYMENT.md) — the image, Compose, environment, reverse
  proxy, backups, upgrades, releases.
- [User guide](docs/USER_GUIDE.md) — daily use of the web app, CLI, REST and
  the agent surfaces.
- [Agent guide](docs/AGENT_GUIDE.md) — for an agent working through Vogt.
- [Configuration reference](docs/CONFIG.md) — every setting, generated from
  the schema.
- [Customisation](docs/CUSTOMISATION.md) — the supported extension points.
- [Engine](docs/ENGINE.md) — the session engine and web app: what it owns, its
  wire contract, the assistant, agent tasks.
- [Architecture](docs/ARCHITECTURE.md) — how the product is put together.
- [Contributing](docs/CONTRIBUTING.md) — workflow, checks, and the
  two-container developer stack.
- [AI policy](AI_POLICY.md) — how this product is built, and what that means
  for contributions.

## Licence

[AGPL-3.0-only](LICENSE), Copyright (c) 2026 TheDancingDeveloper. You can run,
change and redistribute Vogt freely; if you offer a modified Vogt to others as
a network service, the AGPL requires you to offer them its source too.

The terminal's bundled symbol-glyph fallback fonts under `web/public/fonts/`
are subsets of Noto Sans Symbols, Noto Sans Symbols 2 and Noto Sans Math,
licensed under the SIL Open Font License 1.1
(`web/public/fonts/OFL-NotoSans.txt`).
