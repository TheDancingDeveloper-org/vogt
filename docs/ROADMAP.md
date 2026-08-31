# Roadmap

The product described in [`DESIGN.md`](DESIGN.md) is built and shipping: the
Python core, the Rust engine, the Solid PWA, and the Capacitor mobile shell,
released as versioned images from this repository.

The authoritative queue of live work is the GitHub issue tracker. This file
holds only the standing items that are *designed but not built*, or
deliberately deferred — the things an issue alone would not explain. When a
change defers something that was designed, record the deferral here; when a
deferral becomes an issue with a plan, link it and keep the one-line entry
until it ships.

## Designed but not built

- **Forge write-back beyond comments.** The core posts comments and performs
  opt-in write-back for the linked forge, but fuller bidirectional sync
  (historical backfill, forge-derived drift acceptance) is designed and
  deferred. Tracked in the issue tracker as it is picked up.
- **Native Anthropic assistant backend.** The assistant speaks to any
  OpenAI-compatible chat endpoint; a native Anthropic backend is deferred.

## Pending cleanup

- **Legacy `MYDEVENV2_*` names.** The engine still reads `MYDEVENV2_*`
  environment aliases, and a handful of internal identifiers (helper binary
  names, browser-storage keys, an Android notification channel) carry the
  pre-merge name. No released installation depends on them, so removing them
  is ordinary housekeeping — new configuration and examples use `ENGINE_*` /
  `VOGT_*` names only.

## Deliberate deferrals

- **Fork-runnable CI (#207).** Every workflow job names a self-hosted runner
  by policy; a fork cannot run hosted CI as-is. The fork-safe local checks in
  [`CONTRIBUTING.md`](CONTRIBUTING.md) are the supported path until #207
  lands.
- **Device-dependent conformance.** The voice and mobile-push passes that
  need a physical phone and speaker are run per release by the maintainer
  rather than in CI.
