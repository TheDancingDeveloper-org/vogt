# The merged stack in Komodo

> **Scope / drift warning (2026-08-20).** This file documents the **merged,
> single-container** shape (`vogt-stack.compose.yml` +
> `vogt-stack.docker-socket.yml`). The live `vogt-dev` stack no longer runs
> that — it runs the **split overlay**: `vogt.compose.yml` +
> `estate.overlay.yml` + `estate.docker-socket.yml`, as two containers (engine
> front door + core). **`GetStack` on the Komodo API is the source of truth for
> what is actually deployed**, and the pinned digest lives in the `indexarr/ops`
> copy — not this file (§1 explains why the compose exists in two repos and
> drifts). Read this file for the field-by-field *shape* and the ordering
> logic; do not trust its `file_paths` against the running stack. §5 ("engine
> session token") already describes the overlay reality.

What to create, in what order, and what each field is for. The compose file
beside this one is the desired state; this is how Komodo is told about it.
`DEPLOYMENT.md` §9 is the deploy runbook and takes precedence on sequence;
this file is the stack's shape.

Written for the **dev** stack, which is the one NFR-D12 asks for and the one
that makes mobile, voice and push verifiable at all. A prod stack is the same
shape with a `sha-<commit>` digest, its own port and its own volumes.

## 1. The ops repo

Desired state lives in `indexarr/ops`, as it does for every stack on Node B.
Komodo reads the repo; it does not read this one.

```bash
cd ~/Working/Active/apps/ops
mkdir -p personal/vogt-dev
cp ~/Working/Active/apps/vogt/deploy/vogt-stack.compose.yml            personal/vogt-dev/
cp ~/Working/Active/apps/vogt/deploy/vogt-stack.docker-socket.yml      personal/vogt-dev/
git add personal/vogt-dev && git commit -m "add vogt-dev stack" && git push
```

Copied rather than referenced, and that is a real cost worth naming: the
compose file now exists in two repositories and they will drift. The pinned
digest is the line that matters, and it lives in the ops copy — `vogt`'s copy
records which digest was current when it was written, not what is deployed.
Read the ops copy when you want to know what is running.

## 2. The stack

| Field | Value |
|---|---|
| Name | `vogt-dev` |
| Server | Node B |
| Repo | `indexarr/ops` |
| Run directory | `personal/vogt-dev` |
| File paths | merged shape: `vogt-stack.compose.yml`, `vogt-stack.docker-socket.yml` — **but live `vogt-dev` runs the overlay** `vogt.compose.yml`, `estate.overlay.yml`, `estate.docker-socket.yml` (confirm with `GetStack`) |
| Environment | the filled `vogt-stack.env.example` |
| Webhook | `vogt` **off**; `vogt-dev` **on** — see below |

**The socket overlay is a trust decision, not a default.** Listing
`vogt-stack.docker-socket.yml` mounts the host's Docker socket into the pod,
so an agent in a session controls the host daemon. That is the mode the
personal homelab already runs MyDevEnv2 in, and the merged stack inherits the
reason: sessions run agent CLIs and repo workflows that expect `docker` to
work. Omit the overlay and Docker commands inside sessions fail closed, which
is the lower-privilege mode and costs nothing else.

**Webhook off is deliberate — for the stack this document was written about.**
Publishing an image is not deploying it (NFR-D10): production moves when a
human changes the pinned digest and runs `DeployStack`. A webhook that
redeployed on every push would make that a continuous-deployment branch,
which is a different requirement from the one the stack exists to satisfy.

**`vogt-dev` does not follow that rule, and the difference is easy to trip
over.** Read from the Komodo API on 2026-08-20:

| Komodo stack | Run directory | Webhook |
|---|---|---|
| `vogt-dev` | `personal/vogt-dev` | **on** |
| `vogt` | `personal/vogt-prod-candidate` | off |
| `vogt-legacy-core` | `personal/vogt` | on (stack is down) |

So a push to `indexarr/ops` `main` **redeploys `vogt-dev` by itself**, while
the `vogt` stack waits for an explicit `DeployStack`. Two consequences worth
holding: batch ops changes into one push if you want one redeploy, and do not
push a half-finished `vogt-dev` compose expecting to deploy it later.

Note also that the Komodo stack *names* do not match their run directories —
the stack called `vogt` deploys the prod-candidate directory. Check
`GetStack` before assuming which one you are moving.

## 3. The `pre_deploy` hook

Three credentials, all brokered as files (FR-S7). Komodo writes its environment
field to `.env` beside the compose at deploy time; the hook turns those values
into files the container mounts:

```bash
umask 077
sed -n 's/^VOGT_GITHUB_TOKEN=//p' .env > github-token
sed -n 's/^VOGT_CORE_TOKEN=//p'   .env > vogt-core-token
sed -n 's/^VOGT_ENGINE_TOKEN=//p' .env > vogt-engine-token
test -s github-token
chown 1000:1000 github-token vogt-core-token vogt-engine-token
chmod 640       github-token vogt-core-token vogt-engine-token
```

`1000:1000`, not the core-only stack's `1000:0` — this image's `sprooty` has
gid 1000, where the core-only image runs any uid with gid 0 precisely because
it has not decided who it is.

Note `test -s github-token` and **no** such test on the core *or* engine
token: the core token is legitimately empty on a first deploy, and failing the
hook over it would make the stack unbootable in exactly the state you need it
to boot in to mint one. The engine token is the same — the core-driven session
path is simply off until it exists, which is the FR-E9 absence, not a fault.

## 4. First deploy, in order

The order is forced by one chicken-and-egg: the core token is minted *by* the
core, which has to be running first.

1. Fill the environment with `VOGT_CORE_TOKEN` empty. Deploy.
2. `scripts/smoke_merged_stack.sh https://<public-url>` — with no token it
   still checks that the engine answers, that vogt-core is ready behind it,
   that the import root and workspace root agree, that a backup here would
   cover both halves, and that `/api/config` advertises Vogt. Five of its six
   checks need no credential.
3. Mint the pairing:
   ```bash
   docker exec vogt-dev vogt token issue --actor <you> --name front-door \
       --scopes read,work.write --reason "front door pairing"
   ```
4. Put it in `VOGT_CORE_TOKEN`, redeploy, and run the smoke script again with
   a front-door token — the sixth check reports which actor the proxy reached
   the core as, which is FR-S9 working or not.

Between 1 and 4, `/api/vogt` reads work and writes refuse with a named reason.
That is the designed degradation, not a broken stack.

## 5. The engine session token (#157)

Distinct from the core token above, and the other direction: this is the token
the **core** presents to the **engine** so a work item can open its own coding
session (FR-E1). Until it exists, `vogt session start` for a work item reports
`engine_unavailable`/`no engine is configured` — which is the FR-E9 absence
seen from the core, and was the state of *every* stack before this was wired.

It is a scoped front-door token, not a core token, so it is *not* minted with
`vogt token issue`. It is a value you choose, declared to the engine and
brokered to the core as the same value:

1. Generate an opaque value:
   ```bash
   openssl rand -hex 32
   ```
2. Declare it to the engine, with the `sessions` capability and nothing else,
   in the stack's environment:
   ```
   MYDEVENV2_EXTRA_TOKENS_JSON=[{"name":"vogt-core-sessions","token":"<value>","capabilities":["sessions"]}]
   VOGT_ENGINE_TOKEN=<value>
   ```
   The `pre_deploy` hook brokers `VOGT_ENGINE_TOKEN` into `vogt-engine-token`;
   the engine learns the same value from `MYDEVENV2_EXTRA_TOKENS_JSON`.
3. Redeploy. `vogt session start --project <p> --name x --reason "…"` now opens
   a terminal instead of refusing.

The `sessions` capability is deliberately the whole grant: Vogt starts and
stops terminals and has no business writing that pod's files (CONFIG.md,
`engine_token_file`). A token with more capability than that is a token the
core did not need and a blast radius it should not carry.

## 6. What to watch after it is up

`/readyz` carries seven checks and three of them are non-fatal by design, so
the top-level `ok` can be true while something worth knowing is false:

- `vogt_core` — the core is behind the front door at all. A stack can be
  "ready" with no core (FR-E9), which is why the smoke script exists.
- `workspace_agreement` — imported projects are visible to sessions (FR-E3).
- `backup_agreement` — `vogt backup` here would cover the engine's state as
  well as the core's (NFR-I6). False means backups succeed and contain no
  session history, push subscriptions or VAPID keypair.
