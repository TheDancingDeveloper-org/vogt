# Changelog

All notable changes to Vogt are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once released — pre-1.0, a minor version bump may still carry a breaking
change.

This file starts from the public 0.2.0 baseline; earlier history lives in the
git log rather than being reconstructed here.

## [Unreleased]

Nothing yet.

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

[Unreleased]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/TheDancingDeveloper-org/vogt/releases/tag/v0.2.0
