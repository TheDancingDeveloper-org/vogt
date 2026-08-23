# Vogt Production Cutover

Standing up `vogt-prod` as a genuinely separate production instance: a new
`prod` release branch, its own image stream, its own workspace, its own tailnet
identity, and a fresh import of all 52 `TheDancingDeveloper-org` repositories.

- **Drafted** 21 August 2026 · **Facts verified** live, same day
- **Data migration** none — re-import, by decision
- **Also published as** an Artifact:
  <https://claude.ai/code/artifact/10b28a90-50c4-472f-91d7-0ddb2f8072cb>

> This document is operator-local and deliberately git-ignored (`docs/local/`).
> It records one estate's cutover, with its hostnames, tailnet addresses and
> host paths. `docs/DEPLOYMENT.md` is the reference customisation that belongs
> in the public repository; this is the working plan behind one deploy of it.

Decisions taken before drafting:

| | |
|---|---|
| Branch model | `dev` → `main` → `prod` |
| Hostname | `vogt.sprooty.com` |
| Reachability | tailnet only, grey cloud |
| Komodo stack | new `vogt-prod` |
| Workspace | its own volume |
| Import path | the CLI inside the pod |

---

## What is true right now

Everything below was read from the live estate on 21 August 2026 — Komodo's
API, Node B's shell, the Cloudflare API, Infisical, and the tailnet — not from
a document. Two written-down facts turned out to be wrong, and both are noted.

### Komodo stacks on Node B

| Stack | ops directory | Published on | State |
|---|---|---|---|
| `vogt-dev` | `personal/vogt-dev` | `100.92.54.45:18097` | healthy |
| `vogt` | `personal/vogt-prod-candidate` | `127.0.0.1:18098` | up, not ready |
| `vogt-legacy-core` | `personal/vogt` | — | down |

The stack named `vogt` is **not** production — it is the August 18 prod
*candidate*, bound to loopback, running a stale image, and reporting
`readyz: false` (tailscale offline, core at schema 3 of 9). Nothing depends on
it. It is retired in Phase 8.

### Addresses, networks and credentials

| Fact | Value | Bearing on this plan |
|---|---|---|
| Tailnet nodes | `vogt-dev` `100.101.101.57`; `vogt-prod-candidate` `100.103.125.82` | A new `vogt-prod` node joins in Phase 4; the candidate node is removed in Phase 8. |
| Docker subnets | `vogt-dev_default` `10.59.0.0/16`; `vogt_default` `10.54.0.0/16` | The overlay defaults to `10.59.0.0/16`. Prod **must** override it — see the trap in Phase 4. |
| Free port | `18099` | 18080–18098 are all taken. Cadastre holds no port inventory for Node B, so this is from `ss -ltn` on the host. |
| DNS | `*.sprooty.com` → `192.168.1.75` | No `vogt*` record exists. `vogt-dev.sprooty.com` rides the wildcard to Node B's **LAN** address. |
| Caddy CF token | active — sees the sprooty.com zone | This is the token that creates the prod A record and issues its certificate. |
| Tunnel CF token | **401** | Still dead, as `AGENTS.md` warns. Irrelevant here — the chosen design needs no tunnel. |
| Disk on `/mnt/2tnvme` | 445 GB free of 1.8 TB | Enough for a second full estate clone, with room to watch. |

### Two documented facts are stale

**`AGENTS.md` says `oktatf.sprooty.com` is stuck LAN-only** because no proxied
record could be created. The zone now holds it as a proxied A record to
`103.85.38.176`, so somebody fixed it out-of-band. The tunnel token is
genuinely still 401, though — re-verified against
`GET /client/v4/user/tokens/verify`.

**`AGENTS.md` says `HOMELAB_VOGT_GITHUB_TOKEN` is a copy of
`cicd/GITHUB_DANCINGDEVELOPER_PAT`.** It is not, any more, and that is
fortunate: all three `cicd` GitHub PATs now return **401**, while
`apps/HOMELAB_VOGT_GITHUB_TOKEN` authenticates fine as `thedancingdeveloper`.
See *Blockers*.

---

## Phase 1 — Give releases their own branch

`main` is 1 commit ahead of `dev` and 193 behind. The commit is `1026d2f`,
which stopped main's builds cancelling each other, added registry caching, and
replaced location-based runner labels with capability ones. A literal
`--force` deletes it.

