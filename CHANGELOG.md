# Changelog

All notable changes to Vogt are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once released — pre-1.0, a minor version bump may still carry a breaking
change.

This file starts from the public 0.2.0 baseline; earlier history lives in the
git log rather than being reconstructed here.

## [Unreleased]

### Added

- Clean-consumer smoke (`scripts/clean_consumer_smoke.sh`, #613): resolves and
  pulls `vogt-stack` and `vogt-voice` over an anonymous registry token, boots
  `deploy/stack.compose.yml` from `stack.env.example`, walks readiness, the PWA,
  the first token, a core read/write and a full voice round trip, then repeats
  with voice disabled and records image digests and provenance to a receipt.
- Worked custom-image example (`deploy/examples/custom-stack/`, #614): a
  Dockerfile deriving `FROM` the published stack digest, an overlay mounting a
  deployment-owned lifecycle hook and pinning a compatible voice image, and a
  new CUSTOMISATION "Extending the stack image" section covering image-owned vs
  persisted vs deployment-owned paths, the shadowing trap, and upgrade/rollback
  limits — the public shape of the private `vogt-dev`/`vogt-prod` derivatives.
- Release compatibility gate (#616): the release publishes `vogt-stack` and
  `vogt-voice` as one pair, fails if their product versions disagree, records
  both digests in `vogt-release-manifest.json`, and the deployment receipt
  schema gains an optional `voice_image_digest`.
- `scripts/check_docs.py` now validates in-page heading anchors, not only file
  links (#615).

### Changed

- Public docs (README, GETTING_STARTED) state the AIO is one product stack with
  a bundled voice sidecar, not one container (#615).

## [0.5.4] - 2026-09-05

A patch on 0.5.3 for the core stall that took the 0.5.3 production deploy
down: one Inbox read had grown to nine seconds, the badges re-asked for it
every eight seconds, and everything else queued behind it. No schema
migration is required.

### Fixed

- **The Inbox no longer re-reads a project once per failing check.** The
  projection rolled every latest check of a project up again *for each*
  failing check in it, so one page cost Σ failing × checks-per-project —
  730 queries and 732,425 observation objects on a two-week-old estate,
  ~9.5 s, and rising with CI activity. Each project is now rolled up once,
  and triage is applied from one batched lookup. The same read takes 0.3 s
  (#580).
- **A read that exceeds its deadline is no longer retried blindly.** The
  PWA's transport treated a timed-out attempt as a wire failure and tried
  twice more, so one badge refresh was three eight-second reads the core
  still ran to completion after the browser had moved on. A deadline is
  terminal; the caller decides (#581).
- **The badges back off after a failed read** — 8 s, doubling to 2 min —
  drop change nudges while waiting, retry once by themselves, and keep the
  last known value on screen as stale rather than replacing it with a dash
  (#581).
- **The front door sheds a burst instead of queueing it.** `/api/vogt/*`
  now carries an in-flight ceiling of 16; past it a caller gets `503` with a
  `Retry-After` rather than a place in a queue the core cannot cancel.
  Requests a client abandons are now logged with how long it waited; they
  used to vanish from the front door's log entirely (#581).
- **`vogt-mcp-remote` answers `initialize` before it looks around.** The
  bridge pre-flighted the banner and its own `tools/list` on 30 s timeouts
  before reading stdin, so a slow core made every agent session report
  "MCP server vogt connection timed out". The client's first message goes
  first; discovery follows once, with a 3 s budget of its own (#582).

### Security

- **The Firebase service-account key can be read from a file.**
  `ENGINE_FCM_SERVICE_ACCOUNT_FILE` (or `fcm_service_account_file`) is the
  documented form. The inline `ENGINE_FCM_SERVICE_ACCOUNT_JSON` still works
  but is removed from the engine's environment as soon as it is read, so the
  sessions the engine starts no longer inherit a private key, and a boot
  warning names the file form until the deployment moves to it (#583).

## [0.5.3] - 2026-09-05

A patch on 0.5.2 for the phone: the app resumes into a live view instead of a
stale "Disconnected", the terminal draws every symbol an agent CLI uses, and
the phone composition gains the room and the exits it lacked. It also lands
the generic, bundled voice sidecar and the on-demand session secret broker
that reached `dev` after 0.5.2. No schema migration is required.

### Fixed

- **The phone no longer sits on "Disconnected" after a warm open.** The
  session store only counted a server *event* as proof of connection, so a
  resumed app whose first session-list refresh met a still-waking network
  showed "Disconnected" until some session changed state — minutes, with a
  long-running command. The stream opening and the engine's 15-second
  keep-alive comments now count as liveness, a failed list is retried once
  the stream is up, and a stream silent for 45 seconds is presumed dead and
  reopened at once (WI-77).
- **The phone terminal renders agent-CLI symbols instead of tofu.** Claude
  Code's "⏵⏵ bypass permissions" footer drew two empty boxes on Android
  because the system monospace font has no U+23F5. xterm now falls back, last
  in its font list, to three bundled glyph subsets of Noto Sans Symbols,
  Symbols 2 and Math (OFL 1.1, `web/public/fonts/`), fetched only when a
  symbol from their ranges is actually drawn.
- **Phone uplift.** The terminal screen fills the viewport (the hidden bottom
  bar no longer reserves its padding); "Go to…" rides inline in each
  surface header instead of taking a row; bottom-bar count badges are no
  longer clipped; the command palette and the new-session preset picker can
  be closed on touch (× / Close / Cancel, backdrop, and the Android back
  button); and a new Files place lets a phone browse the workspace tree and
  upload into a chosen folder (WI-75).

## [0.5.2] - 2026-09-03

A patch on 0.5.1 that hardens the Google Play release lane and fixes an MCP
bridge regression. No schema migration is required.

### Changed

- **The Android release build targets API 36 and shrinks.** The release bundle
  compiles against Android API 36 (a Play requirement) and runs R8 code
  shrinking, obfuscation, and resource shrinking, on top of an Android Gradle
  Plugin 9.0 upgrade — a smaller, Play-ready artifact.

### Fixed

- **The MCP bridge runs directly when a usable token is already present**,
  instead of re-brokering a credential it does not need.
- **The Play release lane builds without the Infisical CLI.** The signed
  bundle's Firebase configuration is fetched over the API at build time, and
  pnpm is set up and pinned in the release-mobile job so it resolves the same
  toolchain as the rest of CI.

## [0.5.1] - 2026-09-03

A patch on 0.5.0: a terminal-startup regression fix, and the Android app moves
to its published identity. No schema migration is required.

### Changed

- **Android applicationId is now `com.thedancingdeveloper.vogt`** (dev builds
  `com.thedancingdeveloper.vogt.dev`), reverse-DNS of the app's own domain and
  the identity the Google Play record binds to. A new applicationId is a new
  app: there is no in-place upgrade from an earlier build — it installs
  alongside, and push re-registers against the new id.

### Added

- **A Google Play release lane.** A `v*` tag builds a signed Android App
  Bundle and, when publishing is armed, delivers it to the Play internal
  testing track — a laptop-free pipeline that signs with an upload key and
  lets Google hold the app-signing key.

### Fixed

- **Terminal sessions start again when automatic agent authentication is on.**
  A session's shell is launched through a credential-brokering helper; the
  0.5.0 security pass had stripped the very credentials that helper needs, so
  every terminal exited immediately. The helper now receives exactly what it
  needs and drops it again before it hands control to the shell, so the shell
  still never inherits the broker identity.

## [0.5.0] - 2026-09-02

The first generic release: the published all-in-one image now works out of
the box for any consumer, and a broad performance pass and the fixes from a
whole-repo security review land across all three halves. No schema
migration is required.

### Added

- **A generic all-in-one stack image.** The published `vogt-stack` release
  family now carries the agent CLIs (`claude`, `codex`) that its shipped
  session templates launch, and carries no estate-specific tooling. The
  estate overlay moved to its own private package with its own build and
  deploy path, so `deploy/stack.compose.yml` as shipped produces a working
  pod for any consumer.

### Changed

- **Promotion is a verified fast-forward push.** GitHub's rebase merge
  rewrites the promoted commits' SHAs, so the previous PR-based promotion
  broke its own fast-forward gate on every release and needed manual repair.
  The promote workflow now keeps all of its gates (confirmation, green
  source checks, verified dev deployment receipt, ancestry) and ends in a
  plain fast-forward push under a ruleset that admits no other update to a
  release branch.
- The public demo image serves its static assets from a slim runtime base
  instead of the build stage.

### Performance

- **Core:** list surfaces for events, audit, and observations gain covering
  indexes; generated endpoints run off the event loop; last-used writes are
  debounced and `auth_decisions` gains a retention cap; empty sweeps skip
  the projection rebuild and `tools/list` is memoized; `has_evidence_tables`
  and per-view workflow lookups are cached.
- **Engine:** scrollback overflow trimming is amortized (~512× fewer
  memmoves); WebSocket snapshot frames stream zero-copy; session detail
  reads support `tail_bytes` so clients stop fetching whole logs; history
  archive reads are bounded and ANSI stripping runs off the hot path.
- **Web:** board items keep identity across refetches and compare by value,
  so unchanged rows stop re-rendering; live-event nudge fan-out is
  coalesced; terminal cache persistence is lengthened and skips unchanged
  frames.

### Fixed

- Browserslist overrides step past the prototype-write advisories in the
  web toolchain.

### Security

Fixes from a whole-repo security review (#510–#524, #539):

- **Core:** MCP `tools/call` refuses `LOCAL_ONLY` operations (#510); a
  remote `root_path` is contained to the import root (#516); forge
  owner/repo values are validated and path/query injection rejected (#517);
  operators can refuse the first-run install bootstrap (#515); the caller's
  raw MCP tool name is no longer logged.
- **Engine:** symlink escapes closed in write, duplicate, and git diff
  (#512); engine secrets are stripped from spawned child processes (#511);
  untrusted-content delimiters are neutralized and steer text capped
  (#520); WebSocket auth is rate-limited and the legacy `?token=` path is
  config-gated (#518); `gui/kill` is contained and session-detail and
  history reads are gated (#519); the push endpoint gains a config-gated
  SSRF check (#524).
- **Web:** server-supplied URLs route through `safeHref` and push
  navigation is same-origin (#522); the stored API base is validated
  against a scheme allow-list (#521).
- **Mobile:** the Android clipboard/voice bridges are gated on the
  top-level origin (#523); a vulnerable transitive `@xmldom/xmldom` is
  overridden (GHSA-6gmq-8vp8-gcm6).
- **CI / supply chain:** signed releases build cold, off the shared (and
  therefore poisonable) build cache (#514); the load-bearing fork-PR
  approval control is documented (#513); grouped low-severity fixes across
  core, engine, CI, and mobile (#524).

## [0.4.0] - 2026-09-01

A feature release that opens session history to agents and tightens the
terminal. Additive: three new read operations join the surface; no operation
was renamed or removed and no schema migration is required.

### Added

- **Session history is searchable by agents, live and archived (#491).** The
  engine's history search gains `include_live` (on by default) — a bounded,
  write-free scan of each running session's scrollback supplements the archived
  full-text index, and hits from a live session are flagged. The log endpoint
  gains `strip_ansi` for readable plain text. vogt-core exposes three new read
  operations on MCP, CLI and REST — `session.search_output`,
  `session.log_tail`, `session.history_list` — so agents and scripts can reach
  history that was previously GUI-only. The History tab now includes running
  sessions in output search and badges live matches.
- **History retention.** A configurable daily sweeper
  (`history_retention_days`, default 30, `0` keeps forever) bounds both the
  history database and the raw session logs.

### Changed

- **Public tree prepared for open source.** Retired historical docs, estate
  references and backward-compatibility framing so the published tree documents
  only its current state.
- **Deployment docs** clarify that release images are CLI-free and the estate
  is operator-private.

### Fixed

- **History replay is readable (#490).** Replays now resolve terminal escape
  sequences and in-place redraws instead of dumping the raw byte stream.
- **The "New session" button no longer renders oversized (#489).**
- **Terminal lag and replay flood under many sessions (#466).** Lagging
  clients are recovered in-band with burst coalescing, and leaked sockets no
  longer duplicate a session's output.
- **Promotion CI (#460).** `promote.yml` uses the GitHub REST API instead of
  the `gh` CLI, so the green-gate validation and PR creation run on the
  self-hosted runner. Superseded CI runs are now cancelled rather than banked.

### Security

- **Voice sidecar dependencies (#459).** Bumped ureq/url to clear the
  rustls-webpki (high) and idna advisories. The affected crates live only in
  the opt-in voice sidecar, not the default release images.

## [0.3.1] - 2026-08-31

A feature and reliability release focused on the terminal, session history, and
the mobile experience. No operation was renamed or removed and no schema
migration is required.

### Added

- **History lists every session, live and exited.** The History tab now unions
  running sessions with the archive, badged and filterable by status, with an
  in-progress replay preview for live sessions.
- **Background pane pre-warm.** After boot, recently-active session panes are
  warmed in the background so a fresh browser opens any of them instantly
  instead of paying a cold full-snapshot load on first click.
- **Mobile app demo site.** A second public demo,
  `vogt-mobile-demo.thedancingdeveloper.com`, showcases the mobile app
  alongside the existing desktop demo.
- **Mobile Sessions and Assistant redesign** — a swipeable terminal pager and
  reworked mobile workflows.

### Changed

- **Session history is actually recorded now.** A graceful-shutdown drain
  archives every live session on redeploy, a provisional row is written at
  spawn and finalized on exit, and a startup backfill indexes orphaned
  transcript logs — so history is populated without a session having to exit
  in-process.
- **Faster cold terminal attach.** A cold attach transfers and replays only a
  bounded tail of scrollback rather than the full ring buffer, cutting the
  first-open latency of long-lived sessions.
- **Assistant improvements** in the engine.

### Fixed

- Launching a session preset no longer pops an unwanted name prompt; presets
  create immediately with the computed name (hold Shift to name one).

## [0.3.0] - 2026-08-28

The first release since the merged core+engine stack reached production. No
operation was renamed or removed; no schema migration is required (the declared
schema stays at 0015, the observed schema at 0004). Pre-1.0, this remains a
minor bump.

### Added

- **Voice assistant, first-party.** A sidecar foundation plus opt-in
  subprocess speech backends and in-process speech inference, moving voice
  from an unproven adoption toward a validated path.
- **Fabro workflow provider** in the engine, with checkpoint-timeline
  collection and workflow gate/steering bridging for agent sessions.
- **Conditional HTTP reads.** Stable read endpoints now emit validators
  (ETag / `If-None-Match`-style) so clients can revalidate cheaply instead of
  refetching.

### Changed

- **Performance — engine.** HTTP responses are compressed; the authentication
  check is split from operational status so health/status probes are cheaper;
  inactive terminals are parked and silent sockets detected.
- **Performance — web/PWA.** Terminal replay work is bounded and Monaco
  languages load on demand; foreground reconciliation and FileTree wake
  refreshes are coordinated through a shared wake coordinator; a typed TTL/SWR
  cache backs stable reads; taxonomy reads and assistant hydration are shared
  and deferred across surfaces; shell "place" metrics are aggregated; a
  repeatable large-estate rendering profile was added.
- **Documentation** reconciled across engine workflow-provider status, the
  voice POC/delivery gates, and public image examples now matching the current
  release.

### Fixed

- **Sessions/terminals.** PTY output is drained before a run concludes;
  session reconciliation stays cancellable; parked terminals reactivate when
  ready; foreground API reads are bounded and cancellable.
- **Caching correctness.** Conditional stable-read validators are honored,
  metadata ordering is preserved through the cache, taxonomy cache request
  order is preserved, and shared taxonomy reads stay authoritative.
- **Workflows.** Duplicate workflow gate answers are rejected; workflow event
  subscriptions retry once; checkpoints are preserved during poll fallback.
- **Mobile.** Native FCM listeners are awaited before registration.
- **Web.** File-tree conflict resolution is retained; demo assistant approvals
  hydrate correctly.

### Security

- Tracked Firebase credentials are guarded, and open-source-readiness code gaps
  were closed ahead of publishing the container publicly.
- The tailscale auth key is kept out of the container environment.

### Internal

- Release/CI: self-contained GitHub Release job; canonical GHCR retention
  policy consumed; CodeQL (JS/TS) and RustSec audit made runnable on the
  self-hosted runners; estate deployment receipts correlated; dev live-smoke
  window and startup tolerances tuned; `pnpm-workspace.yaml` copied into the
  web build stage; Komodo credentials read from GitHub secrets (Infisical
  dropped for the dev deploy).

## [0.2.0] - 2026-08-14

Baseline entry for this changelog. See the git log and release notes for the
full history up to this tag.

[Unreleased]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.5.4...HEAD
[0.5.4]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.5.3...v0.5.4
[0.5.3]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.5.2...v0.5.3
[0.4.0]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/TheDancingDeveloper-org/vogt/releases/tag/v0.2.0
