# Demo site implementation plan

**Status:** implemented; the current GUI and mobile-app showcase share the
same built Solid PWA and deterministic browser transport
**Target base:** `dev`
**Reviewed:** 2026-08-31
**Visual reference:** the PWA served by
[`vogt-dev.sprooty.com`](https://vogt-dev.sprooty.com/), from the matching
`dev` commit

This is an implementation plan, not a second product specification. Built
architecture remains in [`DESIGN.md`](./DESIGN.md), delivery status remains in
[`ROADMAP.md`](./ROADMAP.md), and the issue tracker decides whether this work
is scheduled.

## 1. Outcome

Deliver a safe, resettable demo site that:

- renders the **same Solid PWA artifact** as the `dev` live site rather than a
  screenshot recreation or a second set of demo components;
- gives every shipped route, surface tab, navigation menu, picker and major
  disclosure representative data;
- includes scripted terminal sessions in idle, running, waiting and exited
  states;
- opens directly into representative two-pane and nested three-pane terminal
  layouts, while leaving the existing split, retarget, maximise, broadcast,
  detach and drag-to-split controls usable;
- lets visitors exercise writes against private, per-browser ephemeral state,
  with no forge write, shell execution, shared mutation or secret;
- works at desktop and phone widths and makes its demo status unmistakable;
- can prove which `dev` commit its UI came from and cannot silently drift onto
  a stale bundle.

The primary acceptance statement is:

> For one `dev` commit, build `web/dist/` once. The live-capable image and the
> demo image consume those exact hashed assets. Only the runtime transport and
> data differ; the rendered application components and CSS do not.

The demo artifact also serves `mobile-demo.html`. It is a presentation shell,
not another application: its phone frame loads the same origin's PWA at the
responsive Sessions, terminal and Assistant routes. This matches the actual
Capacitor architecture, where the Android WebView loads the deployed PWA
directly; native-only push and microphone plumbing are stated as exclusions.
For a dedicated mobile hostname, the same static image may select the showcase
as its root document. Its frame names `/index.html` explicitly, so this mode
does not recurse and does not require a second build or image stream.

## 2. Review findings

### 2.1 The UI already has most of the necessary presentation

- `web/src/routes.ts` declares 19 application routes: the six stable places,
  terminal/editor/git/history/tasks/GUI/Assistant tools, setup, settings and
  work-item detail.
- `web/src/App.tsx` owns the desktop Places rail, running-session menus, recent
  places, Files, phone bottom navigation, the phone More sheet, Settings,
  keyboard help and the command palette.
- `web/src/TerminalWorkspace.tsx` and `web/src/terminalLayout.ts` already
  support horizontal and vertical splits, nested split trees, existing-session
  composition, drag-to-split, pane retargeting, maximise/restore, broadcast,
  detach and persistence in `vogt.terminalLayouts.v1`.
- The PWA already has meaningful sub-views that a demo must populate: Backlog
  and Bugs; Audit trail and Notifications; project overview, dependencies,
  drift and import; Git worktree/log/diff; History search/replay; task runs,
  findings and gates; Assistant transcript and pending action; Settings
  sections and saved workspace layouts.

The demo should therefore provide data and transport behavior to the existing
components. It should not fork their markup.

### 2.2 The browser test fixtures are useful prior art, not a deployable demo

`web/tests/browser/gui.spec.ts` already intercepts a broad set of engine and
Vogt requests, and `page.routeWebSocket` proves a terminal can be fed an
ordered `snapshot-start` / binary output / `snapshot-done` sequence without a
PTY. The fixtures are intentionally per-test, incomplete and embedded in a
large test file. Shipping that harness would make the demo dependent on
Playwright and would leave many routes in their empty state.

Extract reusable **fixture builders and protocol transcripts**, not the
Playwright interception layer. Tests and the demo runtime should consume the
same typed scenario catalogue through their own adapters.

### 2.3 A real public PTY is the wrong demo mechanism

The engine's real sessions are shell processes with writable input. Publishing
a shared token or bypassing auth to let anonymous visitors use them would turn
the demo into remote command execution. Running a real core with one shared
database would also let visitors change one another's board, audit and forge
state.

Terminal output must therefore be simulated in the browser, and all mutable
demo data must be isolated per browser. Direct calls to the demo host's real
engine API must remain unauthorized.

### 2.4 Artifact provenance must be part of parity

At review time the deployed development site served the JS/CSS produced by
commit `aad55d0`, while `origin/dev` had advanced. That is normal for a
digest-pinned deployment, but it means “looks like dev” cannot be an informal
claim. The demo must expose its source commit and asset manifest, and the
deployment check must fail when the advertised commit and the served assets do
not agree.

## 3. Architecture decision

### 3.1 One PWA, two runtime transports

Add a narrow transport seam beneath the existing API clients:

```text
Solid surfaces
  -> api.ts / vogtApi.ts / installApi.ts / push.ts
       -> RuntimeTransport
            |- NetworkTransport  -> real HTTP + SSE + WebSocket
            `- DemoTransport     -> per-browser DemoStore + scripted streams
```

`NetworkTransport` preserves today's behavior. `DemoTransport` is selected
only when the origin serves a valid `/demo-manifest.json`. On ordinary Vogt
deployments that file is absent, so production takes the network path without
an operator setting or guessed hostname.

The manifest should contain only non-secret presentation metadata:

```json
{
  "schema": 1,
  "enabled": true,
  "source_ref": "dev",
  "source_sha": "<40 hex characters>",
  "scenario": "full-estate-v1"
}
```

Application boot must resolve this manifest before deciding whether a saved
credential exists. Demo mode installs a non-secret sentinel credential in the
same browser storage key the normal auth gate reads, then loads the demo
transport. A visitor may still choose **Sign out** to inspect the sign-in
surface; reload restores the disposable demo identity.

No component should branch on demo mode to decide its normal content. The only
permitted UI differences are a small persistent **Demo data** disclosure, a
**Reset demo** action, and copy explaining that writes are private and
temporary.

### 3.2 Consolidate every transport path

The seam is incomplete if only `fetchWithRetry` uses it. Inventory and route
all of these through `RuntimeTransport`:

- authenticated JSON and blob requests in `api.ts`;
- registry-backed `/api/vogt` calls in `vogtApi.ts`;
- install/bootstrap, push, voice and Assistant requests;
- the authorized SSE reader used for server events;
- terminal WebSocket attach;
- any remaining component-local `fetch` call.

Use a small socket interface containing only what `Terminal.tsx` consumes
(`send`, `close`, `readyState`, `binaryType`, and event listeners) rather than
pretending a simulator is a browser-native `WebSocket` in every detail.

### 3.3 Per-browser deterministic state

`DemoStore` owns one versioned scenario. Seed it deterministically on first
load and persist it in `sessionStorage` so:

- navigation and reload preserve the visitor's changes;
- a different browser/tab starts clean;
- no state crosses users or reaches a server;
- **Reset demo** replaces the store and relevant Vogt UI storage keys with the
  canonical scenario.

Use an explicit logical clock anchored to the scenario timestamp. Relative
ages should advance during a visit, but snapshots and tests must be able to
freeze the clock. IDs, cursors, hashes and timestamps must be stable.

Writes should implement the real visible semantics where practical: transition
a card, add a comment, archive/snooze/restore Inbox rows, resolve drift, edit a
file, stage/unstage a change, create/rename/kill a simulated session, run a
simulated task, and approve/reject an Assistant action. Every reason field
remains required. Writes append an audit/event record in the demo store so the
Audit view visibly reconciles.

Unsupported or dangerous actions must return a specific demo refusal, never a
fake success. This includes external forge mutation, push delivery, microphone
upload, GUI process launch and any attempt to execute an arbitrary command.

### 3.4 Serve the demo without an engine or core

Build `web/dist/` once from the chosen `dev` SHA. A demo-image stage copies the
same output, verifies the recorded hashes, adds `demo-manifest.json`, the
static simulated GUI page and the mobile-app showcase, then serves them from a
minimal static Node origin. The demo target contains neither the Rust engine
nor the Python core, making the absence of a PTY or shared write path
structural rather than configuration. Run that server with:

- no token, core URL, workspace or state mount;
- a read-only root filesystem and no Linux capabilities;
- Assistant, voice, FCM, GUI process launch and agent-auth integrations absent.

The browser never calls those real APIs in demo mode. A direct caller receives
a static 404 from every `/api/**` or `/mcp` path. An integration smoke attempts
session creation and proves there is no backend route capable of creating a
PTY.

The image may be published by CI, but it must not deploy itself. If the demo is
hosted on the estate, use a separate digest-pinned Komodo stack and an ops
repository change, following the same publish-versus-deploy separation as the
other Vogt stacks.

## 4. Fixture coverage matrix

The fixture is complete only when each row below has an automated visit and a
non-empty assertion.

| Surface | Required representative data and states |
| --- | --- |
| Board | At least two projects, two workflows, all active workflow columns, mixed kinds/priorities/assignees/labels/initiatives, long content, one finished item, a movable card and one refused-transition scenario. |
| Backlog | Both Backlog and Bugs tabs, ranked rows with differing scores/trust/freshness, pagination/continuation, selection, ranking explanation and saved lenses. |
| Inbox | GitHub, drift, CI and agent sources; evidence and proposed change; source URL; work-item link; session link; active/archived/snoozed counts; batch actions and clear-all confirmation. |
| Projects | Populated registry, searchable list, project overview, coverage, compliance, dependencies, observations, drift proposals and forge repository import choices. Use fictional neutral names and paths. |
| Audit | Audit records for demo mutations, notifications from more than one reason/source, filters, continuation and freshness/coverage statements. |
| Sessions overview | Idle, running, waiting-for-input and exited sessions; templates; attention ordering; bookmark/menu actions; a pending exact-payload approval. |
| Terminal | Scripted scrollback, incremental output, resize/ping, input echo and canned command responses; two-pane and nested three-pane presets; create, compose existing, drag-to-split, retarget, broadcast, maximise and detach. |
| Editor/Files | Nested tree, recent files, modified markers, text files in several languages, a long Markdown document, a binary-file refusal, save and stale-write conflict. |
| Git | Repository chooser, branch list, staged/unstaged/untracked entries, recent commits, selected diff, and ephemeral stage/unstage/discard/commit/checkout behavior. |
| History | Several archived sessions, search hits, selected result, metadata, replay tail, truncation and pins. |
| Tasks | Scheduled and event-triggered tasks, running/completed/errored runs, work-item binding, findings, conclusion, checkpoint branches, an open gate and steering transcript. |
| GUI stream | A same-origin static simulated desktop/IDE frame with visible content; launch/kill actions explicitly refuse because no real process exists. |
| Assistant | Canned multi-turn transcript, Markdown/code reply, profiles, a pending terminal-input action and a pending reasoned Vogt write; typed follow-up produces deterministic replies. Speech controls explain that capture/playback is unavailable in the public demo. |
| Work item detail | Body Markdown, comments, audit link, observed evidence, branch/PR story, drift, related items, assignee/state edits, and linked current/exited sessions. |
| Setup | Linked-account state, repository picker, first-project choices and completed sweep/coverage data; writes mutate only the demo store. |
| Settings | Operational status, templates, app/terminal themes, auth profiles, push-subscription example, storage preferences, saved workspace layouts and reset/confirmation flows. |
| Global chrome | Non-zero place counts, waiting attention card, Running/Recent/Files sections, session row menus, phone More sheet, full command palette providers, keyboard-shortcut help, dialogs and feedback notifications. |

### Terminal showcase presets

Seed these stable session IDs and layouts:

1. **Build + tests** — `demo-build` beside `demo-tests` in a 50/50 row split.
   The build streams compilation progress; tests finish with a mixed but
   non-sensitive summary.
2. **Agent review** — `demo-agent` on the left, with `demo-server` above
   `demo-logs` on the right. The agent is waiting for input and demonstrates
   the attention card and exact-payload response controls.
3. **Incident view** — three already-running sessions suitable for
   broadcast/maximise/detach demonstrations.

Preseed `vogt.tabs.v2`, `vogt.terminalLayouts.v1` and saved workspace-layout
records only when the demo store is new. Existing visitor state wins until
they choose Reset. Publish direct links for the overview and each showcase,
and make those links part of the smoke test.

The simulated socket must follow the real attach ordering:

1. `snapshot-start` with byte position;
2. zero or more binary scrollback chunks;
3. `snapshot-done`;
4. timed binary frames and activity events.

It must also accept auth, resize, ping and input frames. Input is parsed only
enough to echo text and select a canned response. It never reaches a shell,
`eval`, subprocess, forge or network request.

## 5. Delivery sequence

### Phase 0 — freeze the parity boundary

1. Record the target `dev` SHA and the deployed reference asset manifest.
2. Add a build step that emits `web/dist/demo-build.json` containing the source
   SHA and hashes of every entry asset.
3. Define the only allowed artifact delta: `demo-manifest.json`, the simulated
   GUI document and their static assets. Hashed PWA JS/CSS/font/worker files
   must be byte-identical between normal and demo images for the same build.
4. Add a visible demo disclosure design that does not alter the primary
   surface grammar or hide first-useful content at phone width.

**Exit:** a test compares both image roots and rejects any unapproved PWA asset
delta.

### Phase 1 — introduce the runtime transport seam

1. Define network-neutral request, event-stream and terminal-socket interfaces.
2. Move existing behavior into `NetworkTransport` without changing request
   URLs, auth handling, retry semantics or error classes.
3. Route every API client and direct transport call through the seam.
4. Run the current unit and browser suites against `NetworkTransport`.
5. Add a guard test that scans for unapproved direct `fetch`, `EventSource` and
   `new WebSocket` usage outside the transport modules.

**Exit:** production behavior and visual snapshots are unchanged; the demo
transport can be installed before `App` mounts.

### Phase 2 — build the typed full-estate fixture

1. Create small typed fixture factories grouped by domain rather than one
   monolithic JSON file.
2. Add the logical clock, deterministic ID/cursor helpers and a state reducer.
3. Cover every engine request used by `api.ts` and every registry route used by
   `vogtApi.ts`.
4. Add Python parity tests that compare the demo's Vogt operation list to the
   real operation registry. A demo responder may omit a deliberately unused
   operation only through a named exclusion with a reason.
5. Reuse fixture factories from Playwright where that reduces duplicate data;
   do not make the shipped demo import test runner code.

**Exit:** a contract test issues every supported request and validates the
response shape used by its surface.

### Phase 3 — add terminal and live-event simulation

1. Implement the scripted socket and attach-protocol transcripts.
2. Seed the three showcase layouts and existing saved-layout picker entries.
3. Publish activity, session lifecycle and `vogt-changed` events through the
   simulated SSE stream so counts and visible views reconcile normally.
4. Implement safe canned terminal input, split-created simulated sessions and
   cleanup behavior.
5. Test replay, incremental output, reconnect, multiple clients to one session,
   nested layout reload and duplicate-session focus.

**Exit:** direct showcase links render their intended split tree, remain usable
after reload, and no test observes a real network WebSocket or process spawn.

### Phase 4 — populate and exercise every surface

1. Implement the coverage matrix above, beginning with global shell providers
   so rail counts and menus are truthful while individual routes land.
2. Add ephemeral mutation reducers and resulting audit/event records.
3. Add the demo disclosure, reset action and a copyable “start here” set of
   links.
4. Test all sub-view controls, row menus, command-palette groups, phone More
   links, Settings sections and destructive confirmations.
5. Ensure no fixture contains maintainer estate names, private paths, tokens,
   real email addresses or tailnet/ops endpoints.

**Exit:** the automated route sweep finds no accidental loading, unavailable,
empty or not-found state in the canonical scenario.

### Phase 5 — visual, accessibility and responsive parity

Run Chromium at minimum at:

- 1440 × 900 desktop;
- 1280 × 900 desktop;
- 768 px breakpoint boundary;
- iPhone 13 viewport/touch profile.

For every route and sub-view:

1. assert the correct active rail/bottom-nav/tool state and document title;
2. assert meaningful content above the first viewport fold;
3. assert no unexpected horizontal document overflow;
4. open every menu/dialog/disclosure and verify focus return plus accessible
   naming;
5. assert no console error, failed request or uncaught rejection;
6. capture the key screenshot set, including both terminal split presets;
7. run selected snapshots under dark and light themes without inventing
   demo-only CSS.

Use screenshots to catch composition regressions, but use asset identity and
shared components as the primary proof of parity. Do not maintain a second set
of screenshots copied from the live site as a design specification.

**Exit:** the demo browser matrix and the existing PWA browser suite pass from
the same commit.

### Phase 6 — package, deploy and verify

1. Add a demo image target/Compose overlay with no workspace, core, integration
   or credential mounts.
2. Extend CI on `dev` to build, scan and sign the demo image after the normal
   web build; tag it by commit, never as a release.
3. Add a smoke that checks the root document, runtime manifest, source SHA,
   asset hashes, canonical routes and direct terminal showcase links.
4. Add a security smoke that calls real session/files/git/GUI/task mutation
   endpoints without the hidden engine token and expects refusal.
5. Deploy by digest through the operator-owned stack, not from CI. Record the
   URL only after DNS/TLS/exposure and retention decisions are made.
6. Add a scheduled drift check: compare the demo's advertised SHA/asset
   manifest with the intended deployed `dev` build and mark the demo stale
   rather than claiming parity when it lags.

**Exit:** a fresh browser can traverse the entire canonical scenario without a
credential, while direct API clients cannot create a process or change shared
state.

## 6. Expected repository changes

Names may change during implementation, but ownership should remain clear:

```text
web/src/runtimeTransport.ts            transport selection and shared interfaces
web/src/networkTransport.ts            today's real HTTP/SSE/WS behavior
web/src/demo/manifest.ts               validated runtime manifest and bootstrap
web/src/demo/fixtures/                 typed deterministic scenario factories
web/src/demo/store.ts                  per-browser state and reducers
web/src/demo/transport.ts              engine + Vogt request responder
web/src/demo/socket.ts                 scripted terminal attach implementation
web/src/demo/gui-stream.html           same-origin simulated GUI content
web/src/demo/mobile-showcase.html      phone frame around the same responsive PWA
web/tests/browser/demo.spec.ts         complete route/menu/split matrix
deploy/demo.overlay.yml                isolated demo runtime
deploy/demo.env.example                non-secret operator choices
scripts/check_demo_assets.py           normal/demo asset identity gate
docs/DEMO_SITE_PLAN.md                 this plan
```

Keep demo response types close to the existing API types. Do not introduce a
second domain model or a handwritten operation that the Python registry does
not know.

## 7. Acceptance checklist

### Parity and provenance

- [x] Implementation is based on `dev` and records the exact source SHA.
- [x] Normal and demo images contain byte-identical hashed PWA assets from one
      build.
- [x] The demo shows a persistent, accessible demo-data disclosure.
- [x] A stale or mismatched asset manifest fails verification and cannot claim
      current parity.

### Complete data

- [x] Every entry in `APP_ROUTES` has a canonical populated scenario.
- [x] Every stable surface sub-view and Sessions tool is populated.
- [x] Desktop rail, session row menus, command palette, keyboard help, Settings
      sections, phone bottom nav and phone More sheet have meaningful entries.
- [x] The route sweep reports no unintended empty/loading/unavailable state.

### Terminals and layouts

- [x] Four terminal activity/lifecycle states are visible.
- [x] Two-pane and nested three-pane layouts have stable direct links.
- [x] Existing split/retarget/maximise/broadcast/detach/drag controls work.
- [x] Socket replay and incremental frames obey the real protocol ordering.
- [x] Terminal input can produce only canned local responses.

### Isolation and safety

- [x] Demo mutations are private to one browser and resettable.
- [x] No real PTY, subprocess, workspace write, forge call, push, voice upload,
      GUI process or external Assistant request can occur.
- [x] No engine/core/integration token is shipped to the browser.
- [x] Direct API mutation attempts reach no backend route.
- [x] Fixtures contain only fictional, non-sensitive identities and paths.

### Quality and operations

- [x] Existing PWA unit, typecheck, build and browser suites remain green.
- [x] Demo desktop/phone route and screenshot matrices are green.
- [x] The demo image is scanned/signed and deployable only by digest through the
      approved deployment path.
- [x] Standard and mobile-first hostnames can pin one digest; the mobile
      overlay changes only the allowlisted root document.
- [x] A reset and a complete canonical tour work in a fresh browser.

## 8. Cut lines

Do not reduce the first release below complete route/sub-view data, the two
split-layout presets, per-browser isolation or provenance checks; those are the
reason for the demo site.

Safe later increments are richer terminal transcripts, additional themes,
more fixture volume, narrated tours and optional animations. A marketing
landing page, analytics, lead capture, public shell access, live forge writes
and a standalone fork of the PWA are explicitly outside this plan.