```bash
# 1. bring main's CI fix into dev
git checkout dev
git merge origin/main

# 2. resolve three workflow conflicts (see below), then
git push origin dev

# 3. main becomes dev, by fast-forward — no --force anywhere
git checkout main
git merge --ff-only dev
git push origin main

# 4. cut the release branch
git checkout -b prod main
git push -u origin prod
```

### Expect conflicts in step 2 — the merge was dry-run

Three files conflict: `.github/workflows/build.yml`, `ci.yml` and
`release.yml`. Nothing else in the tree does.

They collide because both sides changed CI caching. `main` has `1026d2f`;
`dev` has `#184`, which was landed, reverted for never building green on a real
runner, then re-landed with a builder-side registry fix. **Dev's version is the
later and better-tested one** — take `dev` for the cache configuration, and
take `main` only for the SHA-keyed concurrency group and the capability-only
`runs-on` labels, which dev lacks entirely.

Afterwards, set `prod` as a protected branch on GitHub. It should only ever
move by fast-forward from `main`.

**Verify** — `git log --oneline -1 prod` matches `main`, and
`git branch --contains 1026d2f` lists all three branches.

---

## Phase 2 — Teach the build about the prod stream

Two branches build today and their tags cannot collide by construction. Adding
a third means saying explicitly which stream it belongs to — otherwise `prod`
silently starts producing `main`'s tags.

| Branch | Tags | Who pins it |
|---|---|---|
| `dev` | `dev`, `dev-<sha>` | `vogt-dev` |
| `main` | `sha-<commit>` | nobody — integration only |
| `prod` | `prod-<sha>` | `vogt-prod` |

In `.github/workflows/build.yml`, add `prod` to `on.push.branches`, then fix
the tag rules in **both** image jobs — the core `vogt` image and the merged
`vogt-stack` image:

```yaml
tags: |
  type=sha,enable=${{ github.ref == 'refs/heads/main' }}
  type=sha,prefix=prod-,enable=${{ github.ref == 'refs/heads/prod' }}
  type=sha,prefix=dev-,enable=${{ github.ref == 'refs/heads/dev' }}
  type=raw,value=dev,enable=${{ github.ref == 'refs/heads/dev' }}
```

**Why the first line has to change too.** It currently reads
`enable=${{ github.ref != 'refs/heads/dev' }}` — "anything that is not dev".
The moment `prod` starts building, that rule hands it a plain `sha-` tag,
putting prod images into main's stream and defeating the whole point of the
split. Flip it to an explicit equality.

Update the comment block at the top of `build.yml` too — it documents the
two-stream model in prose, and a third stream that only exists in the YAML is
how the next person gets it wrong.

**Verify** — push `prod` and confirm GHCR holds `vogt-stack:prod-<sha>` and
`vogt:prod-<sha>`, and that no `sha-` tag appeared for the same commit. Record
both digests; Phase 4 pins them.

---

## Phase 3 — Add the ops directory

Komodo deploys from `indexarr/ops`, and the product repo holds the reviewed
source of those files. `vogt-prod` uses the same overlay shape as `vogt-dev`,
which is byte-identical to the repo's `deploy/` copies today (confirmed with
`diff`).

```bash
cd ~/Working/Active/apps/ops
mkdir -p personal/vogt-prod
cp ~/Working/Active/apps/vogt/deploy/vogt.compose.yml    personal/vogt-prod/
cp ~/Working/Active/apps/vogt/deploy/estate.overlay.yml  personal/vogt-prod/
cp personal/vogt-dev/estate.docker-socket.yml            personal/vogt-prod/
git add personal/vogt-prod && git commit -m "personal/vogt-prod: the production stack"
git push
```

### Drift worth closing while you are here

`estate.docker-socket.yml` exists **only** in `indexarr/ops`. There is no copy
in the product repo's `deploy/`, which breaks the rule the compose header
states — that the reviewed source lives with the code. Land it in `deploy/` on
`dev` as a small follow-up.

Also decide deliberately whether prod gets the Docker socket at all. It is a
trust decision, and prod is the one instance where "an agent session can reach
the host daemon" deserves a second look. Dropping it means dropping the file
from `file_paths` in the next phase.

