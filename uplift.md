# MyDevEnv2 Outstanding Uplift Backlog

This is the single canonical backlog for remaining uplift work in MyDevEnv2.
Completed uplift work lives in normal repo history, README status notes, and
the code itself. No other markdown file should carry a parallel open-items
backlog.

---

## Environment Strategy: Dev vs Prod

The currently deployed stack (`prod-mydevenv2`, `mydevenv2.sprooty.com`) stays
prod, unchanged in trigger/behavior. All uplift work below should land on a
new `dev-mydevenv2` stack first and get validated live before promotion to
`main`/prod. This is a prerequisite for the rest of this backlog, not
optional infra polish — several items (mobile reconnect, FCM, split-pane
fix) are only trustworthy once verified against a running instance, and none
of them should be verified against prod.

**Branching:** introduce a long-lived `dev` branch. Uplift PRs land on `dev`
first, auto-deploy to the dev stack, get validated, then merge/fast-forward
to `main` to ship to prod. `main` remains the only branch that deploys to
prod.

**CI (`.woodpecker/server.yml`):**
- fmt/clippy/test/web-typecheck already run on push + PR; no change needed.
- Add a push trigger for `branch: dev` alongside the existing `main` trigger.
- `build-and-push`: on `dev`, tag `:dev` + `:dev-<sha>` instead of `:latest`
  + `:<sha>`, so the two image streams can never collide and a dev build can
  never become `:latest` by accident.
- `komodo-deploy`: branch-conditional. `main` keeps rewriting
  `ops/personal/mydevenv2/docker-compose.yml` (`prod-mydevenv2`, as today);
  `dev` rewrites a new `ops/personal/mydevenv2-dev/docker-compose.yml` and
  triggers `DeployStack` for a new `dev-mydevenv2` stack. Same
  `komodo-deploy.sh`, parameterized by `STACK_NAME`/`STACK_DIR`.
- `mobile-apk`: currently `main`-only, uploads to the Forgejo `apk-latest`
  release tag. See the mobile note below before extending it to `dev` —
  it's not just a branch-condition change.

**Ops repo (`indexarr/ops`):** new `personal/mydevenv2-dev/docker-compose.yml`,
cloned from the prod compose with:
- `container_name: mydevenv2-dev`, `hostname: mydevenv2-dev`, `TAILSCALE_HOSTNAME: mydevenv2-dev`.
- Host port `8911:8910` on Node B (prod holds `8910`).
- Separate state volumes: `/mnt/2tnvme/docker/volumes/mydevenv2-dev/{workspace,home,tailscale}`.
  Recommend the `workspace` bind-mount still point at the same underlying
  `~/Working` data as prod — it's just a mount of the shared workspace, not
  app state, and running two independent MyDevEnv2 servers with PTY sessions
  against the same files is no different from two terminal windows. `home`
  and `tailscale` stay isolated per stack.
- New `MYDEVENV2_TOKEN` for dev, minted separately from prod's.
- Reuse the existing `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_ID` /
  `HOMELAB_MYDEVENV2_INFISICAL_CLIENT_SECRET` viewer-only identity — no
  reason to mint a second one for a read-only credential bridge.
- New Tailscale auth key (`HOMELAB_MYDEVENV2_DEV_TAILSCALE_AUTH_KEY`) so dev
  advertises as its own tailnet node rather than reusing prod's one-shot key.
- Own VAPID/push state falls out naturally from the separate volume — dev
  push subscriptions won't cross-notify prod's.
- New Komodo stack `dev-mydevenv2`, same periphery (Node B) as prod unless
  resource contention later argues otherwise.
- New Caddy site block on Node B: `mydevenv2-dev.sprooty.com -> localhost:8911`.

