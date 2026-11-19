---
title: "Declare graft sources on vogt-dev projects so pod sessions get a minted graft token"
labels: [pods, phase-1, ops, "model:haiku"]
depends_on: [P1.1 deployed to vogt-dev]
vogt: WI-3229
lands: data change through Vogt project_update on vogt-dev (no code), plus the project rule text
---

## Why

Every vogt pod on dev starts without a graft token:

```
WARN session graft token unavailable; the session continues without one session=… reason="the project names no graft source"
session-entrypoint: WARNING: skipping clone of vogt (line rust-2): no graft token is available (the bootstrap response carried no source token)
```

- The per-session mint is built and enabled: `ENGINE_SOURCE_GRAFT_MINT=true`, with a mint token mounted.
  - Mint at pod create: `engine/server/src/sessions.rs:839-850`.
  - Scopes `repo:<r>:read|write|drafts|land:<line>`: `graft_mint.rs:12-16`.
  - TTL is the session budget plus 30 min, capped at 12 h: `graft_mint.rs:45-49,226-232`.
  - Revoke at teardown: `graft_provider.rs:437-460`.
- It is gated on the **project's** declared source (`landing.source`, added in WI-4213 / `2fbef5eb`). Project `vogt` on dev has `"source": null`.
- The entrypoint already writes a minted token to the path the project rule names, `~/.config/graft/sessions/<id>/trial-1.token` (`engine/deploy/session-entrypoint.sh:221-260`).

## What to change

1. After P1.1 is live on dev, `project_update` project `vogt` on vogt-dev so `agent_policy.landing.source` names graft-dev, repo `vogt`, line `rust-2`. Use the exact schema `LandingPolicy.source` takes (`engine/core/src/application/models/entities.rs:442-466`); read it before writing.
2. Do the same for every other project on dev that lands through graft (decision Q8). Check each project's rules for its line (`backlog-1` for most).
3. In project `vogt`'s rule text, change "minted by oversight" to "minted by the engine for this session". Keep the path.

## Done when

1. `project_get vogt` on dev shows a non-null graft `landing.source`. Session starts for vogt succeed (WI-4417 no longer reproduces).
2. A new vogt pod session's engine log has no `graft token unavailable`. Its pod log shows `step clone` with no WARNING, and `g status` inside the pod is clean on `rust-2`.
3. graft-dev's audit log shows `MintToken` by `vogt-engine` for that session, and `RevokeToken` outcome `revoked` after it is stopped.
4. WI-3229 gets a verification comment with this evidence and is closed (its items 2–6 move to P1.11).