---

## Phase 4 — Create the vogt-prod stack

A new Komodo stack on server `69e429838259c3a0a7eccceb`, mirroring `vogt-dev`'s
configuration with every instance-scoped value changed. The digests come from
Phase 2.

```
repo            indexarr/ops   (branch main)
run_directory   personal/vogt-prod
file_paths      vogt.compose.yml, estate.overlay.yml, estate.docker-socket.yml
auto_update     false   poll_for_updates  false   webhook_enabled  true
```

### Environment

| Variable | Value | Why not the default |
|---|---|---|
| `VOGT_PORT` | `18099` | First free port above dev's 18097 and the candidate's 18098. |
| `VOGT_BIND_IP` | `100.92.54.45` | Node B's tailnet address. Never the LAN address, never `0.0.0.0`. |
| `VOGT_PUBLIC_URL` | `https://vogt.sprooty.com` | Nothing in the container can work this out; the overlay refuses to start without it. |
| `TAILSCALE_HOSTNAME` | `vogt-prod` | Defaults to `vogt`, which would be an unhelpful name beside `vogt-dev`. |
| `VOGT_CONTAINER_NAME` | `vogt-prod` | Docker refuses a duplicate name rather than replacing — this is where the first vogt-dev deploy failed. |
| `VOGT_STACK_SUBNET` | `10.60.0.0/16` | **The trap below.** Unset means `10.59.0.0/16`, which vogt-dev already holds. |
| `VOGT_CORE_IP` | `10.60.0.10` | Must sit inside the subnet above. |
| `VOGT_CORE_VOLUME` | `vogt-prod-core-data` | Its own store. No migration from dev, by decision. |
| `VOGT_WORKSPACE_DIR` | `/mnt/2tnvme/docker/volumes/vogt-prod/workspace` | Its own estate tree — the isolation decision. Dev keeps `mydevenv2/workspace`. |
| `VOGT_HOME_DIR` | `/mnt/2tnvme/docker/volumes/vogt-prod/home` | Holds the engine's `state_dir`: VAPID keys, push subscriptions, session history. |
| `VOGT_TAILSCALE_DIR` | `/mnt/2tnvme/docker/volumes/vogt-prod/tailscale` | So the pod does not re-authenticate to the tailnet on every restart. |
| `VOGT_IMPORT_ROOT` | `/home/sprooty/Working/Active` | Same as dev. Imports land at `<root>/<slug>`. |
| `VOGT_STACK_IMAGE` | `…/vogt-stack@sha256:<prod>` | Digest-pinned, from Phase 2. Never a moving tag. |
| `VOGT_IMAGE` | `…/vogt@sha256:<prod>` | The core half, same commit. |

> **The subnet collision will bite silently.** `estate.overlay.yml` defaults
> `VOGT_STACK_SUBNET` to `10.59.0.0/16` and `vogt-dev` does not override it —
> its Komodo environment sets no such variable, and `vogt-dev_default` holds
> exactly that range. A second stack asking for the same subnet fails at
> network creation, and Compose's message points at the network rather than at
> the variable. Set it, and set `VOGT_CORE_IP` to match.

### Credentials

Mint **new** values for `MYDEVENV2_TOKEN` (the front door's primary bearer
token) and `VOGT_ENGINE_TOKEN` rather than reusing dev's — a shared token makes
the two instances indistinguishable in an audit row. Reuse the estate-wide
ones: `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` and `_SECRET`,
`HOMELAB_MYDEVENV2_TAILSCALE_AUTH_KEY`, and
`MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON`. Set `VOGT_GITHUB_TOKEN` from
`apps/prod/HOMELAB_VOGT_GITHUB_TOKEN`, which is verified working. Leave
`VOGT_CORE_TOKEN` empty for now — it is chicken-and-egg and Phase 6 resolves
it.

### The pre_deploy hook

Copy dev's verbatim; it extracts the three credentials into files so they never
appear in `docker inspect`:

```sh
umask 077
sed -n 's/^VOGT_GITHUB_TOKEN=//p' .env > github-token
sed -n 's/^VOGT_CORE_TOKEN=//p'   .env > vogt-core-token
sed -n 's/^VOGT_ENGINE_TOKEN=//p' .env > vogt-engine-token
test -s github-token
chown 1000:1000 github-token vogt-core-token vogt-engine-token
chmod 640       github-token vogt-core-token vogt-engine-token
```