**Status:** done. `.woodpecker/server.yml` has the `dev` push trigger,
`build-and-push-dev` (`:dev` / `:dev-<sha>` tags), and `komodo-deploy-dev`
(targets `dev-mydevenv2` / `personal/mydevenv2-dev`, ops repo's own `dev`
branch) steps, all gated on `branch: dev` and additive to the existing
`main` steps, which are unchanged. The `dev` branch exists in both
`MyDevEnv2` and `ops`; `personal/mydevenv2-dev/docker-compose.yml` is
committed and pushed; the `dev-mydevenv2` Komodo stack exists
(`git_account` set, fetches `ops` cleanly); `MYDEVENV2_DEV_TOKEN` is minted
and stored in Infisical; the Caddy site block for
`mydevenv2-dev.sprooty.com -> localhost:8911` is live and issuing certs
correctly. See MyDevEnv2's `deploy/KOMODO.md` "Dev stack (dev-mydevenv2)"
for the full picture, including a disk-layout fix folded into this rollout:
dev's `home`/`tailscale`/`tmp` now live on a dedicated disk (`/mnt/sdg`, see
root `AGENTS.md` "Node B Disk Layout") instead of prod's root-disk-backed
`/mnt/2tnvme` — validating a fix for the `/tmp`-never-bind-mounted issue
that contributed to a Node B root-disk-full incident, before prod adopts
the same layout. Workspace intentionally still shares prod's existing
`/mnt/2tnvme` mount — see KOMODO.md for why that's a separate, later step.
Still outstanding: the Tailscale auth key
(`HOMELAB_MYDEVENV2_DEV_TAILSCALE_AUTH_KEY`) — optional, the container runs
without tailnet join if it stays empty — and an actual green CI run all the
way through `komodo-deploy-dev` (blocked, then unblocked, by an unrelated
Node B `sccache-redis` OOM-crash-loop; last known state was `test`/`clippy`
passing cleanly once Node B's memory pressure eased).

**Mobile/Android caveat:** an Android device cannot install two APKs sharing
the same `applicationId` side by side. To validate mobile-facing uplift work
(SSE resume reconnect, push notifications) without uninstalling the prod app
between tests, the dev build needs its own `applicationId` (e.g.
`com.sprooty.mydevenv2.dev`), its own Firebase/FCM client entry in
`google-services.json`, and its own app-level config pointing at
`mydevenv2-dev.sprooty.com`. The signing keystore/alias can be shared. This
is real (small) work, not a CI flag flip — scope it as part of standing up
the dev stack, before relying on side-by-side dev/prod APKs for testing.

---

## Stack / Ops

1. **Bearer-token risk boundary** (`server/src/auth.rs`, `deploy/docker-compose.yml`)
   Repo-side audit logging, per-token mutation rate limits, and scoped token
   capabilities now exist. Production now has provisioned read-only and
   interactive scoped tokens, and the live stack is running with
   `MYDEVENV2_EXTRA_TOKENS_JSON` populated. Remaining work is moving the live
   clients onto those scoped tokens where appropriate and deciding whether the
   primary token should keep full Docker-adjacent access long term.

---

## Mobile / Android

1. **Real-device native FCM verification**
   `google-services.json` already includes the `com.sprooty.mydevenv2` client;
   the remaining work is confirming first-launch FCM registration and end-to-end
   delivery on actual Android hardware.

2. **Disconnected-state recovery is asymmetric** (`web/src/store.ts`, `web/src/Terminal.tsx`) — **done**
   `store.ts` now force-reconnects the SSE stream on DOM `visibilitychange`
   -> `visible` and on Capacitor's `App.addListener('resume', ...)`
   (`@capacitor/app` added to `web/package.json` and `mobile/package.json`,
   registered in the Android Gradle project via `cap sync android`). The
   force-reconnect only fires if `startEventStream()` was already called, so
   it can't spin up a connection before login. Not yet validated on real
   Android hardware — needs the dev stack + dev-APK `applicationId` split
   above.

3. **Voice conversational interaction (new surface)**
   Nothing exists today: no STT/TTS Capacitor plugins, no `RECORD_AUDIO`
   permission, no audio code in `web/src`. TTS (agent -> verbal update) is
   low effort via the Web Speech API (`speechSynthesis`) inside the WebView.
   STT (user -> spoken input) needs a native plugin (e.g.
   `@capacitor-community/speech-recognition`) plus the manifest permission —
   Web Speech `SpeechRecognition` is unreliable/absent on Android WebViews.
   The open design question is the agent-side "when to speak" hook: naturally
   piggybacks on the same activity/phrase-watcher events proposed below
   rather than being built from scratch. Scope as a follow-up after the
   multi-agent signal work lands, not before.

---

## Web GUI

1. **Split-pane layout forces full-tree remount on every split/close** — **done**
   (`web/src/TerminalWorkspace.tsx:175-225`, `:248-289`)
   `insertPane`/`removePane` now preserve object/array identity for
   unaffected subtrees (only rebuild ancestors on the actual edit path), and
   the pane click handler binds to `props.node.id` directly instead of the
   `Match`-resolved accessor. `SplitEditor.tsx`'s drag-resize `mousemove`
   handler is now throttled to one `resizePanePair` call per animation
   frame. Not yet validated live against a running dev instance.

---

## Multi-Agent Management

Context: current session model is close to N independent TTYs, but more
groundwork already exists than a from-scratch design would assume.

1. **Push notification on agent hang/fail/end** — **done**
   `push_api::spawn_activity_watcher` now fires on both `WaitingForInput`
   (as before) and `Errored` transitions. A new `spawn_idle_stall_watcher`
   background task scans live sessions every 30s and sends a one-shot
   `NotificationKind::IdleStall` push when a session has sat continuously
   `Idle` (not exited) longer than `idle_stall_after_ms` (config/env
   `MYDEVENV2_IDLE_STALL_AFTER_MS`, default 10 minutes) — the case where
   output just stops without a recognizable prompt. `PushPreferences` gained
   `errored`/`idle_stall` toggles and digest-count fields. Not yet validated
   live.

2. **Auto-retry on transient failures (e.g. 429 / rate limit)** — **done**
   `activity::is_rate_limited` matches `429` / `rate limit` / `overloaded` /
   `too many requests` anywhere in the tail. A new `spawn_retry_watcher`
   (sibling to `spawn_phrase_watcher`) tails the same session output and
   writes a retry keystroke (`\r`) back into the PTY after an exponential
   backoff (5s, 10s, 20s, 40s, capped), giving up and sending one
   `AgentTaskNotify` push after 5 consecutive attempts. Gated per-task by
   the new `auto_retry_on_rate_limit` field (default on), exposed in the
   Agent Tasks UI. Not yet validated against a real rate-limited run.

3. **Cross-agent orchestration (genuinely new design work)**
   Nothing today coordinates *between* sessions — no shared task queue beyond
   `agent_tasks.rs`'s single-shot scheduler, no guard against two agents
   touching the same repo/worktree concurrently, no dependency ordering. If
   "wholistic multi-agent management" means coordination between agents
   rather than better signal from each one individually, this needs real
   design once items 1-2 are in and their coverage is better understood.

---

## Suggested Priority

Code-side work for items 2, 3, 5, and 6 below is implemented and passing
`cargo test` / `pnpm typecheck`, but **none of it has been validated against
a running instance** — that requires the dev stack to exist first, and a
coding agent cannot provision Komodo stacks, mint Tailscale/Infisical keys,
edit Node B's Caddy config, or push/merge branches on its own. Those are the
manual steps left below.

1. **Done:** the `dev-mydevenv2` stack is stood up — `dev` branches exist in
   both `MyDevEnv2` and `ops`, the Komodo stack and Caddy site block are
   live, `MYDEVENV2_DEV_TOKEN` is minted, and dev's state disk layout is
   fixed (see "Environment Strategy" status above). Still outstanding: the
   Tailscale auth key (optional) and a fully green CI run through
   `komodo-deploy-dev`. Everything below should be validated on `dev` before
   promotion to `main`/prod.
2. Split-pane node-identity fix (`TerminalWorkspace.tsx`) — implemented.
   Validate on dev, then promote.
3. SSE visibility/resume reconnect (`store.ts`) + Capacitor resume hook —
   implemented. Validate on real Android hardware against
   `mydevenv2-dev.sprooty.com` (needs the dev-APK `applicationId` split,
   still to do — see the mobile caveat above), then promote.
4. Real-device native FCM verification — cheaper to iterate against dev than
   prod; do this once the dev APK exists. Needs physical hardware, still to
   do.
5. Push notifications on hang/fail/end (`activity.rs` + `agent_tasks.rs`) —
   implemented.
6. Auto-retry on 429/rate-limit — implemented.
7. Bearer-token risk boundary — independent of the dev/prod split, applies
   primarily to prod. Still a judgment call for the user: whether to move
   live clients onto scoped tokens and whether the primary token keeps full
   Docker-adjacent access long term.
8. Voice conversational interaction (scope after 5-6) — not started; new
   surface, no code exists yet.
9. Cross-agent orchestration (scope after 5-6, separate design pass) — not
   started; needs real design work once 5-6 have run in production for a
   while.
