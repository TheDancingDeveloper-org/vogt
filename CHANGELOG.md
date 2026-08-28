# Changelog

All notable changes to Vogt are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
once released — pre-1.0, a minor version bump may still carry a breaking
change, per the compatibility policy in [`opensource.md`](opensource.md).

This file starts from the public 0.2.0 baseline; earlier history lives in the
git log rather than being reconstructed here.

## [Unreleased]

Nothing yet.

## [0.3.0] - 2026-08-28

The first release since the merged core+engine stack reached production. No
operation was renamed or removed; no schema migration is required (the declared
schema stays at 0015, the observed schema at 0004). Pre-1.0, this remains a
minor bump per the compatibility policy in [`opensource.md`](opensource.md).

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

[Unreleased]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/TheDancingDeveloper-org/vogt/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/TheDancingDeveloper-org/vogt/releases/tag/v0.2.0