### Host directories

```bash
tailscale ssh sprooty@winrarhost
sudo mkdir -p /mnt/2tnvme/docker/volumes/vogt-prod/{workspace,home,tailscale}
sudo chown -R 1000:1000 /mnt/2tnvme/docker/volumes/vogt-prod
```

Then `DeployStack`. The first boot comes up without a core token: sessions,
terminals and `/mcp` all work, and `/api/vogt` answers 401. That is FR-E9
behaving correctly, not an outage.

**Verify** — `curl http://100.92.54.45:18099/readyz` shows `workspace_root`,
`state_dir` and `tailscale` green, and `vogt_core` reporting its schema.
`tailscale status | grep vogt-prod` returns a new address — write it down,
Phase 5 needs it.

---

## Phase 5 — The front door: DNS and Caddy

A Cloudflare-managed name pointing at Node B's tailnet address, terminated by
the same Caddy that already fronts `vogt-dev`. Reachable from the tailnet and
nowhere else.

> **The orange cloud has to stay off.** `100.92.54.45` is CGNAT space
> (`100.64/10`). Cloudflare's edge cannot route to it, so a proxied record
> would answer 522 every time. Pointing at the tailnet address and proxying are
> mutually exclusive — you get Cloudflare as the registrar and DNS authority,
> not as a proxy.
>
> This is also stricter than `vogt-dev`, which rides the wildcard to Node B's
> **LAN** address and so answers on the LAN without Tailscale. Prod will not.
> Worth knowing before the first "it's down" from a LAN-only client.

### The record

Create with `HOMELAB_CADDY_CLOUDFLARE_API_TOKEN` against zone
`50c139a1be63065a01ba205cd9ae2e7c`:

```http
POST /client/v4/zones/<zone>/dns_records
{ "type": "A", "name": "vogt", "content": "100.92.54.45",
  "proxied": false, "ttl": 1,
  "comment": "vogt-prod on Node B — tailnet only" }
```

This token authenticates and can read the zone. It is Caddy's DNS-01
credential, so it must also hold `DNS:Edit` — but the create call is the first
proof of that, and if it returns 403 the fallback is the Cloudflare dashboard.

### The Caddy vhost

Back the Caddyfile up first, per the estate's own convention:

```bash
sudo cp /mnt/2tnvme/docker/volumes/caddyv2/conf/Caddyfile \
        /mnt/2tnvme/docker/volumes/caddyv2/conf/Caddyfile.bak-20260821-before-vogt-prod
```

```caddy
vogt.sprooty.com {
	tls {
		dns cloudflare {env.CLOUDFLARE_API_TOKEN}
	}
	import access_log
	import check_banned
	import secure_route
	# The tailnet address, not the MagicDNS name: Caddy resolves inside
	# its own container, which has no MagicDNS resolver.
	reverse_proxy http://<vogt-prod tailnet IP>:8910
}
```

Then `docker exec caddy caddy validate --config /etc/caddy/Caddyfile` and
reload. `secure_route` lets LAN, tailnet and Docker ranges through
unauthenticated and puts Basic Auth on everything else — which, given the
record above, is a belt-and-braces layer nothing should ever reach.

**Verify** — from a tailnet node, `curl https://vogt.sprooty.com/version`
returns the prod image's version. From a LAN-only host, expect the name to
resolve and the connection to fail; that is the design working.

---

## Phase 6 — Pair the core token

Resolving the chicken-and-egg from Phase 4. The core token is what the front
door injects on `/api/vogt`, so Vogt's audit rows name the actor who acted
rather than "the proxy" (FR-S9).

```bash
tailscale ssh sprooty@winrarhost
docker exec vogt-prod-vogt-1 vogt token issue \
    --actor agent:vogt-prod --scope read --scope work.write --scope project.write
```

Store the value in Infisical `apps/prod` as `HOMELAB_VOGT_PROD_CORE_TOKEN`, set
it as `VOGT_CORE_TOKEN` in the Komodo environment, and `DeployStack` again.

