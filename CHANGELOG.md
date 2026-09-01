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