Issue the narrowest scope the front door actually needs — everything in the pod
runs as uid 1000 and can read the file, so the token's scope *is* the pod's
blast radius.

> **This step is human-only.** `AGENTS.md` marks issuing and listing Vogt
> tokens as a boundary agents do not cross. Everything either side of it can be
> driven for you.

**Verify** — `/readyz` stays green and `/api/vogt/status` stops answering 401.

---

## Phase 7 — Import all 52 repositories

A fresh estate, cloned into prod's own workspace. Run from the CLI inside the
pod, which writes the data directory directly and needs no HTTP token at all.

The org holds 52 repositories. The live instance today has 50 registered —
**`cadastre` and `rdpapp` are missing** — and this import closes that gap. All
13 `AusAgentSmith-org` repositories are out of scope.

```bash
docker exec vogt-prod vogt status   # record instance_id and data_dir first

# then, per repo:
docker exec vogt-prod vogt project import \
    --repo-url https://github.com/TheDancingDeveloper-org/<name>
```

13 of the 52 are private, so the clone depends on the GitHub credential the pod
holds — `apps/prod/HOMELAB_VOGT_GITHUB_TOKEN`. Confirm one private repo
(`tfdrift` or `secscan`) before running the batch, so a credential failure
surfaces on repo one rather than repo forty.

`IMPORT-PLAYBOOK.md` marks several DECISION points — chiefly which remote is
authoritative where a repo also has a Forgejo mirror. Those need answering
rather than guessing; a wrong `repo_url` is what every forge collector keys
off, and later sweeps do not correct it.

**Verify** — `project_list` totals 52, every `root_path` sits under prod's own
workspace, and no path resolves into `mydevenv2/workspace`.

---

## Phase 8 — Retire the candidate

Only once prod is verified. Three things carry the old name and each has to be
cleaned up in its own system.

1. **Komodo** — destroy the stack named `vogt` (the prod candidate) and the
   stack named `vogt-legacy-core`, which is already down.
2. **Tailnet** — remove the `vogt-prod-candidate` node at `100.103.125.82`, so
   a stale name cannot be resolved by something that outlived it.
3. **ops** — delete `personal/vogt-prod-candidate` and `personal/vogt`; both
   are now dead desired-state.

Keep the candidate's volumes until prod has run for a few days. They cost disk
and nothing else, and they are the only copy of whatever that instance
accumulated.

Finally, tell the registers what changed: `cadastre check` the new compose
before it lands, and update the declared service facts so the map matches the
estate. `docs/DEPLOYMENT.md` §9 needs the third deployment added — it currently
describes three deployments, and none of them is this one.

---

## Blockers and standing risks

### Three CI GitHub PATs are dead

`cicd/GITHUB_DANCINGDEVELOPER_PAT`, `cicd/GITHUB_PAT` and
`cicd/GITHUB_AUSAGENTSMITH_PAT` all return **401**. This is also why cadastre's
`forge-github` collector has been failing — its error text blames scope, but
the credential is simply invalid.

It does **not** block this deploy, because `apps/HOMELAB_VOGT_GITHUB_TOKEN`
works and is what both the stack and the import use. It does block anything in
CI that reaches GitHub with those names, and it is worth rotating in the same
session.

### Deploying kills live sessions

A `DeployStack` against a vogt stack tears down every PTY in that pod. Prod has
none yet, so Phases 4 and 6 are free — but the moment somebody is working in
it, a redeploy is strictly the last action of a session, never the first.

### Two estates now exist

Giving prod its own workspace is the right isolation call, and it has a
consequence: local work in dev's tree is invisible to prod and vice versa.
Neither instance is authoritative over the other's working copies, and the same
repository will have two independent clones with independent branches. Be
explicit about which one you open a session in.

### Rollback

Every phase is reversible and nothing is destroyed until Phase 8. Git — `prod`
is a fresh branch and `main` only fast-forwarded. Komodo — restore the previous
digests in the stack environment and `DeployStack`. DNS — delete the record.
Caddy — restore the timestamped backup and reload. The candidate stack stays
untouched and re-deployable until you explicitly retire it.

---

*Verified live 21 August 2026 against Komodo, Node B, Cloudflare, Infisical and
the tailnet. Counts and digests are point-in-time — re-read `vogt status` and
`cadastre brief` before acting on any number here.*
